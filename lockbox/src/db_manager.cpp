#include "db_manager.h"

#include <iostream>
#include <memory>
#include <pqxx/pqxx>
#include <string>
#include <toml++/toml.h>
#include "utils.h"

namespace db_manager {

    std::string getDatabaseConnectionString() {
        return utils::getStringConfigVar(utils::DATABASE_URL);
    }

    // Assumes the buffer is large enough. In a real application, ensure buffer safety.
    void serialize(const utils::chacha20_poly1305_encrypted_data* src, unsigned char* buffer, size_t* serialized_len) {
        // Copy `data_len`, `nonce`, and `mac` directly
        size_t offset = 0;
        memcpy(buffer + offset, &src->data_len, sizeof(src->data_len));
        offset += sizeof(src->data_len);

        memcpy(buffer + offset, src->nonce, sizeof(src->nonce));
        offset += sizeof(src->nonce);

        memcpy(buffer + offset, src->mac, sizeof(src->mac));
        offset += sizeof(src->mac);

        // Now copy dynamic `data`
        memcpy(buffer + offset, src->data, src->data_len);
        offset += src->data_len;

        *serialized_len = offset;
    }

    // Returns a newly allocated structure that must be freed by the caller.
    bool deserialize(const unsigned char* buffer, utils::chacha20_poly1305_encrypted_data* dest) {

        if (!dest) return false;

        size_t offset = 0;
        memcpy(&dest->data_len, buffer + offset, sizeof(dest->data_len));
        offset += sizeof(dest->data_len);

        memcpy(dest->nonce, buffer + offset, sizeof(dest->nonce));
        offset += sizeof(dest->nonce);

        memcpy(dest->mac, buffer + offset, sizeof(dest->mac));
        offset += sizeof(dest->mac);

        dest->data = new unsigned char[dest->data_len];
        if (!dest->data) {
            return false; // NULL;
        }
        memcpy(dest->data, buffer + offset, dest->data_len);

        return true;
    }

    // Create/upgrade every table this process needs. Called once at boot (see server.cpp) so a
    // schema change lands on an EXISTING database instead of waiting for the code path that happens
    // to contain a `CREATE TABLE IF NOT EXISTS`.
    //
    // Idempotent by construction: every statement is `IF NOT EXISTS`, so running it on each start is
    // free and there is no "have I migrated yet" state to get wrong.
    bool ensure_schema(std::string& error_message) {
        try
        {
            pqxx::connection conn(getDatabaseConnectionString());
            if (!conn.is_open()) {
                error_message = "could not open the lockbox database";
                return false;
            }
            pqxx::work txn(conn);
            txn.exec(
                "CREATE TABLE IF NOT EXISTS generated_public_key ( "
                "id SERIAL PRIMARY KEY, "
                "statechain_id varchar(50), "
                "sealed_keypair BYTEA, "
                "sealed_secnonce BYTEA, "
                "public_nonce BYTEA, "
                "public_key BYTEA UNIQUE, "
                "sig_count INTEGER DEFAULT 0, "
                // [D8(i)] NULL = no budget = co-signable indefinitely, the default and the
                // pre-existing behaviour. A value is a ratchet: see set_sig_budget.
                "sig_budget INTEGER);");
            txn.exec("ALTER TABLE generated_public_key ADD COLUMN IF NOT EXISTS sig_budget INTEGER;");

            // ── [REQ-56 / REQ-59 / REQ-61 / REQ-65] THE LEAF REGISTRY ────────────────────────────
            //
            // Declared HERE, not lazily inside the accessors that use them. That is not a style
            // preference: `signed_session_cache` is created inside `get_cached_partial_sig` and
            // `store_partial_sig_and_increment` rather than at boot, and the `sig_budget` ALTER two
            // lines above exists because that shape already cost one deployment. A table that
            // appears only when a particular code path runs is a table that is missing exactly when
            // an unusual path runs first.
            //
            // What the SE is allowed to record here is narrow, and the narrowness is the point
            // (REQ-58): every column below is either something the SE AUTHORED (it signed under this
            // sid) or something it WITNESSED through binding (a value and a key read out of a
            // transaction whose sighash it recomputed). Nothing here is a client assertion, because
            // an offline holder could not trust one.
            txn.exec(
                // The SE's own index from a transaction it hashed itself to the sid it signed that
                // transaction under. This is what makes a parent edge SE-authored rather than
                // client-asserted: a child's tier spends (SP.txid, j), and only the SE can say which
                // sid it co-signed SP under.
                "CREATE TABLE IF NOT EXISTS se_signed_tx ( "
                "txid BYTEA PRIMARY KEY, "
                "statechain_id varchar(50) NOT NULL, "
                "sig_index INTEGER NOT NULL);");
            txn.exec(
                "CREATE INDEX IF NOT EXISTS se_signed_tx_sid ON se_signed_tx (statechain_id);");
            txn.exec(
                "CREATE TABLE IF NOT EXISTS se_leaf ( "
                "statechain_id varchar(50) PRIMARY KEY, "
                // NULL for a node whose parent is the root itself.
                "parent_statechain_id varchar(50), "
                "root_statechain_id varchar(50) NOT NULL, "
                // REQ-60: the FULL funding value, never the exit value. The two tier rungs are never
                // broadcast, so their burn is never realised and is not the SSP's to keep.
                "fund_value BIGINT NOT NULL, "
                // REQ-65: the payee's x-only key, read from the WITNESSED payload output of the
                // state tier — identified structurally as the unique P2TR output (REQ-61a), never at
                // a client-supplied index.
                "exit_key BYTEA NOT NULL, "
                // REQ-61: write-once. NULL until armed.
                "latch_key BYTEA, "
                // Form (a)/(b): monotone, never cleared. A release is a consent record, not a
                // spending authorisation.
                "released BOOLEAN NOT NULL DEFAULT false);");
            txn.exec(
                "CREATE INDEX IF NOT EXISTS se_leaf_root ON se_leaf (root_statechain_id);");
            txn.exec(
                "CREATE TABLE IF NOT EXISTS se_root ( "
                "root_statechain_id varchar(50) PRIMARY KEY, "
                // INV-FREEZE: set PROSPECTIVELY and irreversibly, inside collapse_grant, in the same
                // transaction as the frontier check — so no leaf can appear between the check and
                // the ratchet (REQ-64).
                "frozen BOOLEAN NOT NULL DEFAULT false, "
                // REQ-64b: named so a refused wallet retries on the successor instead of surfacing
                // an error. The SE cannot authenticate this (it has no chain access, REQ-58), so it
                // is an operator declaration and MUST be treated by wallets as a retry HINT that
                // they verify themselves — never as proof.
                "successor_root varchar(50));");
            txn.exec(
                "CREATE TABLE IF NOT EXISTS se_release_nonce ( "
                "statechain_id varchar(50) NOT NULL, "
                // R2's single-use nonce. PRIMARY KEY over (sid, nonce) is the replay defence: a
                // second presentation of the same nonce collides and is refused by the database
                // rather than by a check someone can forget to write.
                "nonce BYTEA NOT NULL, "
                "PRIMARY KEY (statechain_id, nonce));");

            txn.commit();
            return true;
        }
        catch (std::exception const &e)
        {
            error_message = e.what();
            return false;
        }
    }

