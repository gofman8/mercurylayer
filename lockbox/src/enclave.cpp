#include <assert.h>
#include "enclave.h"
#include <openssl/rand.h>
#include <openssl/sha.h>
#include <vector>
#include <string>
#include <stdexcept>
#include <string.h>
#include "utils.h"
#include <iostream>
#include "secp256k1.h"
#include "secp256k1_schnorrsig.h"
#include "secp256k1_musig.h"
#include "monocypher.h"

void encrypt_data(
    utils::chacha20_poly1305_encrypted_data *encrypted_data,
    unsigned char* seed, 
    uint8_t* raw_data, size_t raw_data_size)
{
    // Associated data (optional, can be NULL if not used)
    uint8_t *ad = NULL;
    size_t ad_size = 0;

    if (RAND_bytes(encrypted_data->nonce, sizeof(encrypted_data->nonce)) != 1) {
        throw std::runtime_error("Failed to generate random bytes");
    }

    encrypted_data->data_len = raw_data_size;
    crypto_aead_lock(encrypted_data->data, encrypted_data->mac, seed, encrypted_data->nonce, ad, ad_size, raw_data, raw_data_size);
}

int decrypt_data(
    utils::chacha20_poly1305_encrypted_data *encrypted_data,
    unsigned char* seed, 
    uint8_t* decrypted_data, size_t decrypted_data_size)
{
    // Associated data (optional, can be NULL if not used)
    uint8_t *ad = NULL;
    size_t ad_size = 0;
    
    int status = crypto_aead_unlock(decrypted_data, encrypted_data->mac, seed, encrypted_data->nonce, ad, ad_size, encrypted_data->data, encrypted_data->data_len);
    return status;
}
namespace enclave {

    NewKeyPairResponse generate_new_keypair(
        unsigned char* seed
    ) {

        secp256k1_context* ctx = secp256k1_context_create(SECP256K1_CONTEXT_NONE);

        NewKeyPairResponse response;
        memset(response.server_pubkey, 0, 33);
        utils::initialize_encrypted_data(response.encrypted_data, sizeof(secp256k1_keypair));

        unsigned char server_privkey[32];
        memset(server_privkey, 0, 32);

        do {
            if (RAND_bytes(server_privkey, sizeof(server_privkey)) != 1) {
                throw std::runtime_error("Failed to generate random bytes");
            }
        } while (!secp256k1_ec_seckey_verify(ctx, server_privkey));

        secp256k1_keypair server_keypair;

        int return_val = secp256k1_keypair_create(ctx, &server_keypair, server_privkey);
        assert(return_val);

        secp256k1_pubkey server_pubkey;
        return_val = secp256k1_keypair_pub(ctx, &server_pubkey, &server_keypair);
        assert(return_val);

        size_t len = sizeof(response.server_pubkey);
        return_val = secp256k1_ec_pubkey_serialize(ctx, response.server_pubkey, &len, &server_pubkey, SECP256K1_EC_COMPRESSED);
        assert(return_val);
        // Must be the same size as the size of the output, because we passed a 33 byte array.
        assert(len == sizeof(response.server_pubkey));
        // --- remove

        encrypt_data(&response.encrypted_data, seed, server_keypair.data, sizeof(secp256k1_keypair::data));

        secp256k1_context_destroy(ctx);

        return response;
        
    }

    NewNonceResponse generate_nonce(unsigned char* seed, utils::chacha20_poly1305_encrypted_data *encrypted_keypair) {

        NewNonceResponse response;

        memset(response.server_pubnonce, 0, sizeof(response.server_pubnonce));
        utils::initialize_encrypted_data(response.encrypted_secnonce, sizeof(secp256k1_musig_secnonce));

        secp256k1_keypair server_keypair;
        memset(server_keypair.data, 0, sizeof(server_keypair.data));

        int status = decrypt_data(encrypted_keypair, seed, server_keypair.data, sizeof(server_keypair.data));
        if (status != 0) {
            throw std::runtime_error("\nSeed ecryption failed");
        }

        secp256k1_context* ctx = secp256k1_context_create(SECP256K1_CONTEXT_VERIFY);

        // step 1 - Extract server secret and public keys from keypair

        unsigned char server_seckey[32];
        int return_val = secp256k1_keypair_sec(ctx, server_seckey, &server_keypair);
        assert(return_val);

        secp256k1_pubkey server_pubkey;
        return_val = secp256k1_keypair_pub(ctx, &server_pubkey, &server_keypair);
        assert(return_val);

        // step 2 - Generate secret and public nonce

        unsigned char session_id[32];
        memset(session_id, 0, 32);
        if (RAND_bytes(session_id, sizeof(session_id)) != 1) {
            throw std::runtime_error("Failed to generate random bytes");
        }

        secp256k1_musig_pubnonce server_pubnonce;
        secp256k1_musig_secnonce server_secnonce;

        return_val = secp256k1_musig_nonce_gen(ctx, &server_secnonce, &server_pubnonce, session_id, server_seckey, &server_pubkey, NULL, NULL, NULL);
        assert(return_val);

        // step 3 - Encrypt secret nonce
        encrypt_data(&response.encrypted_secnonce, seed, server_secnonce.data, sizeof(secp256k1_musig_secnonce::data));

        return_val = secp256k1_musig_pubnonce_serialize(ctx, response.server_pubnonce, &server_pubnonce);
        assert(return_val);

        secp256k1_context_destroy(ctx);

        return response;
    }

