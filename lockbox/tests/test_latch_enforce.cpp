// [REQ-61] LATCH ENFORCEMENT — the decision procedure, both directions.
//
// Enforcement is only real if BOTH hold: an unauthorised co-signature is refused, AND an authorised
// one is served. A gate that refuses everything looks identical to a working one in any test that
// only tries forgeries — and it would brick every latched coin, which is worse than no gate at all
// because the failure is silent until a user cannot spend.
//
// The message binds the SESSION, not just the sid. That is the difference between an authorisation
// and a bearer token: without it, one captured signature would authorise every future co-signature
// on that coin forever. The replay case below is the one that proves it.
//
// Run: docker exec mercurylayer-lockbox-1 /app/build/test_latch_enforce

#include <secp256k1.h>
#include <secp256k1_extrakeys.h>
#include <secp256k1_schnorrsig.h>

#include <cstdio>
#include <string>
#include <vector>

#include "../include/auth.h"

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
    std::printf("== REQ-61 latch enforcement ==\n");

    secp256k1_context* ctx = secp256k1_context_create(SECP256K1_CONTEXT_NONE);
    const Owner owner = make_owner(ctx, 0x51);
    const Owner impostor = make_owner(ctx, 0x99);

    const std::string sid = "latched_coin_0001";
    // Two DIFFERENT signing rounds. Real sessions are 133 bytes; the values differ so a signature
    // for one must not satisfy the other.
    std::vector<unsigned char> session_a(133, 0x11);
    std::vector<unsigned char> session_b(133, 0x22);

    // ---- the honest direction — a gate that refuses this bricks every latched coin ------------
    {
        const auto msg = auth::latch_message(sid, session_a);
        const auto sig = sign(ctx, owner, msg);
        check(auth::verify_latch(owner.xonly, sid, session_a, sig),
              "the OWNER's authorisation for THIS round is accepted");
    }

    // ---- REPLAY: the property the session-in-message exists for --------------------------------
    {
        const auto msg_a = auth::latch_message(sid, session_a);
        const auto sig_a = sign(ctx, owner, msg_a);
        check(!auth::verify_latch(owner.xonly, sid, session_b, sig_a),
              "an authorisation for round A does NOT authorise round B (not a bearer token)");
    }

    // ---- wrong key ------------------------------------------------------------------------------
    {
        const auto msg = auth::latch_message(sid, session_a);
        const auto sig = sign(ctx, impostor, msg);
        check(!auth::verify_latch(owner.xonly, sid, session_a, sig),
              "a signature by SOMEONE ELSE'S key is refused");
    }

    // ---- wrong sid ------------------------------------------------------------------------------
    {
        const auto msg = auth::latch_message("a_different_coin", session_a);
        const auto sig = sign(ctx, owner, msg);
        check(!auth::verify_latch(owner.xonly, sid, session_a, sig),
              "an authorisation naming a DIFFERENT coin is refused");
    }

    // ---- cross-protocol: a RELEASE signature must not authorise a co-signature -----------------
    //
    // Both are BIP-340 by the same owner key. Only the domain tag separates them, which is exactly
    // what tags are for — and a wrong tag here would let a consent record double as spending
    // authority.
    {
        std::vector<unsigned char> nonce(32, 0x33);
        const auto release_msg = auth::release_message(sid, nonce);
        const auto release_sig = sign(ctx, owner, release_msg);
        check(!auth::verify_latch(owner.xonly, sid, session_a, release_sig),
              "a RELEASE signature does not authorise a co-signature (the tags separate them)");

        const auto latch_msg = auth::latch_message(sid, session_a);
        const auto latch_sig = sign(ctx, owner, latch_msg);
        check(!auth::verify_release(owner.xonly, sid, nonce, latch_sig),
              "...and a LATCH signature does not release a leaf, either");
    }

    // ---- malformed --------------------------------------------------------------------------
    {
        const auto msg = auth::latch_message(sid, session_a);
        auto sig = sign(ctx, owner, msg);
        sig[63] ^= 0x01;
        check(!auth::verify_latch(owner.xonly, sid, session_a, sig),
              "a one-bit-corrupted authorisation is refused");

        check(!auth::verify_latch(owner.xonly, sid, {}, sign(ctx, owner, msg)),
              "an EMPTY session is refused — it would make the message depend on the sid alone");
    }

    secp256k1_context_destroy(ctx);

    if (failures) {
        std::printf("\n%d FAILURE(S) — latch enforcement is not trustworthy\n", failures);
        return 1;
    }
    std::printf("\nall passed: only the owner, only for this round, and only as a co-signature\n");
    return 0;
}
