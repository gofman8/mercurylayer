#ifndef ENCLAVE_H
#define ENCLAVE_H

#include "utils.h"

namespace enclave {

    struct NewKeyPairResponse {
        unsigned char server_pubkey[33];
        utils::chacha20_poly1305_encrypted_data encrypted_data;
    };

    struct NewNonceResponse {
        unsigned char server_pubnonce[66];
        utils::chacha20_poly1305_encrypted_data encrypted_secnonce;
    };

    struct PatialSignatureResponse {
        unsigned char partial_sig_data[32];
    };

    /// [D8] A BIP-340 Schnorr attestation over (statechain_id, sig_count), signed by THIS coin's
    /// server keypair — the same key whose x-only public half the receiver already binds to the
    /// on-chain tx0 output (`validate_tx0_output_pubkey`). No new key material and no new trust
    /// anchor: the verifier is a value the client has already checked against the chain.
    struct SigCountAttestationResponse {
        unsigned char signature[64];
        unsigned char xonly_pubkey[32];
    };

    NewKeyPairResponse generate_new_keypair(unsigned char* seed);
    NewNonceResponse generate_nonce(unsigned char* seed, utils::chacha20_poly1305_encrypted_data *encrypted_keypair);
    PatialSignatureResponse partial_signature(
        unsigned char* seed, 
        utils::chacha20_poly1305_encrypted_data *encrypted_keypair, 
        utils::chacha20_poly1305_encrypted_data *encrypted_secnonce,
        int negate_seckey,
        unsigned char* session_data, 
        size_t session_data_size,
        unsigned char* serialized_server_pubnonce);
    NewKeyPairResponse key_update(
        unsigned char* seed,
        utils::chacha20_poly1305_encrypted_data *old_encrypted_keypair,
        unsigned char* serialized_x1,
        unsigned char* serialized_t2);
    /// [D8] Attest `sig_count` for `statechain_id`. `message` is the caller-built 32-byte digest
    /// (see `server.cpp`'s route) so the exact preimage is defined in one place and cannot drift
    /// between signer and verifier.
    SigCountAttestationResponse attest_sig_count(
        unsigned char* seed,
        utils::chacha20_poly1305_encrypted_data *encrypted_keypair,
        const unsigned char* message32);
    

} // namespace enclave

#endif // ENCLAVE_H