    bool save_generated_public_key(
        const utils::chacha20_poly1305_encrypted_data& encrypted_keypair, 
        unsigned char* server_public_key, size_t server_public_key_size,
        const std::string& statechain_id,
        std::string& error_message) {

        auto database_connection_string = getDatabaseConnectionString();

        try
        {
            pqxx::connection conn(database_connection_string);
            if (conn.is_open()) {

                std::string create_table_query =
                    "CREATE TABLE IF NOT EXISTS generated_public_key ( "
                    "id SERIAL PRIMARY KEY, "
                    "statechain_id varchar(50), "
                    "sealed_keypair BYTEA, "
                    "sealed_secnonce BYTEA, "
                    "public_nonce BYTEA, "
                    "public_key BYTEA UNIQUE, "
                    "sig_count INTEGER DEFAULT 0, "
                    // [D8(i)] NULL = no budget = co-signable indefinitely, which is the default and
                    // the pre-existing behaviour. A value is a ratchet: see set_sig_budget.
                    "sig_budget INTEGER);";

                pqxx::work txn(conn);
                txn.exec(create_table_query);
                // `CREATE TABLE IF NOT EXISTS` is a no-op on an existing database, so the column
                // above never appears on one. Every deployment that predates this line needs the
                // ALTER, and running it unconditionally is cheaper than detecting whether it is
                // needed.
                txn.exec("ALTER TABLE generated_public_key ADD COLUMN IF NOT EXISTS sig_budget INTEGER;");
                txn.commit();


                size_t serialized_len = 0;

                size_t bufferSize = sizeof(encrypted_keypair.data_len) + sizeof(encrypted_keypair.nonce) + sizeof(encrypted_keypair.mac) + encrypted_keypair.data_len;
                unsigned char* buffer = (unsigned char*) malloc(bufferSize);

                if (!buffer) {
                    error_message = "Failed to allocate memory for serialization!";
                    return false;
                }

                serialize(&encrypted_keypair, buffer, &serialized_len);
                assert(serialized_len == bufferSize);

                std::basic_string_view<std::byte> sealed_data_view(reinterpret_cast<std::byte*>(buffer), bufferSize);
                std::basic_string_view<std::byte> public_key_data_view(reinterpret_cast<std::byte*>(server_public_key), server_public_key_size);

                std::string insert_query =
                    "INSERT INTO generated_public_key (sealed_keypair, public_key, statechain_id) VALUES ($1, $2, $3);";
                pqxx::work txn2(conn);

                txn2.exec_params(insert_query, sealed_data_view, public_key_data_view, statechain_id);
                txn2.commit();

                conn.close();
                return true;

                return true;
            } else {
                error_message = "Failed to connect to the database!";
                return false;
            }
        }
        catch (std::exception const &e)
        {
            error_message = e.what();
            return false;
        }

        return true;
    }

