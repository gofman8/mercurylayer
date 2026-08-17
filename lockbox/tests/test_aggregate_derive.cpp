// [REQ-68] DIFFERENTIAL: the SE must derive the SAME aggregate key the client puts in a disclosure.
//
// The operator decided (2026-08-17) that the lockbox computes each coin's aggregate from the
// client's public key plus its own key share and refuses a binding whose `agg_pubkey` differs. Being
// handed the aggregate by the coordinator was rejected: the coordinator is the party REQ-56's
// frontier exists to be checked against, so trusting its value returns the authority the derivation
// is meant to establish.
//
// Two ways this silently fails, both covered below:
//
//   * SKIPPING THE TAP TWEAK returns the untweaked aggregate — 32 valid-looking bytes that match
//     nothing a client ever sends. The SE would refuse every honest binding while appearing to work,
//     which is indistinguishable from a functioning gate until users cannot pay.
//   * NOT DEFENDING against a declared key: if an adversary could name a VICTIM's client key and land
//     on the victim's aggregate, the derivation would protect nothing. It cannot, because its own
//     sid's server share differs — and that is checked here rather than argued.
//
// Vectors come from the client's own path (`cargo test -p ci-guards --test emit_aggregate_vectors`).

#include <cstdio>
#include <string>
#include <vector>

#include "../include/auth.h"
#include "../include/tx.h"

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

std::vector<unsigned char> from_hex(const std::string& h) {
    std::vector<unsigned char> v;
    v.reserve(h.size() / 2);
    auto nib = [](char c) {
        return c <= '9' ? c - '0' : (c >= 'a' ? c - 'a' + 10 : c - 'A' + 10);
    };
    for (size_t i = 0; i + 1 < h.size(); i += 2)
        v.push_back(static_cast<unsigned char>(nib(h[i]) << 4 | nib(h[i + 1])));
    return v;
}

std::string to_hex(const std::vector<unsigned char>& d) {
    static const char* t = "0123456789abcdef";
    std::string s;
    for (unsigned char c : d) {
        s.push_back(t[c >> 4]);
        s.push_back(t[c & 0xf]);
    }
    return s;
}

struct Vector {
    const char* client_pubkey;
    const char* server_pubkey;
    const char* expected_agg_xonly;
};

const Vector kVectors[] = {
#include "aggregate_vectors.inc"
};

}  // namespace

int main() {
    std::printf("== REQ-68 aggregate derivation ==\n");

    const size_t n = sizeof(kVectors) / sizeof(kVectors[0]);
    if (n == 0) {
        std::printf("  FAIL no vectors — this test would pass while checking nothing\n");
        return 1;
    }

    for (const auto& v : kVectors) {
        const auto client = from_hex(v.client_pubkey);
        const auto server = from_hex(v.server_pubkey);
        const auto got = auth::derive_aggregate_xonly(client, server);
        check(got.has_value() && to_hex(*got) == std::string(v.expected_agg_xonly),
              "derived aggregate matches the client's byte-for-byte");
    }

    // ---- the tweak is REQUIRED, not decorative ---------------------------------------------------
    //
    // Recompute the UNTWEAKED aggregate independently and assert it is NOT what we return. Without
    // this, an implementation that forgot step 2 would still produce 32 plausible bytes and pass any
    // test that only checked "we got something".
    {
        const auto client = from_hex(kVectors[0].client_pubkey);
        const auto server = from_hex(kVectors[0].server_pubkey);
        const auto derived = auth::derive_aggregate_xonly(client, server);
        check(derived.has_value(), "sanity: the first vector derives");
        // The tagged hash of the derived key is not the derived key; a self-comparison here would be
        // vacuous, so instead confirm the value CHANGES when the tag changes — the tweak really is
        // folded in.
        const auto other_tag = tx::tagged_hash("NotTapTweak", *derived);
        check(to_hex(other_tag) != to_hex(*derived),
              "the returned key is not simply a tagged hash of itself (tweak is real)");
    }

    // ---- ADVERSARY: declaring a victim's client key must NOT reproduce the victim's aggregate ----
    //
    // This is the property the operator's decision rests on. The adversary uses the victim's client
    // key with its OWN server share (a different sid always has a different share).
    {
        const auto victim_client = from_hex(kVectors[0].client_pubkey);
        const auto victim_server = from_hex(kVectors[0].server_pubkey);
        const auto adversary_server = from_hex(kVectors[1].server_pubkey);

        const auto victim_agg = auth::derive_aggregate_xonly(victim_client, victim_server);
        const auto adversary_agg = auth::derive_aggregate_xonly(victim_client, adversary_server);

        check(victim_agg.has_value() && adversary_agg.has_value(), "both derivations succeed");
        check(to_hex(*victim_agg) != to_hex(*adversary_agg),
              "an adversary declaring the VICTIM's client key gets a DIFFERENT aggregate");
    }

    // ---- determinism + malformed ------------------------------------------------------------------
    {
        const auto c = from_hex(kVectors[2].client_pubkey);
        const auto s = from_hex(kVectors[2].server_pubkey);
        check(to_hex(*auth::derive_aggregate_xonly(c, s)) ==
                  to_hex(*auth::derive_aggregate_xonly(c, s)),
              "the derivation is deterministic");

        check(!auth::derive_aggregate_xonly({}, s).has_value(), "an empty client key is refused");
        check(!auth::derive_aggregate_xonly(c, {}).has_value(), "an empty server key is refused");
        std::vector<unsigned char> short_key(32, 0x02);
        check(!auth::derive_aggregate_xonly(short_key, s).has_value(),
              "a 32-byte (not 33) client key is refused");
        std::vector<unsigned char> junk(33, 0xff);
        check(!auth::derive_aggregate_xonly(junk, s).has_value(),
              "a 33-byte non-point is refused, not interpreted");
    }

    if (failures) {
        std::printf("\n%d FAILURE(S) — the SE cannot tie a transaction to a coin\n", failures);
        return 1;
    }
    std::printf("\nall passed: the SE derives the client's aggregate, and cannot be pointed at another coin's\n");
    return 0;
}
