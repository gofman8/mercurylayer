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