    bool load_generated_key_data(
        const std::string& statechain_id, 
        std::unique_ptr<utils::chacha20_poly1305_encrypted_data>& encrypted_keypair,
        std::unique_ptr<utils::chacha20_poly1305_encrypted_data>& encrypted_secnonce,
        unsigned char* public_nonce, const size_t public_nonce_size, 
        std::string& error_message)
    {
        auto database_connection_string = getDatabaseConnectionString();

        try
        {
            pqxx::connection conn(database_connection_string);
            if (conn.is_open()) {

                std::string sealed_keypair_query =
                    "SELECT sealed_keypair, sealed_secnonce, public_nonce FROM generated_public_key WHERE statechain_id = $1;";

                pqxx::nontransaction ntxn(conn);

                conn.prepare("load_generated_key_data_query", sealed_keypair_query);

                pqxx::result result = ntxn.exec_prepared("load_generated_key_data_query", statechain_id);

                if (!result.empty()) {
                    auto sealed_keypair_field = result[0]["sealed_keypair"];
                    auto sealed_secnonce_field = result[0]["sealed_secnonce"];
                    auto public_nonce_field = result[0]["public_nonce"];

                    if (sealed_keypair_field.is_null()) {
                        encrypted_keypair.reset();
                    } else if (encrypted_keypair != nullptr) {
                        auto sealed_keypair_view = sealed_keypair_field.as<std::basic_string<std::byte>>();

                        std::vector<unsigned char> sealed_keypair(sealed_keypair_view.size());
                        memcpy(sealed_keypair.data(), sealed_keypair_view.data(), sealed_keypair_view.size());

                        if (!deserialize(sealed_keypair.data(), encrypted_keypair.get())) {
                            error_message = "Failed to deserialize keypair!";
                            return false;
                        }
                    }

                    if (sealed_secnonce_field.is_null()) {
                        encrypted_secnonce.reset();
                    } else if (encrypted_secnonce != nullptr) {
                        auto sealed_secnonce_view = sealed_secnonce_field.as<std::basic_string<std::byte>>();

                        std::vector<unsigned char> sealed_secnonce(sealed_secnonce_view.size());
                        memcpy(sealed_secnonce.data(), sealed_secnonce_view.data(), sealed_secnonce_view.size());

                        if (!deserialize(sealed_secnonce.data(), encrypted_secnonce.get())) {
                            error_message = "Failed to deserialize keypair!";
                            return false;
                        }
                    } 

                    if (!public_nonce_field.is_null() && public_nonce != nullptr) {
                        auto public_nonce_view = public_nonce_field.as<std::basic_string<std::byte>>();

                        if (public_nonce_view.size() != public_nonce_size) {
                            error_message = "Failed to retrieve public nonce. Different size than expected !";
                            return false;
                        }

                        memcpy(public_nonce, public_nonce_view.data(), public_nonce_size);
                    }
                }
                else {
                    error_message = "Failed to retrieve keypair. No data found !";
                    return false;
                }

                conn.close();
                return true;
            } else {
                error_message = "Failed to connect to the database!";
                return false;
            }
        }
        catch (std::exception const &e)
        {
            error_message = e.what();
            return false;
        }
    }

