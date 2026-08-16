// DIFFERENTIAL: the SE's session reconstruction must equal the client's, byte for byte.
//
// Witness binding works by rebuilding the signing session from a disclosed transaction and comparing
// it against the 133 bytes the client sent. That comparison is only meaningful if both sides build
// the SAME object from the SAME inputs — and a wrong reconstruction is INVISIBLE from inside the SE,
// because it would disagree with every client identically and consistently. The SE would refuse
// every honest co-signature while looking like a working security control.
//
// So the expectations here are produced by the client's own call chain (the one
// `calculate_musig_session` uses), not by this file:
//
//     cargo +stable test -p ci-guards --test emit_session_vectors -- --nocapture
//
// Each vector supplies exactly what a disclosure carries — output key, aggregate nonce, sighash,
// blinding factor, output tweak — and the session the client derived from them.

#include <secp256k1.h>
#include <secp256k1_musig.h>

#include <cstdio>
#include <cstring>
#include <string>
#include <vector>

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

std::string to_hex(const unsigned char* d, size_t n) {
    static const char* t = "0123456789abcdef";
    std::string s;
    s.reserve(n * 2);
    for (size_t i = 0; i < n; ++i) {
        s.push_back(t[d[i] >> 4]);
        s.push_back(t[d[i] & 0xf]);
    }
    return s;
}

struct Vector {
    const char* agg_pubkey;
    const char* agg_nonce;
    const char* sighash;
    const char* blinding_factor;
    const char* out_tweak;
    const char* session;
};

const Vector kVectors[] = {
#include "session_vectors.inc"
};

}  // namespace

int main() {
    std::printf("== lockbox blinded-session reconstruction ==\n");

    const size_t n = sizeof(kVectors) / sizeof(kVectors[0]);
    if (n == 0) {
        std::printf("  FAIL no vectors — this test would pass while checking nothing\n");
        return 1;
    }

    secp256k1_context* ctx = secp256k1_context_create(SECP256K1_CONTEXT_NONE);

    for (const auto& v : kVectors) {
        const auto pk_bytes = from_hex(v.agg_pubkey);
        const auto nonce_bytes = from_hex(v.agg_nonce);
        const auto sighash = from_hex(v.sighash);
        auto blinding = from_hex(v.blinding_factor);
        const auto tweak = from_hex(v.out_tweak);
        const auto want = from_hex(v.session);

        // The route accepts exactly 133 bytes and the SE compares the whole session object against
        // them. If the client's serialisation is a different length, the compare in `witness::bind`
        // is reading past or short of the object and cannot be correct.
        if (want.size() != 133) {
            std::printf("  FAIL session vector is %zu bytes, not 133\n", want.size());
            ++failures;
            continue;
        }

        secp256k1_pubkey agg_pk;
        if (!secp256k1_ec_pubkey_parse(ctx, &agg_pk, pk_bytes.data(), pk_bytes.size())) {
            std::printf("  FAIL agg_pubkey did not parse\n");
            ++failures;
            continue;
        }

        secp256k1_musig_aggnonce agg_nonce;
        if (!secp256k1_musig_aggnonce_parse(ctx, &agg_nonce, nonce_bytes.data())) {
            std::printf("  FAIL agg_nonce did not parse\n");
            ++failures;
            continue;
        }

        secp256k1_musig_session session;
        if (!secp256k1_blinded_musig_nonce_process_without_keyaggcoeff(
                ctx, &session, &agg_nonce, sighash.data(), &agg_pk, nullptr, blinding.data(),
                tweak.data())) {
            std::printf("  FAIL nonce_process refused the inputs\n");
            ++failures;
            continue;
        }

        const auto* got = reinterpret_cast<const unsigned char*>(&session);
        if (std::memcmp(got, want.data(), 133) == 0) {
            std::printf("  ok   session matches the client byte-for-byte\n");
        } else {
            std::printf("  FAIL session mismatch\n         got  %s\n         want %s\n",
                        to_hex(got, 133).c_str(), v.session);
            ++failures;
        }
    }

    secp256k1_context_destroy(ctx);

    if (failures) {
        std::printf("\n%d FAILURE(S) — witness binding would refuse every honest request\n", failures);
        return 1;
    }
    std::printf("\nall passed: the SE reconstructs the client's session exactly\n");
    return 0;
}