    PatialSignatureResponse partial_signature(
        unsigned char* seed, 
        utils::chacha20_poly1305_encrypted_data *encrypted_keypair, 
        utils::chacha20_poly1305_encrypted_data *encrypted_secnonce,
        int negate_seckey,
        unsigned char* session_data, 
        size_t session_data_size,
        unsigned char* serialized_server_pubnonce) {
            
            PatialSignatureResponse response;
            memset(response.partial_sig_data, 0, sizeof(response.partial_sig_data));

            // step 0 - Decrypt encrypted_keypair

            secp256k1_keypair server_keypair;
            memset(server_keypair.data, 0, sizeof(server_keypair.data));

            int status = decrypt_data(encrypted_keypair, seed, server_keypair.data, sizeof(server_keypair.data));
            if (status != 0) {
                throw std::runtime_error("\nSeed ecryption failed");
            }

            secp256k1_context* ctx = secp256k1_context_create(SECP256K1_CONTEXT_VERIFY);

            // step 1 - Extract server secret and public keys from keypair

            unsigned char server_seckey[32];
            int return_val = secp256k1_keypair_sec(ctx, server_seckey, &server_keypair);
            assert(return_val);

            secp256k1_pubkey server_pubkey;
            return_val = secp256k1_keypair_pub(ctx, &server_pubkey, &server_keypair);
            assert(return_val);

            // step 2 - Decrypt encrypted_secnonce

            secp256k1_musig_secnonce server_secnonce;

            status = decrypt_data(encrypted_secnonce, seed, server_secnonce.data, sizeof(server_secnonce.data));
            if (status != 0) {
                throw std::runtime_error("\nSeed ecryption failed");
            }

            secp256k1_musig_session session;
            memcpy(session.data, session_data, session_data_size);

            secp256k1_musig_pubnonce server_pubnonce;
            secp256k1_musig_pubnonce_parse(ctx, &server_pubnonce, serialized_server_pubnonce);

            secp256k1_musig_partial_sig partial_sig;

            return_val = secp256k1_blinded_musig_partial_sign_without_keyaggcoeff(ctx, &partial_sig, &server_secnonce, &server_keypair, &session, negate_seckey);
            assert(return_val);

            unsigned char serialized_partial_sig[32];
            memset(serialized_partial_sig, 0, 32);

            return_val = secp256k1_musig_partial_sig_serialize(ctx,serialized_partial_sig, &partial_sig);
            assert(return_val);   

            memcpy(response.partial_sig_data, serialized_partial_sig, sizeof(serialized_partial_sig));

            secp256k1_context_destroy(ctx);

            return response;
        }

