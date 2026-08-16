// DIFFERENTIAL: the SE's BIP-340 verification must accept exactly what the client signs.
//
// The SE had never verified a signature before this — `secp256k1_schnorrsig_verify` had zero hits
// anywhere under lockbox/. Two failure modes matter here, and they look identical from inside:
//
//   * a verifier that ACCEPTS TOO MUCH is indistinguishable from a working one until someone forges;
//   * a verifier that computes the WRONG TAGGED MESSAGE refuses every honest request, disagreeing
//     with every client identically and consistently — which reads as "the control is working".
//
// So the positive cases below are not enough on their own, and each is paired with a control that
// must FAIL. The vectors come from the client's own signing path
// (`cargo test -p ci-guards --test emit_bip340_vectors`), whose tagged hash is itself pinned against
// libsecp256k1's internal BIP-340 challenge — so a shared misreading of the standard cannot hide
// here either.

#include <cstdio>
#include <cstring>
#include <string>
#include <vector>

#include "../include/auth.h"
#include "../include/tx.h"

namespace {

int failures = 0;

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
    s.reserve(d.size() * 2);
    for (unsigned char c : d) {
        s.push_back(t[c >> 4]);
        s.push_back(t[c & 0xf]);
    }
    return s;
}

void check(bool cond, const char* what) {
    if (cond) {
        std::printf("  ok   %s\n", what);
    } else {
        std::printf("  FAIL %s\n", what);
        ++failures;
    }
}

struct Vector {
    const char* xonly;
    const char* sid;
    const char* nonce32;
    const char* msg32;
    const char* sig64;
};

const Vector kVectors[] = {
#include "bip340_vectors.inc"
};

}  // namespace

int main() {
    std::printf("== lockbox BIP-340 verification ==\n");

    const size_t n = sizeof(kVectors) / sizeof(kVectors[0]);
    if (n == 0) {
        std::printf("  FAIL no vectors — this test would pass while checking nothing\n");
        return 1;
    }

    for (const auto& v : kVectors) {
        const auto xonly = from_hex(v.xonly);
        const auto nonce = from_hex(v.nonce32);
        const auto want_msg = from_hex(v.msg32);
        const auto sig = from_hex(v.sig64);
        const std::string sid = v.sid;

        // 1. The tagged message must match the client's byte for byte. If this drifts, every
        //    signature is checked against a message no client ever signed.
        const auto msg = auth::release_message(sid, nonce);
        check(msg == want_msg, "release_message matches the client's tagged message");

        // 2. The honest signature verifies.
        check(auth::verify_release(xonly, sid, nonce, sig), "an honest release signature verifies");

        // 3. CONTROL — a different sid must NOT verify. This is the replay case: a release consented
        //    to for one leaf must not discharge another.
        check(!auth::verify_release(xonly, sid + "00", nonce, sig),
              "a signature for a DIFFERENT sid is refused");

        // 4. CONTROL — a different nonce must NOT verify (single-use is meaningless otherwise).
        auto other_nonce = nonce;
        other_nonce[0] ^= 0x01;
        check(!auth::verify_release(xonly, sid, other_nonce, sig),
              "a signature for a DIFFERENT nonce is refused");

        // 5. CONTROL — one flipped signature bit must NOT verify.
        auto bad_sig = sig;
        bad_sig[63] ^= 0x01;
        check(!auth::verify_release(xonly, sid, nonce, bad_sig),
              "a one-bit-corrupted signature is refused");

        // 6. CONTROL — a different key must NOT verify. Without this, a verifier that ignores the
        //    pubkey entirely passes every case above.
        auto other_key = xonly;
        other_key[0] ^= 0x01;
        check(!auth::verify_release(other_key, sid, nonce, sig),
              "a signature under a DIFFERENT key is refused");
    }

    // 7. Malformed input is refused rather than read past — these arrive from a request body.
    check(!auth::verify_bip340({}, std::vector<unsigned char>(32), std::vector<unsigned char>(64)),
          "an empty pubkey is refused, not dereferenced");
    check(!auth::verify_bip340(std::vector<unsigned char>(32), std::vector<unsigned char>(31),
                               std::vector<unsigned char>(64)),
          "a 31-byte message is refused");
    check(!auth::verify_bip340(std::vector<unsigned char>(32), std::vector<unsigned char>(32),
                               std::vector<unsigned char>(63)),
          "a 63-byte signature is refused");

    // 8. The tag must actually bind: the same preimage under a different tag is a different message.
    const auto a = auth::release_message("abcd", std::vector<unsigned char>(32, 0x11));
    const auto b = tx::tagged_hash("utexo/leaf_release/v2", [] {
        std::vector<unsigned char> p{'a', 'b', 'c', 'd'};
        p.insert(p.end(), 32, 0x11);
        return p;
    }());
    check(a != b, "changing the domain tag changes the message");

    if (failures) {
        std::printf("\n%d FAILURE(S) — the SE's BIP-340 verification is not trustworthy\n", failures);
        return 1;
    }
    std::printf("\nall passed: the SE verifies exactly what the client signs, and nothing else\n");
    return 0;
}
