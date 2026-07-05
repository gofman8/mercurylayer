#include "server.h"
#include <crow.h>
#include <openssl/rand.h>
#include "utils.h"
#include "enclave.h"
#include "google_key_manager.h"
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

            bool sig_count_updated = db_manager::update_sig_count(statechain_id);
            if (!sig_count_updated) {
                return crow::response(500, "Failed to update signature count!");
            }

            auto partial_sig_hex = utils::key_to_string(response.partial_sig_data, sizeof(response.partial_sig_data));

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

            std::cout << "Using Google KMS key manager" << std::endl;

            seed = key_manager::get_seed();
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
        ([](std::string statechain_id){

            int sig_count;
            std::string error_message;
            bool count_retrieved = db_manager::signature_count(statechain_id, sig_count);

            if (!count_retrieved) {
                error_message = "Failed to retrieve signature count: " + error_message;
                return crow::response(500, error_message);
            }

            crow::json::wvalue result({{"sig_count", sig_count}});
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