    bool load_and_consume_secnonce(
        const std::string& statechain_id,
        std::unique_ptr<utils::chacha20_poly1305_encrypted_data>& encrypted_keypair,
        std::unique_ptr<utils::chacha20_poly1305_encrypted_data>& encrypted_secnonce,
        unsigned char* public_nonce, const size_t public_nonce_size,
        std::string& error_message)
    {
        auto database_connection_string = getDatabaseConnectionString();

        try
        {
            pqxx::connection conn(database_connection_string);
            if (!conn.is_open()) {
                error_message = "Failed to connect to the database!";
                return false;
            }

            // Single transaction: FOR UPDATE locks the row so a concurrent partial-signature call
            // for the same statechain_id serializes behind us. We read the sealed secnonce and, in
            // the SAME transaction, null it. The loser of the race observes a NULL secnonce.
            pqxx::work txn(conn);

            std::string select_query =
                "SELECT sealed_keypair, sealed_secnonce, public_nonce FROM generated_public_key "
                "WHERE statechain_id = $1 FOR UPDATE;";

            pqxx::result result = txn.exec_params(select_query, statechain_id);

            if (result.empty()) {
                error_message = "Failed to retrieve keypair. No data found !";
                return false;
            }

            auto sealed_keypair_field = result[0]["sealed_keypair"];
            auto sealed_secnonce_field = result[0]["sealed_secnonce"];
            auto public_nonce_field = result[0]["public_nonce"];

            if (sealed_keypair_field.is_null()) {
                encrypted_keypair.reset();
            } else if (encrypted_keypair != nullptr) {
                auto sealed_keypair_view = sealed_keypair_field.as<std::basic_string<std::byte>>();

                std::vector<unsigned char> sealed_keypair(sealed_keypair_view.size());
                memcpy(sealed_keypair.data(), sealed_keypair_view.data(), sealed_keypair_view.size());

                if (!deserialize(sealed_keypair.data(), encrypted_keypair.get())) {
                    error_message = "Failed to deserialize keypair!";
                    return false;
                }
            }

            bool had_secnonce = false;
            if (sealed_secnonce_field.is_null()) {
                encrypted_secnonce.reset();
            } else if (encrypted_secnonce != nullptr) {
                auto sealed_secnonce_view = sealed_secnonce_field.as<std::basic_string<std::byte>>();

                std::vector<unsigned char> sealed_secnonce(sealed_secnonce_view.size());
                memcpy(sealed_secnonce.data(), sealed_secnonce_view.data(), sealed_secnonce_view.size());

                if (!deserialize(sealed_secnonce.data(), encrypted_secnonce.get())) {
                    error_message = "Failed to deserialize secnonce!";
                    return false;
                }
                had_secnonce = true;
            }

            if (!public_nonce_field.is_null() && public_nonce != nullptr) {
                auto public_nonce_view = public_nonce_field.as<std::basic_string<std::byte>>();

                if (public_nonce_view.size() != public_nonce_size) {
                    error_message = "Failed to retrieve public nonce. Different size than expected !";
                    return false;
                }

                memcpy(public_nonce, public_nonce_view.data(), public_nonce_size);
            }

            // Consume the secnonce in the same transaction so it can never be signed with twice.
            if (had_secnonce) {
                txn.exec_params(
                    "UPDATE generated_public_key SET sealed_secnonce = NULL WHERE statechain_id = $1;",
                    statechain_id);
            }
            txn.commit();

            conn.close();
            return true;
        }
        catch (std::exception const &e)
        {
            error_message = e.what();
            return false;
        }
    }

    bool update_sealed_secnonce(
        const std::string& statechain_id,
        unsigned char* serialized_server_pubnonce, const size_t serialized_server_pubnonce_size,
        const utils::chacha20_poly1305_encrypted_data& encrypted_secnonce,
        std::string& error_message)
    {
        auto database_connection_string = getDatabaseConnectionString();

        try
        {
            pqxx::connection conn(database_connection_string);
            if (conn.is_open()) {

                size_t serialized_len = 0;

                size_t bufferSize = sizeof(encrypted_secnonce.data_len) + sizeof(encrypted_secnonce.nonce) + sizeof(encrypted_secnonce.mac) + encrypted_secnonce.data_len;
                unsigned char* buffer = (unsigned char*) malloc(bufferSize);

                if (!buffer) {
                    error_message = "Failed to allocate memory for serialization!";
                    return false;
                }

                serialize(&encrypted_secnonce, buffer, &serialized_len);
                assert(serialized_len == bufferSize);

                std::basic_string_view<std::byte> sealed_secnonce_view(reinterpret_cast<std::byte*>(buffer), bufferSize);
                std::basic_string_view<std::byte> serialized_server_pubnonce_view(reinterpret_cast<std::byte*>(serialized_server_pubnonce), serialized_server_pubnonce_size);

                std::string updated_query =
                    "UPDATE generated_public_key SET public_nonce = $1, sealed_secnonce = $2 WHERE statechain_id = $3";
                pqxx::work txn(conn);

                txn.exec_params(updated_query, serialized_server_pubnonce_view, sealed_secnonce_view, statechain_id);
                txn.commit();

                conn.close();
                return true;
            } else {
                error_message = "Failed to connect to the database!";
                return false;
            }
        }
        catch (std::exception const &e)
        {
            error_message = e.what();
            return false;
        }
    }

    bool update_sig_count(const std::string& statechain_id) 
    {
        auto database_connection_string = getDatabaseConnectionString();

        try
        {
            pqxx::connection conn(database_connection_string);
            if (conn.is_open()) {

                std::string update_query =
                    "UPDATE generated_public_key SET sig_count = sig_count + 1 WHERE statechain_id = $1;";
                pqxx::work txn(conn);

                txn.exec_params(update_query, statechain_id);
                txn.commit();

                conn.close();
                return true;
            } else {
                return false;
            }
        }
        catch (std::exception const &e)
        {
            return false;
        }
    }

