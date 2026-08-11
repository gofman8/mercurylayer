#include "server.h"
#include <crow.h>
#include <openssl/rand.h>
#include <openssl/sha.h>
#include "utils.h"
#include "enclave.h"
#ifdef WITH_GOOGLE_KMS
#include "google_key_manager.h"
#endif
#include "hashicorp_api_key_manager.h"
#include "hashicorp_container_key_manager.h"
#include "filesystem_key_manager.h"
#include "db_manager.h"
#include <toml++/toml.h>
#include <chrono>
#include <thread>

namespace lockbox {

    crow::response generate_new_keypair(const std::string& statechain_id,  unsigned char *seed) {
        auto new_key_pair_response = enclave::generate_new_keypair(seed);

        std::string error_message;
        bool data_saved = db_manager::save_generated_public_key(
            new_key_pair_response.encrypted_data, 
            new_key_pair_response.server_pubkey, 
            sizeof(new_key_pair_response.server_pubkey), 
            statechain_id, 
            error_message);

        if (!data_saved) {
            error_message = "Failed to save aggregated key data: " + error_message;
            return crow::response(500, error_message);
        }

        std::string server_pubkey_hex = utils::key_to_string(new_key_pair_response.server_pubkey, 33);

        crow::json::wvalue result({{"server_pubkey", server_pubkey_hex}});
        return crow::response{result};
    }

    crow::response generate_public_nonce(const std::string& statechain_id,  unsigned char *seed) {

        auto encrypted_keypair = std::make_unique<utils::chacha20_poly1305_encrypted_data>();

        // the secret nonce is not defined yet
        auto encrypted_secnonce = std::make_unique<utils::chacha20_poly1305_encrypted_data>();
        encrypted_secnonce.reset();

        std::string error_message;
        bool data_loaded = db_manager::load_generated_key_data(
            statechain_id,
            encrypted_keypair,
            encrypted_secnonce,
            nullptr,
            0,
            error_message
        );

        assert(encrypted_secnonce == nullptr);

        if (!data_loaded) {
            error_message = "Failed to load aggregated key data: " + error_message;
            return crow::response(500, error_message);
        }

        auto response = enclave::generate_nonce(seed, encrypted_keypair.get());

        bool data_saved = db_manager::update_sealed_secnonce(
            statechain_id,
            response.server_pubnonce, sizeof(response.server_pubnonce),
            response.encrypted_secnonce,
            error_message
        );

        if (!data_saved) {
            error_message = "Failed to save sealed secret nonce: " + error_message;
            return crow::response(500, error_message);
        }

        auto serialized_server_pubnonce_hex = utils::key_to_string(response.server_pubnonce, sizeof(response.server_pubnonce));

        crow::json::wvalue result({{"server_pubnonce", serialized_server_pubnonce_hex}});
        return crow::response{result};
    }