    NewKeyPairResponse key_update(
        unsigned char* seed, 
        utils::chacha20_poly1305_encrypted_data *old_encrypted_keypair,
        unsigned char* serialized_x1,
        unsigned char* serialized_t2) {

            NewKeyPairResponse response;
            memset(response.server_pubkey, 0, 33);
            utils::initialize_encrypted_data(response.encrypted_data, sizeof(secp256k1_keypair));

            // step 0 - Decrypt encrypted_keypair

            secp256k1_keypair server_keypair;
            memset(server_keypair.data, 0, sizeof(server_keypair.data));

            int status = decrypt_data(old_encrypted_keypair, seed, server_keypair.data, sizeof(server_keypair.data));
            if (status != 0) {
                throw std::runtime_error("\nSeed ecryption failed");
            }

            secp256k1_context* ctx = secp256k1_context_create(SECP256K1_CONTEXT_VERIFY);

            // step 1 - Extract server secret from keypair

            unsigned char server_seckey[32];
            int return_val = secp256k1_keypair_sec(ctx, server_seckey, &server_keypair);
            assert(return_val);

            unsigned char new_server_seckey[32];
            memcpy(new_server_seckey, server_seckey, 32);

            return_val = secp256k1_ec_seckey_tweak_add(ctx, new_server_seckey, serialized_t2);
            assert(return_val);

            unsigned char x1[32];
            memcpy(x1, serialized_x1, 32);

            return_val = secp256k1_ec_seckey_verify(ctx, x1);
            assert(return_val);

            return_val = secp256k1_ec_seckey_negate(ctx, x1);
            assert(return_val);

            return_val = secp256k1_ec_seckey_tweak_add(ctx, new_server_seckey, x1);
            assert(return_val);

            secp256k1_keypair new_server_keypair;

            return_val = secp256k1_keypair_create(ctx, &new_server_keypair, new_server_seckey);
            assert(return_val);

            secp256k1_pubkey new_server_pubkey;
            return_val = secp256k1_keypair_pub(ctx, &new_server_pubkey, &new_server_keypair);
            assert(return_val);

            unsigned char local_compressed_server_pubkey[33];
            memset(local_compressed_server_pubkey, 0, 33);

            size_t len = sizeof(local_compressed_server_pubkey);
            return_val = secp256k1_ec_pubkey_serialize(ctx, local_compressed_server_pubkey, &len, &new_server_pubkey, SECP256K1_EC_COMPRESSED);
            assert(return_val);
            // Should be the same size as the size of the output, because we passed a 33 byte array.
            assert(len == sizeof(local_compressed_server_pubkey));

            memcpy(response.server_pubkey, local_compressed_server_pubkey, 33);

            encrypt_data(&response.encrypted_data, seed, new_server_keypair.data, sizeof(secp256k1_keypair::data));

            secp256k1_context_destroy(ctx);

            return response;

        }

    /**
     * [D8] Attest a coin's signature count.
     *
     * WHY THIS EXISTS. `sig_count` is the right-hand side of the receiver's anti-theft census
     * (`se_num_sigs == flat_backups + tiers + superseded`, exact equality). Until now it travelled
     * from this database to the client as a bare integer with no signature, MAC or attestation at
     * any hop, so a coordinator that UNDER-REPORTED it by k could hide k co-signed rival states:
     * the census still balanced exactly and the receiver accepted a coin the sender could still
     * reclaim. Theft, resting entirely on coordinator honesty.
     *
     * WHY THIS KEY. The signature is made with THIS COIN'S server keypair — the same key whose
     * public half the receiver already binds to the on-chain tx0 output
     * (`validate_tx0_output_pubkey`: tx0 output == sender_pubkey + enclave_pubkey). So the verifier
     * is a value the client has already checked against the chain. No new key material, no new
     * trust anchor, and nothing to distribute.
     *
     * The 32-byte digest is built by the caller so that signer and verifier read one definition of
     * the preimage and cannot drift apart.
     */
    SigCountAttestationResponse attest_sig_count(
        unsigned char* seed,
        utils::chacha20_poly1305_encrypted_data *encrypted_keypair,
        const unsigned char* message32) {

        SigCountAttestationResponse response;
        memset(response.signature, 0, sizeof(response.signature));
        memset(response.xonly_pubkey, 0, sizeof(response.xonly_pubkey));

        secp256k1_context* ctx = secp256k1_context_create(SECP256K1_CONTEXT_SIGN | SECP256K1_CONTEXT_VERIFY);

        secp256k1_keypair server_keypair;
        int status = decrypt_data(encrypted_keypair, seed, server_keypair.data, sizeof(secp256k1_keypair::data));
        if (status != 0) {
            secp256k1_context_destroy(ctx);
            throw std::runtime_error("\nSeed ecryption failed");
        }

        // Auxiliary randomness, per BIP-340. Not optional: a deterministic-nonce failure here would
        // leak the coin's server key, which is the same key that co-signs its transactions.
        unsigned char auxiliary_rand[32];
        if (RAND_bytes(auxiliary_rand, sizeof(auxiliary_rand)) != 1) {
            secp256k1_context_destroy(ctx);
            throw std::runtime_error("Failed to generate random bytes");
        }

        int return_val = secp256k1_schnorrsig_sign32(ctx, response.signature, message32, &server_keypair, auxiliary_rand);
        assert(return_val);

        secp256k1_xonly_pubkey xonly_pubkey;
        return_val = secp256k1_keypair_xonly_pub(ctx, &xonly_pubkey, NULL, &server_keypair);
        assert(return_val);

        return_val = secp256k1_xonly_pubkey_serialize(ctx, response.xonly_pubkey, &xonly_pubkey);
        assert(return_val);

        secp256k1_context_destroy(ctx);

        return response;
    }