    // [KEYSTONE] See db_manager.h. Look up a previously-produced partial sig for an EXACT session replay.
    bool get_cached_partial_sig(
        const std::string& statechain_id,
        const std::string& session_key,
        std::string& out_partial_sig)
    {
        auto database_connection_string = getDatabaseConnectionString();

        try
        {
            pqxx::connection conn(database_connection_string);
            if (!conn.is_open()) {
                return false;
            }

            pqxx::work txn(conn);
            // Create-if-absent so a lookup before the first store never throws (older coins predate this).
            txn.exec(
                "CREATE TABLE IF NOT EXISTS signed_session_cache ("
                "statechain_id varchar(50) NOT NULL, "
                "session_key varchar NOT NULL, "
                "partial_sig varchar NOT NULL, "
                "PRIMARY KEY (statechain_id, session_key));");

            pqxx::result r = txn.exec_params(
                "SELECT partial_sig FROM signed_session_cache "
                "WHERE statechain_id = $1 AND session_key = $2;",
                statechain_id, session_key);
            txn.commit();

            if (r.empty() || r[0]["partial_sig"].is_null()) {
                return false;
            }
            out_partial_sig = r[0]["partial_sig"].as<std::string>();
            return true;
        }
        catch (std::exception const &e)
        {
            // Fail SAFE, not open: on any cache error report "no cached sig" so the caller falls through
            // to the normal consume+sign path (which the secnonce single-use still guards). Worst case a
            // retry that could have been served from cache instead 400s — degrades to pre-cache behaviour,
            // never a double-sign.
            return false;
        }
    }

    // [KEYSTONE] See db_manager.h. Atomically: insert the cache row AND increment sig_count, both or
    // neither, and count at most once per session.
    bool store_partial_sig_and_increment(
        const std::string& statechain_id,
        const std::string& session_key,
        const std::string& partial_sig,
        std::string& error_message)
    {
        auto database_connection_string = getDatabaseConnectionString();

        try
        {
            pqxx::connection conn(database_connection_string);
            if (!conn.is_open()) {
                error_message = "Failed to connect to the database!";
                return false;
            }

            pqxx::work txn(conn);

            txn.exec(
                "CREATE TABLE IF NOT EXISTS signed_session_cache ("
                "statechain_id varchar(50) NOT NULL, "
                "session_key varchar NOT NULL, "
                "partial_sig varchar NOT NULL, "
                "PRIMARY KEY (statechain_id, session_key));");

            // Insert the produced sig. ON CONFLICT DO NOTHING makes a re-store of the SAME session a
            // no-op, and the increment below is gated on a row ACTUALLY being inserted, so the SE count
            // can never be inflated by a duplicate store of one session.
            pqxx::result ins = txn.exec_params(
                "INSERT INTO signed_session_cache (statechain_id, session_key, partial_sig) "
                "VALUES ($1, $2, $3) ON CONFLICT (statechain_id, session_key) DO NOTHING;",
                statechain_id, session_key, partial_sig);

            if (ins.affected_rows() == 1) {
                txn.exec_params(
                    "UPDATE generated_public_key SET sig_count = sig_count + 1 WHERE statechain_id = $1;",
                    statechain_id);
            }

            txn.commit();
            conn.close();
            return true;
        }
        catch (std::exception const &e)
        {
            error_message = e.what();
            return false;
        }
    }

    // [D8(i)] See db_manager.h for why this is a RATCHET and not a setter.
    bool set_sig_budget(const std::string& statechain_id, int budget, std::string& error_message) {

        auto database_connection_string = getDatabaseConnectionString();

        if (budget < 0) {
            error_message = "a negative spend budget is meaningless";
            return false;
        }

        try
        {
            pqxx::connection conn(database_connection_string);
            if (!conn.is_open()) {
                error_message = "failed to open the lockbox database";
                return false;
            }

            // The read and the write share ONE transaction. Split across two, a concurrent request
            // could read "no budget", another could lower it to 1, and the first could then write 5
            // — a raise, assembled out of two individually-legal steps. The WHERE clause carries the
            // monotonicity so the database decides it, not this process.
            pqxx::work txn(conn);
            conn.prepare(
                "set_budget_monotone",
                "UPDATE generated_public_key SET sig_budget = $1 "
                "WHERE statechain_id = $2 AND (sig_budget IS NULL OR sig_budget > $1);");
            pqxx::result r = txn.exec_prepared("set_budget_monotone", budget, statechain_id);

            if (r.affected_rows() == 0) {
                // Either the coin does not exist, or the write would have RAISED the budget. Tell
                // them apart: the second is an attempted downgrade of a terminality guarantee a
                // receiver may already be relying on, and it must not read as a benign no-op.
                conn.prepare(
                    "read_budget",
                    "SELECT sig_budget FROM generated_public_key WHERE statechain_id = $1;");
                pqxx::result cur = txn.exec_prepared("read_budget", statechain_id);
                txn.commit();
                if (cur.empty()) {
                    error_message = "unknown statechain id";
                    return false;
                }
                auto field = cur[0]["sig_budget"];
                if (!field.is_null() && field.as<int>() <= budget) {
                    if (field.as<int>() == budget) {
                        return true;  // idempotent: the same budget, set again, is not a raise
                    }
                    error_message =
                        "refusing to RAISE the spend budget from " + std::to_string(field.as<int>()) +
                        " to " + std::to_string(budget) +
                        ": terminality is a ratchet. A receiver may already hold an attestation "
                        "saying this coin is spent out; raising the budget would make that "
                        "attestation false after the fact and license a rival co-signature.";
                    return false;
                }
                error_message = "the spend budget could not be set";
                return false;
            }

            txn.commit();
            return true;
        }
        catch (std::exception const &e)
        {
            error_message = std::string("set_sig_budget failed: ") + e.what();
            return false;
        }
    }

