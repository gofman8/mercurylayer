// [REQ-54 R2] /release, end to end against a REAL Postgres and the REAL verifier.
//
// A route that accepts a valid release proves almost nothing: one that accepts EVERYTHING passes
// that test too. What has to hold is the set of refusals, because a release moves a leaf out of what
// REQ-56 forces the collapse to pay on chain. Accepting a forged one silently removes a holder from
// the frontier — and REQ-67 says that once `C` confirms they have no recourse at all.
//
// Not run at build time (no database during `docker build`). Run explicitly:
//     docker exec mercurylayer-lockbox-1 /app/build/test_release_route

#include <pqxx/pqxx>
#include <secp256k1.h>
#include <secp256k1_extrakeys.h>
#include <secp256k1_schnorrsig.h>

#include <cstdio>
#include <cstring>
#include <string>
#include <vector>

#include "../include/auth.h"
#include "../include/db_manager.h"

namespace db_manager {
std::string getDatabaseConnectionString();
}

namespace {

int failures = 0;

void check(bool cond, const char* what) {
    if (cond) {
        std::printf("  ok   %s\n", what);
    } else {
        std::printf("  FAIL %s\n", what);
        ++failures;
    }
}

/// A keypair standing in for a leaf's owner. Returns the x-only key and keeps the keypair so the
/// test can sign as that owner — and, for the negative cases, as somebody else.
struct Owner {
    secp256k1_keypair kp;
    std::vector<unsigned char> xonly;
};

Owner make_owner(secp256k1_context* ctx, unsigned char seed) {
    Owner o;
    std::vector<unsigned char> sk(32, seed);
    if (!secp256k1_keypair_create(ctx, &o.kp, sk.data())) std::abort();
    secp256k1_xonly_pubkey xo;
    if (!secp256k1_keypair_xonly_pub(ctx, &xo, nullptr, &o.kp)) std::abort();
    o.xonly.resize(32);
    secp256k1_xonly_pubkey_serialize(ctx, o.xonly.data(), &xo);
    return o;
}

std::vector<unsigned char> sign(secp256k1_context* ctx, const Owner& o,
                                const std::vector<unsigned char>& msg32) {
    std::vector<unsigned char> sig(64);
    if (!secp256k1_schnorrsig_sign32(ctx, sig.data(), msg32.data(), &o.kp, nullptr)) std::abort();
    return sig;
}

}  // namespace

int main() {
    std::printf("== /release (live Postgres + real BIP-340) ==\n");

    std::string err;
    if (!db_manager::ensure_schema(err)) {
        std::printf("  FAIL ensure_schema: %s\n", err.c_str());
        return 1;
    }

    secp256k1_context* ctx = secp256k1_context_create(SECP256K1_CONTEXT_NONE);
    const Owner owner = make_owner(ctx, 0x41);
    const Owner impostor = make_owner(ctx, 0x77);

    const std::string sid = "testrel_0001";
    const std::string unlatched = "testrel_UNLATCHED";

    auto purge = [&] {
        try {
            pqxx::connection conn(db_manager::getDatabaseConnectionString());
            pqxx::work txn(conn);
            txn.exec_params("DELETE FROM se_released WHERE statechain_id LIKE $1;", "testrel_%");
            txn.exec_params("DELETE FROM se_release_nonce WHERE statechain_id LIKE $1;", "testrel_%");
            txn.exec_params("DELETE FROM se_latch WHERE statechain_id LIKE $1;", "testrel_%");
            txn.commit();
        } catch (std::exception const& e) {
            std::printf("  (purge failed: %s)\n", e.what());
        }
    };
    purge();

    bool armed = false;
    if (!db_manager::arm_latch(sid, owner.xonly, armed, err) || !armed) {
        std::printf("  FAIL could not arm the latch: %s\n", err.c_str());
        return 1;
    }
    std::printf("  ok   latch armed to the owner's key\n");

    // The route's logic, exercised through the same functions the handler calls. (The HTTP layer is
    // covered by the live E2E; what matters here is the decision procedure and its refusals.)
    auto try_release = [&](const std::string& target, const Owner& signer,
                           const std::vector<unsigned char>& nonce,
                           const std::string& signed_over_sid) -> bool {
        std::vector<unsigned char> latch;
        bool has = false;
        std::string e;
        if (!db_manager::get_latch(target, latch, has, e) || !has) return false;  // fail closed
        const auto msg = auth::release_message(signed_over_sid, nonce);
        const auto sig = sign(ctx, signer, msg);
        if (!auth::verify_release(latch, target, nonce, sig)) return false;
        if (!db_manager::consume_release_nonce(target, nonce, e)) return false;  // replay
        return db_manager::record_release(target, e);
    };

    const std::vector<unsigned char> n1(32, 0x01);
    const std::vector<unsigned char> n2(32, 0x02);

    // ---- the refusals, first — they are the point --------------------------------------------
    check(!try_release(unlatched, owner, n1, unlatched),
          "a coin with NO latch cannot be released (fails CLOSED)");

    check(!try_release(sid, impostor, n1, sid),
          "a signature by SOMEONE ELSE'S key is refused");

    // Signed over a DIFFERENT sid: the tagged message binds the sid, so a release consented to for
    // one leaf must not discharge another.
    check(!try_release(sid, owner, n1, "some_other_sid"),
          "a signature over a DIFFERENT sid is refused (the tag binds it)");

    // ---- the honest path ----------------------------------------------------------------------
    check(try_release(sid, owner, n1, sid), "the owner's own signature is accepted");

    bool released = false;
    check(db_manager::is_released(sid, released, err) && released,
          "the leaf reads back as released");

    // ---- replay --------------------------------------------------------------------------------
    check(!try_release(sid, owner, n1, sid),
          "REPLAYING the same nonce is refused (single-use is a PRIMARY KEY, not a check)");

    // A fresh nonce from the same owner is fine — releasing twice is consenting twice.
    check(try_release(sid, owner, n2, sid), "a FRESH nonce from the owner is accepted again");

    // ---- released is monotone ------------------------------------------------------------------
    check(db_manager::is_released(sid, released, err) && released,
          "released stays true — there is no path that clears it");

    bool other = true;  // deliberately wrong, to catch a path that fails to assign
    check(db_manager::is_released("testrel_NEVER", other, err) && !other,
          "an unknown sid reads as NOT released, and the flag is assigned on that path");

    purge();
    secp256k1_context_destroy(ctx);

    if (failures) {
        std::printf("\n%d FAILURE(S) — a forged release could discharge a leaf\n", failures);
        return 1;
    }
    std::printf("\nall passed: only the owner's own fresh signature releases a leaf\n");
    return 0;
}