    // ═══════════════════════════════════════════════════════════════════════════════════════════
    // [D69] THE ENCLAVE'S LONG-TERM ATTESTATION IDENTITY
    // ═══════════════════════════════════════════════════════════════════════════════════════════
    //
    // See `enclave.h` for why this exists (TRUST-MODEL B11). In one line: an attestation signed by
    // the COIN's key can only be verified by someone who knows that coin's key, and the sole honest
    // way to learn it is the on-chain funding output — which a deep in-ladder-split ancestor
    // deliberately does not have.
    //
    // Derivation, and every part of it is load-bearing:
    //
    //   * DOMAIN SEPARATION. The seed already produces per-coin sealing keys; hashing it with a
    //     distinct ASCII tag means this key cannot collide with, or be substituted for, any other
    //     use of the same secret.
    //   * VERSION IN THE TAG. `/v1` so a future rotation is a different derivation rather than a
    //     silent change of meaning under the same name.
    //   * COUNTER RETRY. A SHA-256 output is not guaranteed to be a valid secp256k1 scalar (it can
    //     be zero or >= n). The probability is ~2^-128, so this loop will not run twice in the life
    //     of the universe — but "will not happen" is not "cannot happen", and a keypair_create
    //     failure here would be an unhandled throw at boot.
    static void derive_identity_keypair(unsigned char* seed, secp256k1_context* ctx,
                                        secp256k1_keypair* out) {
        const std::string domain = "utexo/attestation-identity/v1";
        for (unsigned int counter = 0; counter < 256; ++counter) {
            std::vector<unsigned char> preimage(domain.begin(), domain.end());
            preimage.insert(preimage.end(), seed, seed + 32);
            preimage.push_back(static_cast<unsigned char>(counter));

            unsigned char sk[32];
            SHA256(preimage.data(), preimage.size(), sk);

            if (secp256k1_keypair_create(ctx, out, sk) == 1) {
                memset(sk, 0, sizeof(sk));
                return;
            }
            memset(sk, 0, sizeof(sk));
        }
        throw std::runtime_error(
            "could not derive the attestation identity key from the seed after 256 attempts — this "
            "is cryptographically impossible with a real seed, so the seed is almost certainly not "
            "what this build thinks it is");
    }

    void attestation_identity_pubkey(unsigned char* seed, unsigned char* xonly_pubkey_out32) {
        secp256k1_context* ctx =
            secp256k1_context_create(SECP256K1_CONTEXT_SIGN | SECP256K1_CONTEXT_VERIFY);
        secp256k1_keypair kp;
        try {
            derive_identity_keypair(seed, ctx, &kp);
        } catch (...) {
            secp256k1_context_destroy(ctx);
            throw;
        }
        secp256k1_xonly_pubkey xonly;
        int ok = secp256k1_keypair_xonly_pub(ctx, &xonly, NULL, &kp);
        assert(ok);
        ok = secp256k1_xonly_pubkey_serialize(ctx, xonly_pubkey_out32, &xonly);
        assert(ok);
        memset(&kp, 0, sizeof(kp));
        secp256k1_context_destroy(ctx);
    }

    SigCountAttestationResponse attest_sig_count_identity(
        unsigned char* seed,
        const unsigned char* message32) {

        SigCountAttestationResponse response;
        memset(response.signature, 0, sizeof(response.signature));
        memset(response.xonly_pubkey, 0, sizeof(response.xonly_pubkey));

        secp256k1_context* ctx =
            secp256k1_context_create(SECP256K1_CONTEXT_SIGN | SECP256K1_CONTEXT_VERIFY);
        secp256k1_keypair kp;
        try {
            derive_identity_keypair(seed, ctx, &kp);
        } catch (...) {
            secp256k1_context_destroy(ctx);
            throw;
        }

        // Auxiliary randomness, per BIP-340. Not optional: this key signs every attestation the
        // enclave ever makes, so a deterministic-nonce failure would be worse here than anywhere
        // else in the process.
        unsigned char auxiliary_rand[32];
        if (RAND_bytes(auxiliary_rand, sizeof(auxiliary_rand)) != 1) {
            memset(&kp, 0, sizeof(kp));
            secp256k1_context_destroy(ctx);
            throw std::runtime_error("Failed to generate random bytes");
        }

        int ok = secp256k1_schnorrsig_sign32(ctx, response.signature, message32, &kp, auxiliary_rand);
        assert(ok);

        secp256k1_xonly_pubkey xonly;
        ok = secp256k1_keypair_xonly_pub(ctx, &xonly, NULL, &kp);
        assert(ok);
        ok = secp256k1_xonly_pubkey_serialize(ctx, response.xonly_pubkey, &xonly);
        assert(ok);

        memset(&kp, 0, sizeof(kp));
        secp256k1_context_destroy(ctx);
        return response;
    }

} // namespace enclave