    bool get_sig_budget(const std::string& statechain_id, int& budget, bool& has_budget) {

        auto database_connection_string = getDatabaseConnectionString();
        has_budget = false;

        try
        {
            pqxx::connection conn(database_connection_string);
            if (!conn.is_open()) {
                return false;
            }

            pqxx::nontransaction ntxn(conn);
            conn.prepare("get_budget", "SELECT sig_budget FROM generated_public_key WHERE statechain_id = $1;");
            pqxx::result result = ntxn.exec_prepared("get_budget", statechain_id);

            if (!result.empty()) {
                auto field = result[0]["sig_budget"];
                if (!field.is_null()) {
                    budget = field.as<int>();
                    has_budget = true;
                }
            }
            conn.close();
            return true;
        }
        catch (std::exception const &e)
        {
            return false;
        }
    }

    /// **[D76] The count is an OUT-PARAM, so every return path must assign it — and "no such coin"
    /// must be distinguishable from "zero signatures".**
    ///
    /// This function used to return `true` on `result.empty()` (no row for this statechain id) and on
    /// a NULL `sig_count` column WITHOUT ever assigning `sig_count`. Both callers then read whatever
    /// happened to be on the stack: the `/signature_count` route declared `int sig_count;`
    /// uninitialised and ATTESTED the value, and the budget gate compared it against the spend
    /// budget. An uninitialised read is undefined behaviour, and in the attestation's case it is
    /// undefined behaviour that gets SIGNED.
    ///
    /// This is a correctness fix, not a closed theft path: in practice the stack slot held zero and
    /// the budget gate initialises its own variable. Stating it that way rather than as a security
    /// win is the honest description ([D73]).
    ///
    /// `found` distinguishes the two "true" cases: query succeeded AND a row with a non-NULL count
    /// exists (`found = true`), versus query succeeded and there is no such count (`found = false`,
    /// `sig_count = 0`). A caller that cannot tell them apart cannot fail closed.
    bool signature_count(const std::string& statechain_id, int& sig_count, bool& found) {

        sig_count = 0;
        found = false;

        auto database_connection_string = getDatabaseConnectionString();

        try
        {
            pqxx::connection conn(database_connection_string);
            if (conn.is_open()) {

                std::string sig_count_query =
                    "SELECT sig_count FROM generated_public_key WHERE statechain_id = $1;";
            
                pqxx::nontransaction ntxn(conn);

                conn.prepare("sig_count_query", sig_count_query);

                pqxx::result result = ntxn.exec_prepared("sig_count_query", statechain_id);

                if (!result.empty()) {
                    auto sig_count_field = result[0]["sig_count"];
                    if (!sig_count_field.is_null()) {
                        sig_count = sig_count_field.as<int>();
                        found = true;
                        conn.close();
                        return true;
                    }
                }

                // No row, or a NULL column. The QUERY succeeded, so this is not an error — but there
                // is no count, and `found` says so. `sig_count` is already 0 from the prologue.
                conn.close();
                return true;
            } else {
                return false;
            }
        }
        catch (std::exception const &e)
        {
            return false;
        }
    }

    bool update_sealed_keypair(
        const utils::chacha20_poly1305_encrypted_data& encrypted_keypair, 
        unsigned char* server_public_key, size_t server_public_key_size,
        const std::string& statechain_id,
        std::string& error_message) 
    {
        auto database_connection_string = getDatabaseConnectionString();

        try
        {
            pqxx::connection conn(database_connection_string);
            if (conn.is_open()) {

                std::string insert_query =
                    "UPDATE generated_public_key "
                    "SET sealed_keypair = $1, public_key = $2, sealed_secnonce = NULL, public_nonce = NULL "
                    "WHERE statechain_id = $3;";
                pqxx::work txn2(conn);

                size_t serialized_len = 0;

                size_t bufferSize = sizeof(encrypted_keypair.data_len) + sizeof(encrypted_keypair.nonce) + sizeof(encrypted_keypair.mac) + encrypted_keypair.data_len;
                unsigned char* buffer = (unsigned char*) malloc(bufferSize);

                if (!buffer) {
                    error_message = "Failed to allocate memory for serialization!";
                    return false;
                }

                serialize(&encrypted_keypair, buffer, &serialized_len);
                assert(serialized_len == bufferSize);

                std::basic_string_view<std::byte> sealed_data_view(reinterpret_cast<std::byte*>(buffer), bufferSize);
                std::basic_string_view<std::byte> public_key_data_view(reinterpret_cast<std::byte*>(server_public_key), server_public_key_size);

                txn2.exec_params(insert_query, sealed_data_view, public_key_data_view, statechain_id);
                txn2.commit();

                conn.close();
                return true;
            } else {
                error_message = "Failed to connect to the database!";
                return false;
            }
        }
        catch (std::exception const &e)
        {
            error_message = e.what();
            return false;
        }
    }

