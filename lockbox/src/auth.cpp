#include "../include/auth.h"

#include <secp256k1.h>
#include <secp256k1_extrakeys.h>
#include <secp256k1_schnorrsig.h>

#include "../include/tx.h"

namespace auth {

bool verify_bip340(const std::vector<unsigned char>& xonly,
                   const std::vector<unsigned char>& msg32,
                   const std::vector<unsigned char>& sig) {
    // Lengths are checked, never assumed: all three arrive from a request body. A short buffer here
    // would be read past by the C API.
    if (xonly.size() != 32 || msg32.size() != 32 || sig.size() != 64) return false;

    secp256k1_context* ctx = secp256k1_context_create(SECP256K1_CONTEXT_NONE);
    if (ctx == nullptr) return false;

    secp256k1_xonly_pubkey pk;
    if (!secp256k1_xonly_pubkey_parse(ctx, &pk, xonly.data())) {
        secp256k1_context_destroy(ctx);
        return false;
    }

    const int ok = secp256k1_schnorrsig_verify(ctx, sig.data(), msg32.data(), 32, &pk);
    secp256k1_context_destroy(ctx);
    return ok == 1;
}

std::vector<unsigned char> release_message(const std::string& sid,
                                           const std::vector<unsigned char>& nonce32) {
    std::vector<unsigned char> preimage;
    preimage.reserve(sid.size() + nonce32.size());
    preimage.insert(preimage.end(), sid.begin(), sid.end());
    preimage.insert(preimage.end(), nonce32.begin(), nonce32.end());
    return tx::tagged_hash(RELEASE_TAG, preimage);
}

bool verify_release(const std::vector<unsigned char>& xonly,
                    const std::string& sid,
                    const std::vector<unsigned char>& nonce32,
                    const std::vector<unsigned char>& sig) {
    if (nonce32.size() != 32) return false;
    return verify_bip340(xonly, release_message(sid, nonce32), sig);
}

std::optional<std::vector<unsigned char>> derive_aggregate_xonly(
    const std::vector<unsigned char>& client_pubkey33,
    const std::vector<unsigned char>& server_pubkey33) {
    // Lengths checked, never assumed: the client key arrives over the wire.
    if (client_pubkey33.size() != 33 || server_pubkey33.size() != 33) return std::nullopt;

    secp256k1_context* ctx = secp256k1_context_create(SECP256K1_CONTEXT_NONE);
    if (ctx == nullptr) return std::nullopt;
    auto cleanup = [&](std::optional<std::vector<unsigned char>> r) {
        secp256k1_context_destroy(ctx);
        return r;
    };

    secp256k1_pubkey client_pk, server_pk;
    if (!secp256k1_ec_pubkey_parse(ctx, &client_pk, client_pubkey33.data(), 33)) {
        return cleanup(std::nullopt);
    }
    if (!secp256k1_ec_pubkey_parse(ctx, &server_pk, server_pubkey33.data(), 33)) {
        return cleanup(std::nullopt);
    }

    // 1. aggregate = client + server, the same plain EC addition `create_aggregated_address` does.
    const secp256k1_pubkey* parts[2] = {&client_pk, &server_pk};
    secp256k1_pubkey aggregate;
    if (!secp256k1_ec_pubkey_combine(ctx, &aggregate, parts, 2)) return cleanup(std::nullopt);

    secp256k1_xonly_pubkey agg_xonly;
    if (!secp256k1_xonly_pubkey_from_pubkey(ctx, &agg_xonly, nullptr, &aggregate)) {
        return cleanup(std::nullopt);
    }
    std::vector<unsigned char> agg_xonly_bytes(32);
    if (!secp256k1_xonly_pubkey_serialize(ctx, agg_xonly_bytes.data(), &agg_xonly)) {
        return cleanup(std::nullopt);
    }

    // 2. tweak = tagged_hash("TapTweak", xonly(aggregate)) — BIP-341 with NO merkle root. Omitting
    //    this step returns the untweaked aggregate: valid-looking bytes that match nothing a client
    //    sends, so the SE would refuse every honest binding while appearing to work.
    const auto tweak = tx::tagged_hash("TapTweak", agg_xonly_bytes);
    if (tweak.size() != 32) return cleanup(std::nullopt);

    // 3. output = aggregate + tweak*G, taken x-only. `xonly_pubkey_tweak_add` lifts the x-only key
    //    to its even-Y point first, which is exactly BIP-341's `lift_x`.
    secp256k1_pubkey tweaked;
    if (!secp256k1_xonly_pubkey_tweak_add(ctx, &tweaked, &agg_xonly, tweak.data())) {
        return cleanup(std::nullopt);
    }
    secp256k1_xonly_pubkey out_xonly;
    if (!secp256k1_xonly_pubkey_from_pubkey(ctx, &out_xonly, nullptr, &tweaked)) {
        return cleanup(std::nullopt);
    }
    std::vector<unsigned char> out(32);
    if (!secp256k1_xonly_pubkey_serialize(ctx, out.data(), &out_xonly)) {
        return cleanup(std::nullopt);
    }
    return cleanup(out);
}

std::vector<unsigned char> latch_message(const std::string& sid,
                                         const std::vector<unsigned char>& session) {
    std::vector<unsigned char> preimage;
    preimage.reserve(sid.size() + session.size());
    preimage.insert(preimage.end(), sid.begin(), sid.end());
    preimage.insert(preimage.end(), session.begin(), session.end());
    return tx::tagged_hash(LATCH_TAG, preimage);
}

bool verify_latch(const std::vector<unsigned char>& latch_key,
                  const std::string& sid,
                  const std::vector<unsigned char>& session,
                  const std::vector<unsigned char>& sig) {
    // An empty session would make the message depend on the sid alone, turning a per-round
    // authorisation into a bearer token for that coin.
    if (session.empty()) return false;
    return verify_bip340(latch_key, latch_message(sid, session), sig);
}

}  // namespace auth