    crow::response generate_partial_signature(
        const std::string& statechain_id, 
        int64_t negate_seckey, 
        std::vector<unsigned char>& serialized_session,
        unsigned char *seed) {

            auto encrypted_keypair = std::make_unique<utils::chacha20_poly1305_encrypted_data>();
            auto encrypted_secnonce = std::make_unique<utils::chacha20_poly1305_encrypted_data>();

            unsigned char serialized_server_pubnonce[66];
            memset(serialized_server_pubnonce, 0, sizeof(serialized_server_pubnonce));

            std::string error_message;

            // [KEYSTONE / retry-safety] Idempotent signing-round cache — checked BEFORE consuming the
            // secnonce. The session bytes are the client's exact message commitment, so an identical
            // sign/second retry (same session) is served the SAME partial sig from cache: no re-sign, no
            // re-increment of sig_count. This closes the window where a lost sign/second RESPONSE would
            // otherwise leave sig_count advanced with no tier on the client, desynchronising the receiver
            // census and bricking the coin. A DIFFERENT session after the secnonce is consumed falls
            // through to the guard below and 400s, so the nonce-reuse defence is unchanged.
            std::string session_key = utils::key_to_string(serialized_session.data(), serialized_session.size());
            std::string cached_partial_sig;
            if (db_manager::get_cached_partial_sig(statechain_id, session_key, cached_partial_sig)) {
                crow::json::wvalue cached_result({{"partial_sig", cached_partial_sig}});
                return crow::response{cached_result};
            }

            // Atomically load AND consume the sealed secnonce (row-locked, nulled in the same txn).
            // This enforces one-signature-per-secnonce AT THE ENCLAVE: a second partial-signature
            // request for the same statechain_id — whether racing this one or arriving after — finds
            // the secnonce already NULL and is refused below. That closes the MuSig2 nonce-reuse key
            // leak that the server-side per-nonce challenge binding alone cannot (two concurrent
            // sign/first calls yield two pubnonce rows mapping to one sealed secnonce).
            bool data_loaded = db_manager::load_and_consume_secnonce(
                statechain_id,
                encrypted_keypair,
                encrypted_secnonce,
                serialized_server_pubnonce,
                sizeof(serialized_server_pubnonce),
                error_message
            );

            if (!data_loaded) {
                error_message = "Failed to load aggregated key data: " + error_message;
                return crow::response(500, error_message);
            }

            bool is_sealed_keypair_empty = encrypted_keypair == nullptr;
            bool is_sealed_secnonce_empty = encrypted_secnonce == nullptr;

            if (is_sealed_keypair_empty || is_sealed_secnonce_empty) {
                // Secnonce already consumed (or never generated): refuse rather than sign again.
                // Signing a second time with the same secnonce over a different challenge would
                // leak the SE key share. The client must call sign/first for a fresh nonce.
                return crow::response(400, "Empty sealed keypair or sealed secnonce (secnonce already consumed — refusing to prevent nonce reuse)!");
            }

            auto response = enclave::partial_signature(
                seed, 
                encrypted_keypair.get(),
                encrypted_secnonce.get(),
                (int) negate_seckey,
                serialized_session.data(), serialized_session.size(),
                serialized_server_pubnonce);

            auto partial_sig_hex = utils::key_to_string(response.partial_sig_data, sizeof(response.partial_sig_data));

            // [KEYSTONE] Cache the produced sig AND increment sig_count ATOMICALLY (both or neither),
            // replacing the standalone update_sig_count. Ordering matters for retry-safety:
            //   - crash AFTER this commit, response lost  ⟹ retry hits the cache above, gets this exact
            //     sig; sig_count counted exactly once. SAFE.
            //   - crash BETWEEN the secnonce consume and this commit ⟹ no cache row AND no increment, so
            //     the client sees a failure, restarts sign/first (mints a fresh secnonce, overwriting the
            //     consumed one) and re-signs; the count advances once for the one tier it ends up with.
            //     No phantom increment ⟹ no desync. SAFE.
            std::string store_error;
            bool stored = db_manager::store_partial_sig_and_increment(
                statechain_id, session_key, partial_sig_hex, store_error);
            if (!stored) {
                return crow::response(500, "Failed to persist signature count: " + store_error);
            }

            crow::json::wvalue result({{"partial_sig", partial_sig_hex}});
            return crow::response{result};
    }