    bool delete_statechain(const std::string& statechain_id) {
        auto database_connection_string = getDatabaseConnectionString();

        std::string error_message;
        pqxx::connection conn(database_connection_string);
        if (conn.is_open()) {

            std::string delete_comm =
                "DELETE FROM generated_public_key WHERE statechain_id = $1;";
            pqxx::work txn2(conn);

            txn2.exec_params(delete_comm, statechain_id);
            txn2.commit();

            conn.close();

            return true;
        } else {
            return false;
        }
    }

    // ── [REQ-56 / REQ-65] THE LEAF REGISTRY ──────────────────────────────────────────────────────

    bool record_signed_tx(const std::vector<unsigned char>& txid,
                          const std::string& statechain_id,
                          int sig_index,
                          std::string& error_message) {
        if (txid.size() != 32) {
            error_message = "txid must be 32 bytes";
            return false;
        }
        try {
            pqxx::connection conn(getDatabaseConnectionString());
            if (!conn.is_open()) { error_message = "db closed"; return false; }
            pqxx::work txn(conn);
            // DO NOTHING rather than DO UPDATE: the first sid to be recorded for a txid wins. The
            // retry path re-presents the same transaction under the same sid, so the conflict is
            // benign; but if two DIFFERENT sids ever claimed one txid, silently overwriting would
            // let the second rewrite an edge the first already established.
            txn.exec_params(
                "INSERT INTO se_signed_tx (txid, statechain_id, sig_index) VALUES ($1, $2, $3) "
                "ON CONFLICT (txid) DO NOTHING;",
                pqxx::binarystring(txid.data(), txid.size()), statechain_id, sig_index);
            txn.commit();
            return true;
        } catch (std::exception const& e) {
            error_message = e.what();
            return false;
        }
    }

    bool signed_tx_owner(const std::vector<unsigned char>& txid,
                         std::string& statechain_id,
                         bool& found,
                         std::string& error_message) {
        found = false;          // assigned on EVERY path
        statechain_id.clear();
        if (txid.size() != 32) {
            error_message = "txid must be 32 bytes";
            return false;
        }
        try {
            pqxx::connection conn(getDatabaseConnectionString());
            if (!conn.is_open()) { error_message = "db closed"; return false; }
            pqxx::work txn(conn);
            auto rows = txn.exec_params(
                "SELECT statechain_id FROM se_signed_tx WHERE txid = $1;",
                pqxx::binarystring(txid.data(), txid.size()));
            if (!rows.empty()) {
                statechain_id = rows[0][0].as<std::string>();
                found = true;
            }
            txn.commit();
            return true;
        } catch (std::exception const& e) {
            error_message = e.what();
            return false;
        }
    }

    bool establish_leaf(const std::string& statechain_id,
                        const std::string& parent_statechain_id,
                        const std::string& root_statechain_id,
                        int64_t fund_value,
                        const std::vector<unsigned char>& exit_key,
                        std::string& error_message) {
        // Checked, not asserted: a short key would be stored and later compared against a 32-byte
        // output key, silently never matching — an obligation that can never be discharged.
        if (exit_key.size() != 32) {
            error_message = "exit_key must be 32 bytes";
            return false;
        }
        if (fund_value <= 0) {
            error_message = "fund_value must be positive";
            return false;
        }
        try {
            pqxx::connection conn(getDatabaseConnectionString());
            if (!conn.is_open()) { error_message = "db closed"; return false; }
            pqxx::work txn(conn);
            // ON CONFLICT DO NOTHING: a retried establishment must not add a second row. Two rows
            // for one leaf would be counted twice by the frontier and `C` would overpay — which is
            // the operator's loss rather than a holder's, but it is still a wrong answer from a
            // predicate whose whole job is to be exact.
            txn.exec_params(
                "INSERT INTO se_leaf (statechain_id, parent_statechain_id, root_statechain_id, "
                "fund_value, exit_key) VALUES ($1, $2, $3, $4, $5) "
                "ON CONFLICT (statechain_id) DO NOTHING;",
                statechain_id,
                parent_statechain_id.empty() ? pqxx::zview() : pqxx::zview(parent_statechain_id),
                root_statechain_id,
                fund_value,
                pqxx::binarystring(exit_key.data(), exit_key.size()));
            txn.commit();
            return true;
        } catch (std::exception const& e) {
            error_message = e.what();
            return false;
        }
    }

