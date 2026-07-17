#pragma once

#ifndef DB_MANAGER_H
#define DB_MANAGER_H

#include "../Enclave_u.h"
#include <memory>
#include <string>

namespace db_manager {

    void serialize(const chacha20_poly1305_encrypted_data* src, unsigned char* buffer, size_t* serialized_len);

    bool deserialize(const unsigned char* buffer, chacha20_poly1305_encrypted_data* dest);

    bool save_generated_public_key(
        const chacha20_poly1305_encrypted_data& encrypted_keypair, 
        unsigned char* server_public_key, size_t server_public_key_size,
        const std::string& statechain_id,
        std::string& error_message);

    bool load_generated_key_data(
        const std::string& statechain_id, 
        std::unique_ptr<chacha20_poly1305_encrypted_data>& encrypted_keypair,
        std::unique_ptr<chacha20_poly1305_encrypted_data>& encrypted_secnonce,
        unsigned char* public_nonce, const size_t public_nonce_size, 
        std::string& error_message);

    // [P0-1 / SGX LANE] Load the key data AND consume the sealed secnonce in one transaction.
    //
    // MUST be used instead of load_generated_key_data on any path that produces a partial signature.
    // A secnonce that survives a signing round can be signed with a SECOND time over a DIFFERENT
    // message, and two Schnorr/MuSig2 partial sigs sharing a nonce algebraically reveal the SE's
    // secret key share:  s1 = k + c1*x, s2 = k + c2*x  =>  x = (s1 - s2) / (c1 - c2).
    // The lockbox lane has had this guard since P0-1 (lockbox/src/db_manager.cpp:215); this SGX lane
    // did not, which is why it is added here.
    bool load_and_consume_secnonce(
        const std::string& statechain_id,
        std::unique_ptr<chacha20_poly1305_encrypted_data>& encrypted_keypair,
        std::unique_ptr<chacha20_poly1305_encrypted_data>& encrypted_secnonce,
        unsigned char* public_nonce, const size_t public_nonce_size,
        std::string& error_message);

    bool update_sealed_secnonce(
        const std::string& statechain_id,
        unsigned char* serialized_server_pubnonce, const size_t serialized_server_pubnonce_size,
        const chacha20_poly1305_encrypted_data& encrypted_secnonce,
        std::string& error_message);

    bool update_sig_count(const std::string& statechain_id);

    bool signature_count(const std::string& statechain_id, int& sig_count);

    bool update_sealed_keypair(
        const chacha20_poly1305_encrypted_data& encrypted_keypair, 
        unsigned char* server_public_key, size_t server_public_key_size,
        const std::string& statechain_id,
        std::string& error_message);

    bool delete_statechain(const std::string& statechain_id);
}

#endif // DB_MANAGER_H