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
    ///
    /// ⚠️ **[D69] SUPERSEDED for the shipping route — kept for the per-coin attestation only.**
    /// Signing with THIS COIN's keypair means the verifier must know that coin's server key, whose
    /// only honest anchor is the on-chain funding output. A deep in-ladder-split ancestor's funding
    /// is deliberately un-broadcast, so no such anchor exists and the identity of the attester rested
    /// on the coordinator's word (TRUST-MODEL B11). `/signature_count` now uses
    /// [`attest_sig_count_identity`].
    SigCountAttestationResponse attest_sig_count(
        unsigned char* seed,
        utils::chacha20_poly1305_encrypted_data *encrypted_keypair,
        const unsigned char* message32);

    /// **[D69] The enclave's LONG-TERM ATTESTATION IDENTITY, derived from the seed.**
    ///
    /// One key for the whole enclave, independent of any coin, so a verifier can pin it once and
    /// check every attestation against it — including attestations about ancestors whose funding
    /// output was never broadcast. That is what closes B11: the attester's identity stops depending
    /// on whether the coin it is talking about is on chain.
    ///
    /// **This is not a new secret.** Every per-coin keypair is already sealed under this same seed,
    /// so the seed is already the single point of compromise; the identity key is one more
    /// domain-separated derivation from it and concentrates nothing that was not already
    /// concentrated.
    ///
    /// Derivation: `SHA256("utexo/attestation-identity/v1" || seed || u8(counter))`, counter from 0,
    /// retried until the result is a valid secp256k1 scalar. Domain separation is what stops this
    /// key colliding with any other use of the same seed.
    void attestation_identity_pubkey(unsigned char* seed, unsigned char* xonly_pubkey_out32);

    /// [D69] Sign `message32` with the identity key from [`attestation_identity_pubkey`].
    SigCountAttestationResponse attest_sig_count_identity(
        unsigned char* seed,
        const unsigned char* message32);
    

} // namespace enclave

#endif // ENCLAVE_H