    bool load_leaves(const std::string& root_statechain_id,
                     std::vector<registry::Leaf>& out,
                     std::string& error_message) {
        out.clear();
        try {
            pqxx::connection conn(getDatabaseConnectionString());
            if (!conn.is_open()) { error_message = "db closed"; return false; }
            pqxx::work txn(conn);
            auto rows = txn.exec_params(
                "SELECT statechain_id, COALESCE(parent_statechain_id, ''), fund_value, exit_key, "
                "released FROM se_leaf WHERE root_statechain_id = $1 ORDER BY statechain_id;",
                root_statechain_id);
            for (const auto& r : rows) {
                registry::Leaf l;
                l.statechain_id = r[0].as<std::string>();
                l.parent_statechain_id = r[1].as<std::string>();
                l.fund_value = static_cast<uint64_t>(r[2].as<int64_t>());
                pqxx::binarystring key(r[3]);
                l.exit_key.assign(key.data(), key.data() + key.size());
                l.released = r[4].as<bool>();
                out.push_back(std::move(l));
            }
            txn.commit();
            return true;
        } catch (std::exception const& e) {
            error_message = e.what();
            return false;
        }
    }

    bool mark_released(const std::string& statechain_id, std::string& error_message) {
        try {
            pqxx::connection conn(getDatabaseConnectionString());
            if (!conn.is_open()) { error_message = "db closed"; return false; }
            pqxx::work txn(conn);
            // Monotone by construction: only ever set to true, never back. A release that could be
            // revoked would let an operator un-discharge a holder who already gave up their old leaf.
            txn.exec_params("UPDATE se_leaf SET released = true WHERE statechain_id = $1;",
                            statechain_id);
            txn.commit();
            return true;
        } catch (std::exception const& e) {
            error_message = e.what();
            return false;
        }
    }

    bool consume_release_nonce(const std::string& statechain_id,
                               const std::vector<unsigned char>& nonce,
                               std::string& error_message) {
        if (nonce.size() != 32) {
            error_message = "nonce must be 32 bytes";
            return false;
        }
        try {
            pqxx::connection conn(getDatabaseConnectionString());
            if (!conn.is_open()) { error_message = "db closed"; return false; }
            pqxx::work txn(conn);
            // The single-use rule is the PRIMARY KEY, not a SELECT-then-INSERT: a check-then-act
            // pair races against a concurrent replay of the same nonce, and the whole point of the
            // nonce is to survive an adversary who retries.
            auto res = txn.exec_params(
                "INSERT INTO se_release_nonce (statechain_id, nonce) VALUES ($1, $2) "
                "ON CONFLICT DO NOTHING;",
                statechain_id, pqxx::binarystring(nonce.data(), nonce.size()));
            txn.commit();
            if (res.affected_rows() == 0) {
                error_message = "nonce already used for this statechain";
                return false;
            }
            return true;
        } catch (std::exception const& e) {
            error_message = e.what();
            return false;
        }
    }

    bool freeze_root(const std::string& root_statechain_id,
                     const std::string& successor_root,
                     std::string& error_message) {
        try {
            pqxx::connection conn(getDatabaseConnectionString());
            if (!conn.is_open()) { error_message = "db closed"; return false; }
            pqxx::work txn(conn);
            // Irreversible: the UPDATE branch only ever sets frozen = true. There is deliberately no
            // path that clears it — INV-FREEZE is a ratchet, and a clearable freeze is not one.
            txn.exec_params(
                "INSERT INTO se_root (root_statechain_id, frozen, successor_root) "
                "VALUES ($1, true, $2) "
                "ON CONFLICT (root_statechain_id) DO UPDATE SET frozen = true, "
                "successor_root = COALESCE(EXCLUDED.successor_root, se_root.successor_root);",
                root_statechain_id,
                successor_root.empty() ? pqxx::zview() : pqxx::zview(successor_root));
            txn.commit();
            return true;
        } catch (std::exception const& e) {
            error_message = e.what();
            return false;
        }
    }

    bool is_root_frozen(const std::string& root_statechain_id, bool& frozen,
                        std::string& error_message) {
        frozen = false;  // assigned on EVERY path: an unknown root is not frozen, and a caller must
                         // never read an uninitialised value and act on it.
        try {
            pqxx::connection conn(getDatabaseConnectionString());
            if (!conn.is_open()) { error_message = "db closed"; return false; }
            pqxx::work txn(conn);
            auto rows = txn.exec_params(
                "SELECT frozen FROM se_root WHERE root_statechain_id = $1;", root_statechain_id);
            if (!rows.empty()) frozen = rows[0][0].as<bool>();
            txn.commit();
            return true;
        } catch (std::exception const& e) {
            error_message = e.what();
            return false;
        }
    }
} // namespace db_manager