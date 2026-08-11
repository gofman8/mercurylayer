#pragma once

#ifndef DB_MANAGER_H
#define DB_MANAGER_H

#include <memory>
#include <string>
#include "utils.h"

namespace db_manager {

    void serialize(const utils::chacha20_poly1305_encrypted_data* src, unsigned char* buffer, size_t* serialized_len);

    bool deserialize(const unsigned char* buffer, utils::chacha20_poly1305_encrypted_data* dest);

    bool save_generated_public_key(
        const utils::chacha20_poly1305_encrypted_data& encrypted_keypair, 
        unsigned char* server_public_key, size_t server_public_key_size,
        const std::string& statechain_id,
        std::string& error_message);

    bool load_generated_key_data(
        const std::string& statechain_id, 
        std::unique_ptr<utils::chacha20_poly1305_encrypted_data>& encrypted_keypair,
        std::unique_ptr<utils::chacha20_poly1305_encrypted_data>& encrypted_secnonce,
        unsigned char* public_nonce, const size_t public_nonce_size, 
        std::string& error_message);

    bool update_sealed_secnonce(
        const std::string& statechain_id,
        unsigned char* serialized_server_pubnonce, const size_t serialized_server_pubnonce_size,
        const utils::chacha20_poly1305_encrypted_data& encrypted_secnonce,
        std::string& error_message);

    // Atomically load the sealed keypair + secnonce AND null the secnonce in a single
    // row-locked transaction (SELECT ... FOR UPDATE, then UPDATE ... SET sealed_secnonce = NULL).
    // This makes the secnonce SINGLE-USE at the enclave: a second concurrent/subsequent partial
    // signature over the same statechain_id blocks on the row lock, then observes a NULL secnonce
    // (returned as empty encrypted_secnonce) and must be refused by the caller. Without this, two
    // partial signatures produced with one secnonce over two different challenges leak the SE key
    // share (MuSig2 nonce reuse). Used ONLY by generate_partial_signature.
    bool load_and_consume_secnonce(
        const std::string& statechain_id,
        std::unique_ptr<utils::chacha20_poly1305_encrypted_data>& encrypted_keypair,
        std::unique_ptr<utils::chacha20_poly1305_encrypted_data>& encrypted_secnonce,
        unsigned char* public_nonce, const size_t public_nonce_size,
        std::string& error_message);

    bool update_sig_count(const std::string& statechain_id);

    // [KEYSTONE / retry-safety] Idempotent signing-round cache.
    //
    // A signing round is TWO calls (sign/first seals a secnonce; sign/second produces the partial sig and
    // increments sig_count). If the sign/second RESPONSE is lost in flight, sig_count has advanced but the
    // client holds no tier; a naive retry re-consumes a now-empty secnonce → 400 → the SE count and the
    // client's disclosed tier set desynchronise PERMANENTLY, which bricks the coin (its receiver census
    // `num_sigs == v1_backups + tiers + superseded` can never rebalance). This is a BENIGN failure — no
    // attacker needed — and it is the last thing gating V2-as-default.
    //
    // Fix: cache the produced partial sig keyed on (statechain_id, session). A retry that presents the SAME
    // session returns the cached sig WITHOUT re-signing and WITHOUT re-incrementing. A DIFFERENT session
    // after the secnonce is consumed still 400s, so the MuSig2 nonce-reuse guard is untouched.
    //
    // get_cached_partial_sig: true + out_partial_sig set iff a row exists for (statechain_id, session_key).
    bool get_cached_partial_sig(
        const std::string& statechain_id,
        const std::string& session_key,
        std::string& out_partial_sig);

    // store_partial_sig_and_increment: in ONE transaction, insert the cache row AND increment sig_count —
    // both or neither. The increment is bound to the INSERT actually adding a row (ON CONFLICT DO NOTHING
    // + affected-rows guard), so a session can never be double-counted. Replaces the standalone
    // update_sig_count on the produce path.
    bool store_partial_sig_and_increment(
        const std::string& statechain_id,
        const std::string& session_key,
        const std::string& partial_sig,
        std::string& error_message);

    // Create/upgrade every table this process needs. Called ONCE AT BOOT, so a schema change lands
    // on an existing database rather than waiting for whichever code path happens to carry a
    // `CREATE TABLE IF NOT EXISTS`. Idempotent; a failure here is fatal at startup by design.
    bool ensure_schema(std::string& error_message);

    bool signature_count(const std::string& statechain_id, int& sig_count);

    // ── [D8(i)] THE SPEND BUDGET, MIRRORED INTO THE SE ────────────────────────────────────────────
    //
    // The coordinator has always held a `sig_budget` and refused to co-sign past it, which makes a
    // node terminal. But terminality is a fact a RECEIVER has to rely on — the census argument for a
    // split child assumes its parent can never be spent again — and until now the only witness to it
    // was the coordinator, i.e. the party a receiver is being protected from. The SE could attest the
    // COUNT (D8) but had no notion of a budget, so it could not attest that the count was final.
    //
    // set_sig_budget is MONOTONE, and that is the whole security content: a budget may be created
    // where none exists, or LOWERED, never raised. Without that rule a coordinator could hand a
    // receiver an attestation saying "1 of 1 consumed, terminal", then raise the budget and co-sign a
    // rival — the attestation would have been true when issued and worthless a block later.
    // Terminality has to be a ratchet or it is not a property at all.
    //
    // Returns false on a DB failure OR on a rejected raise; `error_message` distinguishes them.
    bool set_sig_budget(const std::string& statechain_id, int budget, std::string& error_message);

    // `has_budget` is false when this coin has no budget (co-signable indefinitely, the default).
    bool get_sig_budget(const std::string& statechain_id, int& budget, bool& has_budget);

    bool update_sealed_keypair(
        const utils::chacha20_poly1305_encrypted_data& encrypted_keypair, 
        unsigned char* server_public_key, size_t server_public_key_size,
        const std::string& statechain_id,
        std::string& error_message);

    bool delete_statechain(const std::string& statechain_id);
}

#endif // DB_MANAGER_H