    crow::response keyupdate(
        const std::string& statechain_id, 
        std::vector<unsigned char>& serialized_t2,
        std::vector<unsigned char>& serialized_x1,
        unsigned char *seed) {

            auto old_encrypted_keypair = std::make_unique<utils::chacha20_poly1305_encrypted_data>();
        
            // the secret nonce is not used here
            auto encrypted_secnonce = std::make_unique<utils::chacha20_poly1305_encrypted_data>();
            encrypted_secnonce.reset();

            std::string error_message;
            bool data_loaded = db_manager::load_generated_key_data(
                statechain_id,
                old_encrypted_keypair,
                encrypted_secnonce,
                nullptr,
                0,
                error_message
            );

            if (!data_loaded) {
                error_message = "Failed to load aggregated key data: " + error_message;
                return crow::response(500, error_message);
            }

            if (old_encrypted_keypair == nullptr) {
                return crow::response(400, "Empty encrypted keypair!");
            }

            auto response = enclave::key_update(
                seed, 
                old_encrypted_keypair.get(),
                serialized_x1.data(),
                serialized_t2.data());

            bool data_saved = db_manager::update_sealed_keypair(
                response.encrypted_data, 
                response.server_pubkey, sizeof(response.server_pubkey),
                statechain_id, 
                error_message);

            if (!data_saved) {
                error_message = "Failed to update aggregated key data: " + error_message;
                return crow::response(500, error_message);
            }

            auto new_server_seckey_hex = utils::key_to_string(response.server_pubkey, sizeof(response.server_pubkey));

            crow::json::wvalue result({{"server_pubkey", new_server_seckey_hex}});
            return crow::response{result};
    }

    std::string getKeyManager() {
        return utils::getStringConfigVar(utils::KEY_MANAGER);
    }

    /**
     * Get the seed from the Hashicorp container key manager
     * This requires the container to be running
     * So this function is necessary to wait the container to be ready
     */
    std::vector<uint8_t> getHashicorpContainerSeed() {
        const auto start_time = std::chrono::steady_clock::now();
        const auto timeout_duration = std::chrono::minutes(3);
        
        while (true) {
            try {
                return hashicorp_container_key_manager::get_seed();
            } catch (const std::runtime_error& e) {
                auto current_time = std::chrono::steady_clock::now();
                if (current_time - start_time >= timeout_duration) {
                    throw std::runtime_error("Failed to get Hashicorp container seed after 3 minutes of retries");
                }
                
                std::this_thread::sleep_for(std::chrono::seconds(5));
            }
        }
    }

    void start_server() {

        std::vector<uint8_t> seed;

        auto key_provider = getKeyManager();

        if (key_provider == "filesystem") {

            std::cout << "Using filesystem key manager" << std::endl;

            seed = filesystem_key_manager::get_seed();
        } else if (key_provider == "google_kms") {

#ifdef WITH_GOOGLE_KMS
            std::cout << "Using Google KMS key manager" << std::endl;

            seed = key_manager::get_seed();
#else
            // FAIL LOUD. Silently falling through to another key manager would hand the enclave a
            // DIFFERENT SEED than the operator asked for — every sealed keypair would decrypt to
            // garbage, or worse, a fresh seed would be minted and the existing coins orphaned.
            throw std::runtime_error(
                "KEY_MANAGER=google_kms but this binary was built without Google KMS support. "
                "Rebuild with -DWITH_GOOGLE_KMS=ON and the 'google-kms' vcpkg feature, or select a "
                "different KEY_MANAGER.");
#endif
        } else if (key_provider == "hashicorp_api") {

            std::cout << "Using Hashicorp API key manager" << std::endl;

            seed = hashicorp_api_key_manager::get_seed();
        } else if (key_provider == "hashicorp_container") {

            std::cout << "Using Hashicorp container key manager" << std::endl;

            seed = getHashicorpContainerSeed();
        } else {
            throw std::runtime_error("Invalid key manager: " + key_provider);
        }

        /* std::string seed_hex = utils::key_to_string(seed.data(), seed.size());

        std::cout << "Seed:       " << seed_hex << std::endl; */

        // Initialize Crow HTTP server
        crow::SimpleApp app;

        // Define a simple route
        CROW_ROUTE(app, "/")([](){
            return "Hello, Crow!";
        });

        CROW_ROUTE(app, "/get_public_key")
        .methods("POST"_method)([&seed](const crow::request& req) {

            auto req_body = crow::json::load(req.body);
            if (!req_body)
                return crow::response(400);

            if (req_body.count("statechain_id") == 0)
                return crow::response(400, "Invalid parameter. It must be 'statechain_id'.");

            std::string statechain_id = req_body["statechain_id"].s();

            return generate_new_keypair(statechain_id, seed.data());
        });

        CROW_ROUTE(app, "/get_public_nonce")
        .methods("POST"_method)([&seed](const crow::request& req) {

            auto req_body = crow::json::load(req.body);
            if (!req_body)
                return crow::response(400);

            if (req_body.count("statechain_id") == 0) {
                return crow::response(400, "Invalid parameters. They must be 'statechain_id'.");
            }

            std::string statechain_id = req_body["statechain_id"].s();

            return generate_public_nonce(statechain_id, seed.data());
        });

        CROW_ROUTE(app, "/get_partial_signature")
            .methods("POST"_method)([&seed](const crow::request& req) {

                auto req_body = crow::json::load(req.body);
                if (!req_body)
                    return crow::response(400);

                if (req_body.count("statechain_id") == 0 || 
                    req_body.count("negate_seckey") == 0 ||
                    req_body.count("session") == 0) {
                    return crow::response(400, "Invalid parameters. They must be 'statechain_id', 'negate_seckey' and 'session'.");
                }

                std::string statechain_id = req_body["statechain_id"].s();
                int64_t negate_seckey = req_body["negate_seckey"].i();
                std::string session_hex = req_body["session"].s();


                if (session_hex.substr(0, 2) == "0x") {
                    session_hex = session_hex.substr(2);
                }

                std::vector<unsigned char> serialized_session = utils::ParseHex(session_hex);

                if (serialized_session.size() != 133) {
                    return crow::response(400, "Invalid session length. Must be 133 bytes!");
                }

                return generate_partial_signature(statechain_id, negate_seckey, serialized_session, seed.data());
        
        });

        CROW_ROUTE(app,"/signature_count/<string>")
        ([&seed](const crow::request& req, std::string statechain_id){

            int sig_count;
            std::string error_message;
            bool count_retrieved = db_manager::signature_count(statechain_id, sig_count);

            if (!count_retrieved) {
                error_message = "Failed to retrieve signature count: " + error_message;
                return crow::response(500, error_message);
            }

            // [D8] ATTEST THE COUNT. Returning it bare is what let a coordinator under-report it by
            // k and hide k co-signed rival states while the receiver's exact-equality census still
            // balanced — theft, resting on coordinator honesty alone. The attestation is signed by
            // THIS coin's server key, whose public half the receiver already binds to the on-chain
            // tx0 output, so verification needs nothing the client does not already hold.
            //
            // PREIMAGE, defined here and mirrored exactly by the verifier:
            //     sha256("utexo/sig_count/v1" || statechain_id || u32_be(sig_count) || nonce32)
            //
            // `nonce` is a 32-byte hex challenge the CLIENT chooses and passes as a query parameter.
            // Without it the attestation is a static value: a coordinator could serve a genuine
            // signature captured when the count was legitimately lower, and replay it forever. The
            // nonce makes each attestation answer one specific question asked once. A caller that
            // omits it gets the count unattested rather than a forgeable-looking one — the field is
            // simply absent, so a verifier can tell "not attested" from "attested".
            auto nonce_hex = req.url_params.get("nonce");

            crow::json::wvalue result;
            result["sig_count"] = sig_count;

            if (nonce_hex != nullptr) {
                std::string nonce_str(nonce_hex);
                if (nonce_str.size() != 64) {
                    return crow::response(400, "nonce must be 32 bytes of hex (64 characters)");
                }
                auto nonce_bytes = utils::ParseHex(nonce_str);
                if (nonce_bytes.size() != 32) {
                    return crow::response(400, "nonce must decode to exactly 32 bytes");
                }

                auto encrypted_keypair = std::make_unique<utils::chacha20_poly1305_encrypted_data>();
                auto encrypted_secnonce = std::make_unique<utils::chacha20_poly1305_encrypted_data>();
                encrypted_secnonce.reset();

                bool data_loaded = db_manager::load_generated_key_data(
                    statechain_id, encrypted_keypair, encrypted_secnonce, nullptr, 0, error_message);

                if (!data_loaded) {
                    return crow::response(500, "Failed to load key data for attestation: " + error_message);
                }

                // Build the digest. `sig_count` is serialised big-endian and fixed-width so the
                // preimage is unambiguous — a length-varying decimal string would let two different
                // (id, count) pairs collide under concatenation.
                std::string domain = "utexo/sig_count/v1";
                std::vector<unsigned char> preimage(domain.begin(), domain.end());
                preimage.insert(preimage.end(), statechain_id.begin(), statechain_id.end());
                uint32_t c = static_cast<uint32_t>(sig_count);
                preimage.push_back((c >> 24) & 0xff);
                preimage.push_back((c >> 16) & 0xff);
                preimage.push_back((c >> 8) & 0xff);
                preimage.push_back(c & 0xff);
                preimage.insert(preimage.end(), nonce_bytes.begin(), nonce_bytes.end());

                unsigned char digest[32];
                SHA256(preimage.data(), preimage.size(), digest);

                try {
                    auto att = enclave::attest_sig_count(seed.data(), encrypted_keypair.get(), digest);
                    result["attestation"] = utils::key_to_string(att.signature, sizeof(att.signature));
                    result["attestation_pubkey"] = utils::key_to_string(att.xonly_pubkey, sizeof(att.xonly_pubkey));
                    result["attestation_nonce"] = nonce_str;
                } catch (std::exception const &e) {
                    return crow::response(500, std::string("Failed to attest signature count: ") + e.what());
                }
            }

            return crow::response{result};
        });

        CROW_ROUTE(app, "/keyupdate")
            .methods("POST"_method)([&seed](const crow::request& req) {
                
                auto req_body = crow::json::load(req.body);
                if (!req_body)
                    return crow::response(400);

                if (req_body.count("statechain_id") == 0 || 
                    req_body.count("t2") == 0 ||
                    req_body.count("x1") == 0) {
                    return crow::response(400, "Invalid parameters. They must be 'statechain_id', 't2' and 'x1'.");
                }

                std::string statechain_id = req_body["statechain_id"].s();
                std::string t2_hex = req_body["t2"].s();
                std::string x1_hex = req_body["x1"].s();

                if (t2_hex.substr(0, 2) == "0x") {
                    t2_hex = t2_hex.substr(2);
                }

                std::vector<unsigned char> serialized_t2 = utils::ParseHex(t2_hex);

                if (serialized_t2.size() != 32) {
                    return crow::response(400, "Invalid t2 length. Must be 32 bytes!");
                }

                if (x1_hex.substr(0, 2) == "0x") {
                    x1_hex = x1_hex.substr(2);
                }

                std::vector<unsigned char> serialized_x1 = utils::ParseHex(x1_hex);

                if (serialized_x1.size() != 32) {
                    return crow::response(400, "Invalid x1 length. Must be 32 bytes!");
                }

                return keyupdate(statechain_id, serialized_t2, serialized_x1, seed.data());
        });

        CROW_ROUTE(app,"/delete_statechain/<string>")
            .methods("DELETE"_method)([](std::string statechain_id){
                if (db_manager::delete_statechain(statechain_id)) {
                    return crow::response(200, "Statechain deleted.");
                } else {
                    return crow::response(500, "Failed to connect to the database and delete statechain.");
                }
        });

        uint16_t server_port = 0;

        try {
            server_port = utils::getServerPort();
        } catch (const std::exception& e) {
            throw std::runtime_error("Failed to get enclave port");
        }
        
        app.port(server_port).multithreaded().run();
    }
} // namespace lockbox