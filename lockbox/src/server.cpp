#include <algorithm>
#include "server.h"
#include "../include/auth.h"
#include "../include/witness.h"
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
#include <cstdlib>
#include <iostream>
#include <chrono>
#include <thread>

namespace lockbox {

    crow::response generate_new_keypair(const std::string& statechain_id,  unsigned char *seed,
                                        const std::optional<std::vector<unsigned char>>& client_pubkey) {
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

        // ── [REQ-68] DERIVE the coin's aggregate here, from the client's key and the share just
        // minted. Operator decision 2026-08-17: the SE computes it and never accepts one, because
        // the party that would supply it is the party REQ-56's frontier is checked against.
        //
        // Optional while clients migrate. A coin with no stored aggregate simply is not checked
        // (D24 already ignores legacy coins); refusing here would brick every pre-existing coin and
        // every un-migrated client on deploy.
        if (client_pubkey.has_value()) {
            std::vector<unsigned char> server_pub(
                new_key_pair_response.server_pubkey,
                new_key_pair_response.server_pubkey + 33);
            const auto aggregate = auth::derive_aggregate_xonly(*client_pubkey, server_pub);
            if (!aggregate) {
                // The client key did not parse or the arithmetic failed. Refuse the KEYGEN rather
                // than create a coin the SE can never check — this is the one place where failing
                // closed costs nothing, because no coin exists yet.
                return crow::response(400, "client public key is not a valid 33-byte point");
            }
            std::string agg_err;
            if (!db_manager::store_aggregate(statechain_id, *aggregate, agg_err)) {
                return crow::response(500, "could not store the derived aggregate: " + agg_err);
            }
            CROW_LOG_INFO << "AGGREGATE_DERIVED statechain " << statechain_id;
        }

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
        unsigned char *seed,
        const std::optional<witness::Disclosure>& disclosure,
        const std::optional<std::vector<unsigned char>>& latch_sig) {

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

            // ── [REQ-57] WITNESS BINDING ─────────────────────────────────────────────────────────
            //
            // If the caller disclosed the transaction, the SE reproduces the sighash and rebuilds the
            // blinded session from it, then byte-compares against the 133 bytes it was asked to sign.
            // A disclosure that does not reproduce the session is refused.
            //
            // Placed with the other pre-consumption gates and BEFORE the secnonce is touched, so a
            // refusal costs the coin nothing and can be retried with a correct disclosure.
            //
            // Absent disclosure is NOT refused here: binding is opt-in per request while clients are
            // migrated. Routes that must have it enforce that themselves — a blanket requirement at
            // this layer would brick every existing client on deploy.
            //
            // ── [REQ-61] LATCH ENFORCEMENT ──────────────────────────────────────────────────────
            //
            // Once a coin's latch is armed, a co-signature under that sid requires a fresh BIP-340
            // by the latch key over `tagged(LATCH_TAG, sid || session)`. The session is in the
            // message so the authorisation is for THIS round only — otherwise it is a bearer token
            // that can be replayed against any later transaction.
            //
            // WHY NO "EXEMPT COUNT" IS NEEDED. REQ-61(b) worried that the payer co-signs the payee's
            // tiers before the payee holds anything, so "every co-signature" could not mean what it
            // said. REQ-61(a2) dissolves it: the latch arms only from a tier that hands control
            // OUTWARD, which is the state tier — the LAST of the ladder. Establishment therefore
            // completes before the latch binds anything, and no arbitrary exempt count has to be
            // chosen or maintained.
            //
            // DEFAULT OFF. Enforcement refuses any client that does not yet sign, which is every
            // client today. `UTEXO_ENFORCE_LATCH=1` turns it on; the deploy order is: ship signing
            // in the clients, confirm every co-signature carries one, then flip. Shipping it on by
            // default would brick the system on upgrade, which is the same hazard REQ-57's opt-in
            // binding has and must be sequenced with it.
            const char* enforce_env = std::getenv("UTEXO_ENFORCE_LATCH");
            const bool enforce_latch = enforce_env != nullptr && std::string(enforce_env) == "1";
            if (enforce_latch) {
                std::vector<unsigned char> latch_key;
                bool has_latch = false;
                std::string latch_err;
                if (!db_manager::get_latch(statechain_id, latch_key, has_latch, latch_err)) {
                    return crow::response(500, "could not read the owner latch: " + latch_err);
                }
                if (has_latch) {
                    // A latched coin with no authorisation is refused — this is the whole gate.
                    if (!latch_sig.has_value()) {
                        return crow::response(
                            401,
                            "this coin is latched: a co-signature requires a BIP-340 authorisation "
                            "by the owner key over tagged(\"utexo/leaf_latch/v1\", sid || session)");
                    }
                    if (!auth::verify_latch(latch_key, statechain_id, serialized_session,
                                            *latch_sig)) {
                        return crow::response(
                            401, "owner authorisation does not verify under this coin's latch");
                    }
                    CROW_LOG_INFO << "LATCH_AUTHORISED statechain " << statechain_id;
                }
            }

            // Set only on a successful bind, and consumed only after the signature commits below.
            std::optional<std::vector<unsigned char>> bound_txid;
            std::optional<std::vector<unsigned char>> bound_latch_key;
            // [#157] The outpoint THIS rung spends, captured where the parsed transaction is in
            // scope. Taken from the parsed bytes — the ones whose sighash was recomputed and whose
            // session was byte-compared — so a caller cannot name one outpoint and sign another.
            std::optional<std::vector<unsigned char>> bound_prevout_txid;
            int64_t bound_prevout_value = 0;
            int64_t bound_prevout_vout = -1;
            if (disclosure.has_value()) {
                std::vector<unsigned char> bound_sighash;
                std::string detail;
                const auto r = witness::bind(*disclosure, serialized_session, &bound_sighash, &detail);
                if (r != witness::BindResult::Match) {
                    return crow::response(400, std::string("witness binding refused: ") + detail);
                }
                // ── [REQ-68] IS THIS TRANSACTION EVEN THIS COIN'S? ──────────────────────────────
                //
                // Binding proves the disclosure reproduces the session — and every input to that is
                // the CALLER's, so on its own it says nothing about which coin the transaction
                // belongs to. This is the check that closes it: the disclosed `agg_pubkey` must be
                // the aggregate the SE DERIVED for this sid at keygen.
                //
                // Without it, `se_signed_tx` records "the first sid that presented a valid binding"
                // rather than the owner, and a parent edge resolved through it could be claimed by
                // whoever registers a txid first.
                //
                // **[V-7] FAILS CLOSED.** This used to serve a disclosure for a coin with no stored
                // aggregate, on the reasoning that an old client sent no key and refusing would
                // brick every pre-existing coin. Two things changed and the reasoning with them:
                //
                //   * the coordinator now REFUSES an empty `user_public_key`, so no request can mint
                //     an unbindable coin any more — the unbound set is finite and closed, not
                //     growing;
                //   * every shipped client already sends the key (Rust, both wasm builds, Kotlin all
                //     go through `create_deposit_msg1`), so the set is HISTORICAL. My own V-7 note
                //     claimed the bindings predated the field; measuring the artifacts disproved it.
                //
                // What remains behind this branch is exactly the pre-0009 legacy population D24
                // already decided to ignore. Serving them is not "compatibility", it is a hole any
                // caller could aim at by naming a legacy sid — so refuse, and say which coin.
                {
                    std::vector<unsigned char> stored_agg;
                    bool has_agg = false;
                    std::string agg_err;
                    if (!db_manager::get_aggregate(statechain_id, stored_agg, has_agg, agg_err)) {
                        return crow::response(500, "could not read the coin's aggregate: " + agg_err);
                    }
                    if (!has_agg) {
                        CROW_LOG_WARNING << "AGGREGATE_ABSENT statechain " << statechain_id;
                        return crow::response(
                            403,
                            "this coin has no aggregate on record, so a disclosure about it cannot "
                            "be checked (REQ-68 / V-7). It predates the binding and is one of the "
                            "legacy coins D24 ignores; it can still be withdrawn, but it will not be "
                            "co-signed against an unverifiable disclosure.");
                    }
                    {
                        // The disclosure carries the 33-byte compressed tweaked key; its x-only is
                        // the 32 bytes after the parity prefix.
                        const auto disclosed = utils::ParseHex(disclosure->agg_pubkey_hex);
                        if (disclosed.size() != 33) {
                            return crow::response(400, "disclosure agg_pubkey is not 33 bytes");
                        }
                        const std::vector<unsigned char> disclosed_xonly(disclosed.begin() + 1,
                                                                        disclosed.end());
                        if (disclosed_xonly != stored_agg) {
                            CROW_LOG_WARNING << "AGGREGATE_MISMATCH statechain " << statechain_id;
                            return crow::response(
                                403,
                                "the disclosed transaction is not this coin's: agg_pubkey does not "
                                "match the aggregate derived for this statechain");
                        }
                    }
                }

                // Emitted so that "the honest path still works" can be told apart from "the gate never
                // ran". Both look identical from outside — an absent disclosure is served, so a client
                // that silently failed to serialise one would produce a GREEN end-to-end test while
                // binding nothing. sdk92 counts these lines across its honest half and requires > 0.
                CROW_LOG_INFO << "WITNESS_BIND_MATCH statechain " << statechain_id;

                // ── [REQ-56] COMPUTE the txid here; RECORD it only once a signature exists ───────
                //
                // Deliberately NOT written yet. An earlier version recorded at this point, which is
                // before the spend-budget gate (410) and before the secnonce is consumed (400) — so
                // a request that was ultimately REFUSED still left a permanent row. Measured, not
                // theorised: sdk92's [b2] probe took a 400 and its row was already committed.
                //
                // That made the index a free, caller-chosen, permanent write: combined with
                // `ON CONFLICT (txid) DO NOTHING`, whoever wrote a txid first owned it forever, at
                // no cost in budget or nonces. The row is now written after
                // `store_partial_sig_and_increment` commits, so a row means the SE actually produced
                // and counted a co-signature for these bytes — which costs the caller a real slot on
                // a coin it controls.
                const auto parsed_disclosure = tx::parse_hex(disclosure->unsigned_tx_hex);
                if (parsed_disclosure) {
                    bound_txid = tx::txid(*parsed_disclosure);
                    if (!parsed_disclosure->vin.empty()) {
                        const auto& po_in = parsed_disclosure->vin[0].prevout;
                        bound_prevout_txid =
                            std::vector<unsigned char>(po_in.txid, po_in.txid + 32);
                        bound_prevout_vout = static_cast<int64_t>(po_in.vout);
                    }
                    bound_prevout_value =
                        disclosure->prevout_values.empty()
                            ? 0
                            : static_cast<int64_t>(disclosure->prevout_values[0]);
                    // [REQ-61] The latch key, read STRUCTURALLY from the money: the unique P2TR
                    // output. Ambiguous or absent yields nothing and the latch simply is not armed
                    // by this signature — refusing here would brick any shape the SE does not yet
                    // model, and an un-armed latch fails OPEN in the same way the system behaved
                    // before it existed.
                    //
                    // BUT NOT EVERY TIER CARRIES A USABLE KEY, and arming from the wrong one is a
                    // latent brick. Of the tier builders in `lib/src/tesr.rs`, four pay
                    // `to_address` — the coin's own AGGREGATE (2-of-2) address — and only the state
                    // tiers pay `owner_address`, the holder's unilateral backup key. A latch armed
                    // to the aggregate could never be satisfied: signing under it needs the SE,
                    // which is the very thing the latch gates. The coin would be bricked the moment
                    // enforcement went on.
                    //
                    // The SE can tell them apart without being told, because it already has the
                    // prevout: a tier that pays BACK to the key it spends is staying in the 2-of-2,
                    // and a tier that pays ELSEWHERE is the one handing control to a unilateral
                    // owner. So arm only when the payload key differs from the input's key.
                    //
                    // Measured on the live lane before this guard existed: sdk92's latch armed to
                    // the backup key correctly — but only because a state tier happened to bind
                    // first. That is ordering luck, not a property.
                    if (const auto po = witness::payload_output(*parsed_disclosure)) {
                        const auto prevout_spk =
                            utils::ParseHex(disclosure->prevout_spks_hex.empty()
                                                ? std::string()
                                                : disclosure->prevout_spks_hex[0]);
                        const auto prevout_key = tx::p2tr_xonly_key(prevout_spk);
                        // **[#157] WHAT THIS RUNG ACTUALLY SHOWS THE SE.**
                        //
                        // REQ-56 needs two facts per leaf, and REQ-60 says they are NOT the same
                        // number: `exit_key` (REQ-65, the state tier's payload key) and `fund_value`
                        // (the FULL funding value, because the tier rungs are never broadcast so
                        // their burn is never realised). Reading both off ONE rung would silently
                        // record the exit value as the funding value and underpay every absentee in
                        // a collapse by the burn — the failure REQ-60 exists to name.
                        //
                        // So before wiring `establish_leaf`, log what each rung witnesses: the
                        // prevout value it spends, the payload value it pays, and whether it hands
                        // control onward. The tier chain is F -> T -> X -> S, so the funding value
                        // is visible only at the rung whose prevout IS `F`, while the exit key is
                        // visible only at the rung that pays elsewhere. This line is what turns that
                        // reasoning into a measurement.
                        const uint64_t prevout_value =
                            disclosure->prevout_values.empty() ? 0 : disclosure->prevout_values[0];
                        const bool hands_control_on = !prevout_key || *prevout_key != po->xonly;
                        CROW_LOG_INFO << "LEAF_OBSERVE statechain " << statechain_id
                                      << " prevout_value=" << prevout_value
                                      << " payload_value=" << po->value
                                      << " burn=" << (prevout_value > po->value
                                                          ? prevout_value - po->value : 0)
                                      << " hands_control_on=" << (hands_control_on ? 1 : 0)
                                      << " payload_key="
                                      << utils::key_to_string(po->xonly.data(), po->xonly.size());
                        if (hands_control_on) {
                            bound_latch_key = po->xonly;
                        } else {
                            CROW_LOG_INFO << "LATCH_SKIP_AGGREGATE statechain " << statechain_id
                                          << " (tier pays back to the key it spends)";
                        }
                    }
                } else {
                    // Unreachable in practice: `bind` parsed the same hex a moment ago. Logged
                    // rather than assumed, because "cannot happen" is how an index quietly goes
                    // incomplete.
                    CROW_LOG_WARNING << "WITNESS_BIND_INDEX_MISS statechain " << statechain_id
                                     << ": disclosure re-parse failed after a successful bind";
                }
            }

            // ── [D8(i)] THE SPEND BUDGET, ENFORCED **HERE**, NOT ONLY AT THE COORDINATOR ──────────
            //
            // The coordinator has always refused to co-sign past a coin's budget, and that is what
            // makes a split/combine node terminal. But a receiver's whole census argument rests on
            // that terminality, and the only witness to it was the coordinator — the party the
            // receiver is being protected FROM. The SE could attest the count (D8) but knew nothing
            // of a budget, so it could not attest that the count was FINAL.
            //
            // Now it can, because it enforces it. The check sits AFTER the idempotency cache on
            // purpose: a retry of a session already served must keep returning the same signature
            // even once the budget is exhausted, or the retry-safety keystone breaks and a lost
            // response bricks the coin. Only a NEW session consumes budget.
            //
            // It sits BEFORE the secnonce is consumed, so a refused request costs nothing.
            {
                int budget = 0;
                bool has_budget = false;
                if (!db_manager::get_sig_budget(statechain_id, budget, has_budget)) {
                    // Fail CLOSED. "I could not read the budget" must not mean "there isn't one" —
                    // that reading is exactly how a terminal node becomes re-signable.
                    return crow::response(500, "could not read the spend budget for this coin; refusing to co-sign");
                }
                if (has_budget) {
                    int sig_count = 0;
                    bool count_found = false;
                    if (!db_manager::signature_count(statechain_id, sig_count, count_found)) {
                        return crow::response(500, "could not read the signature count for this coin; refusing to co-sign");
                    }
                    // [D76] A coin that HAS a spend budget but no count row is inconsistent state.
                    // Reading the absent count as 0 would say "no signatures yet, go ahead" — the
                    // fail-OPEN reading, on the gate that enforces terminality. Refuse instead.
                    if (!count_found) {
                        return crow::response(500, "this coin has a spend budget but no signature count; refusing to co-sign");
                    }
                    if (sig_count >= budget) {
                        crow::json::wvalue exhausted;
                        exhausted["message"] =
                            "spend budget exhausted: this coin is TERMINAL and the enclave refuses "
                            "further co-signatures";
                        exhausted["sig_count"] = sig_count;
                        exhausted["sig_budget"] = budget;
                        return crow::response(410, exhausted.dump());
                    }
                }
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

            // ── [REQ-56] THE SE'S OWN INDEX — written only now that a signature EXISTS ───────────
            //
            // A child's tier spends `(SP.txid, j)`, and resolving that outpoint to a sid needs a
            // table only the SE can write. Written here, after `store_partial_sig_and_increment`
            // commits, so a row means "the SE produced and counted a co-signature over these bytes"
            // rather than merely "someone presented a self-consistent disclosure". Writing it at the
            // binding gate instead made the row free: budget and secnonce are checked AFTER that
            // point, so refused requests were still recorded permanently.
            //
            // A failure here does NOT fail the request: the signature is already produced, counted,
            // and about to be returned. Refusing now would tell the client its co-signature failed
            // while the count says otherwise — a desync far worse than a missing index row, which
            // surfaces later as a leaf whose parent will not resolve and which the predicate already
            // refuses on (SetError::ParentNotInSet).
            //
            // WHAT THIS ROW STILL DOES NOT MEAN. Binding proves the disclosed transaction reproduces
            // the session being signed; the SE never learns the coin's AGGREGATE key (it stores only
            // its own share), so it cannot check the session belongs to this coin. A row therefore
            // attests "signed under this sid", NOT "is a tier of this coin". Do not resolve a parent
            // edge through this table until that gap is closed.
            if (bound_txid) {
                std::string rec_err;
                if (!db_manager::record_signed_tx(*bound_txid, statechain_id, 0, rec_err)) {
                    CROW_LOG_WARNING << "WITNESS_BIND_INDEX_MISS statechain " << statechain_id
                                     << ": " << rec_err;
                }
            }

            // ── [REQ-56/#157] RECORD THE LEAF, from the facts THIS co-signature witnessed ───────
            //
            // Same placement and same reason as the index above: only a co-signature that was
            // actually produced leaves state behind. The two facts REQ-56 needs arrive on different
            // rungs (see `observe_leaf` for the measurement), so this is called on EVERY bound rung
            // and the database does the combining — the funding value ratchets UP, the exit key is
            // written once by the first rung that hands control onward.
            //
            // The parent edge is resolved from the outpoint this rung spends, through the SE's own
            // `se_signed_tx`. That became sound only when REQ-68 landed: before the SE derived the
            // coin's aggregate, a bound row meant "signed under this sid" rather than "is a tier of
            // this coin", and resolving parenthood through it would have let a caller graft a leaf
            // onto a tree it does not belong to.
            //
            // A failure here does NOT fail the request, for the same reason the index does not: the
            // signature is produced and counted, and refusing now would tell the client its
            // co-signature failed while the count says otherwise. A missing observation surfaces
            // later as a leaf the predicate refuses on, which is the safe direction.
            if (bound_txid) {
                std::vector<std::string> parent_sids;
                std::string owner_err;
                if (bound_prevout_txid) {
                    // ALL co-signers of the funding transaction (REQ-56a): a combine spends N coins,
                    // so this child has N parents and every one of them has stopped existing.
                    // Taking only the first leaves N-1 coins looking unspent and payable again.
                    if (!db_manager::signed_tx_owners(*bound_prevout_txid, parent_sids, owner_err)) {
                        CROW_LOG_WARNING << "LEAF_PARENT_MISS statechain " << statechain_id << ": "
                                         << owner_err;
                    }
                    // **A COIN IS NOT ITS OWN PARENT.** Caught by running this: the first live row
                    // came back with `parent_statechain_id = statechain_id`, because the tier chain
                    // `F -> T -> X -> S` is FOUR transactions of ONE coin, and the SE signed each of
                    // them under that same sid. Resolving the extension's prevout therefore finds
                    // the trigger — same coin, no parent edge.
                    //
                    // Left in, this is not cosmetic. The frontier is "every node that is not the
                    // parent of another", so a self-parented leaf is its own parent, drops out of
                    // the frontier, and `C` is never required to pay it. A holder would be
                    // discharged without being paid, which is the single outcome REQ-56 exists to
                    // make impossible — and REQ-67 leaves them no recourse afterwards.
                    parent_sids.erase(
                        std::remove(parent_sids.begin(), parent_sids.end(), statechain_id),
                        parent_sids.end());
                }
                const int64_t prevout_value = bound_prevout_value;
                if (prevout_value > 0) {
                    std::string leaf_err;
                    if (!db_manager::observe_leaf(statechain_id, prevout_value,
                                                  bound_latch_key ? *bound_latch_key
                                                                  : std::vector<unsigned char>(),
                                                  parent_sids,
                                                  bound_prevout_txid ? *bound_prevout_txid
                                                                     : std::vector<unsigned char>(),
                                                  bound_prevout_vout, leaf_err)) {
                        CROW_LOG_WARNING << "LEAF_OBSERVE_MISS statechain " << statechain_id << ": "
                                         << leaf_err;
                    }
                }
            }

            // ── [REQ-61] ARM THE OWNER LATCH, write-once ────────────────────────────────────────
            //
            // Armed here for the same reason the index is: only a co-signature that was actually
            // produced should leave state behind. NOT YET ENFORCED — nothing refuses a later
            // co-signature for want of a BIP-340 by this key. Arming first is deliberate: it lets
            // the key be observed on the real lane before anything depends on it, and enforcement
            // that arrives before the arming is proven correct would refuse honest payments.
            if (bound_latch_key) {
                bool armed_now = false;
                std::string latch_err;
                if (!db_manager::arm_latch(statechain_id, *bound_latch_key, armed_now, latch_err)) {
                    CROW_LOG_WARNING << "LATCH_ARM_MISS statechain " << statechain_id << ": "
                                     << latch_err;
                } else if (armed_now) {
                    CROW_LOG_INFO << "LATCH_ARMED statechain " << statechain_id;
                }
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

        // ── SCHEMA FIRST, AT BOOT ─────────────────────────────────────────────────────────────────
        //
        // The table used to be created lazily inside `save_generated_public_key`, i.e. on the first
        // DEPOSIT. That is fine for a `CREATE TABLE IF NOT EXISTS` on an empty database and wrong for
        // everything else: a column added to that statement never appears on a database that already
        // has the table, and the ALTER that would add it does not run until someone deposits.
        //
        // Found the hard way. The `sig_budget` column [D8(i)] was added to the CREATE and the
        // deployed lockbox went on answering `column "sig_budget" does not exist` — the migration was
        // sitting behind a code path nothing had called yet. Running it here means the process
        // either comes up with a schema it can serve, or fails loudly at boot.
        {
            std::string migrate_error;
            if (!db_manager::ensure_schema(migrate_error)) {
                std::cerr << "FATAL: the lockbox database schema could not be prepared: "
                          << migrate_error << std::endl;
                std::exit(1);
            }
        }

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

            // [REQ-68] Optional while clients migrate; strict when present.
            std::optional<std::vector<unsigned char>> client_pubkey;
            if (req_body.count("user_public_key") != 0) {
                auto parsed = utils::ParseHex(std::string(req_body["user_public_key"].s()));
                if (parsed.size() != 33) {
                    return crow::response(400, "user_public_key must be a 33-byte compressed key");
                }
                client_pubkey = parsed;
            }

            return generate_new_keypair(statechain_id, seed.data(), client_pubkey);
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

                // **[REQ-57 / #162] MANDATORY. The SE does not sign what it has not been shown.**
                //
                // This was optional while clients migrated, and the instrumentation added to measure
                // that migration is what closed it: WITNESS_BIND_ABSENT counted the unbound requests
                // by name, and across the live suite (the laddered lane, the coloured multi-input
                // combine, the flat lane) the count reached ZERO while WITNESS_BIND_MATCH reached
                // 118. Every client in this repo builds a disclosure, because they all forward
                // `PartialSignatureMsg1::partial_signature_request_payload` wholesale and mercurylib
                // populates it — the JS clients included.
                //
                // An optional security gate is not a security gate: a signer that accepts an unbound
                // request will sign blind for whoever omits the field, and "all our clients send it"
                // is a statement about clients, not about what the SE will do. The refusal is what
                // makes REQ-57 a property of the SE rather than a convention among callers.
                //
                // Both failure shapes refuse, and they stay DISTINCT: absent is a caller that has
                // not been migrated, malformed is a caller that has — one is a deployment fact and
                // the other is a bug, and merging them would hide whichever is rarer.
                std::optional<witness::Disclosure> disclosure;
                if (req_body.count("disclosure") == 0) {
                    CROW_LOG_WARNING << "WITNESS_BIND_ABSENT statechain " << statechain_id;
                    return crow::response(
                        400,
                        "this request carries no witness disclosure, so the SE cannot verify what it "
                        "would be signing. Refusing to co-sign blind. Rebuild the client against a "
                        "mercurylib that populates `disclosure` (every in-repo client does) and retry.");
                }
                disclosure = witness::parse_disclosure(req.body);
                if (!disclosure.has_value()) {
                    CROW_LOG_WARNING << "WITNESS_BIND_MALFORMED statechain " << statechain_id;
                    return crow::response(400, "disclosure present but malformed");
                }

                // [REQ-61] The owner's authorisation for THIS signing round. Optional on the wire
                // while clients migrate; strict when present, for the same reason the disclosure is:
                // "I could not read it" must never be treated as "there wasn't one".
                std::optional<std::vector<unsigned char>> latch_sig;
                if (req_body.count("latch_sig") != 0) {
                    auto parsed_sig = utils::ParseHex(std::string(req_body["latch_sig"].s()));
                    if (parsed_sig.size() != 64) {
                        return crow::response(400, "latch_sig present but not 64 bytes");
                    }
                    latch_sig = parsed_sig;
                }

                return generate_partial_signature(statechain_id, negate_seckey, serialized_session, seed.data(), disclosure, latch_sig);
        
        });

        // [D8(i)] Set a coin's spend budget. MONOTONE — see db_manager::set_sig_budget: a budget may
        // be created or LOWERED, never raised. The coordinator calls this alongside its own
        // `set_sig_budget` so the SE can enforce, and therefore attest, terminality itself.
        CROW_ROUTE(app, "/sig_budget")
            .methods("POST"_method)([](const crow::request& req) {

                auto req_body = crow::json::load(req.body);
                if (!req_body)
                    return crow::response(400);

                if (req_body.count("statechain_id") == 0 || req_body.count("sig_budget") == 0) {
                    return crow::response(400, "Invalid parameters. They must be 'statechain_id' and 'sig_budget'.");
                }

                std::string statechain_id = req_body["statechain_id"].s();
                int64_t budget = req_body["sig_budget"].i();

                if (budget < 0 || budget > INT32_MAX) {
                    return crow::response(400, "sig_budget out of range");
                }

                std::string error_message;
                if (!db_manager::set_sig_budget(statechain_id, static_cast<int>(budget), error_message)) {
                    // 409, not 500: a refused RAISE is the ratchet working, not a failure of this
                    // server, and a caller must be able to tell those apart.
                    bool is_raise = error_message.find("refusing to RAISE") != std::string::npos;
                    return crow::response(is_raise ? 409 : 500, error_message);
                }

                crow::json::wvalue result;
                result["message"] = "Success";
                result["sig_budget"] = budget;
                return crow::response{result};
        });

        // ═══ [D69] PUBLISH THE ATTESTATION IDENTITY, so a client can PIN it ═══
        //
        // Read-only, no secret leaves: the x-only PUBLIC half of the enclave's long-term attestation
        // key. An operator curls this once and compiles the value into the client
        // (`lib/src/tesr.rs::attestation_identity_const`).
        //
        // The route exists so that the pinning step is a documented operation rather than folklore,
        // and so a regtest/CI wallet — which faces a lockbox with a freshly generated seed and can
        // have no compiled-in pin — has one defined place to read it from.
        //
        // ⚠️ Serving it here does NOT make it a trust anchor. A client that fetched this key at
        // verification time and then verified against it would be checking a signature against a key
        // from the same party in the same conversation, which proves nothing. It is an anchor only
        // once it is pinned OUT OF BAND, which is the whole point of D69 option (a).
        // ── [REQ-54 R2] /release — the SE's FIRST authenticated route ────────────────────────────
        //
        // A leaf's holder consents to being discharged off-chain by signing
        // `tagged("utexo/leaf_release/v1", sid || nonce32)` under the coin's LATCH key — the key
        // read from the money itself at establishment (REQ-61a), not one the caller names here.
        //
        // This is a CONSENT RECORD, not a spending authorisation. It moves the leaf out of what
        // REQ-56 forces the collapse to pay on chain; it authorises no transaction and moves no
        // coin. That distinction is why a release is safe to accept from anyone holding the key.
        //
        // ORDER IS LOAD-BEARING: verify the signature FIRST (no state changes on a bad one), then
        // consume the nonce (single-use, enforced by a PRIMARY KEY collision rather than a
        // check-then-act that races the replay it exists to stop), then record. A burned nonce with
        // no release is recoverable — the holder retries with a fresh one. The reverse is not.
        CROW_ROUTE(app, "/release")
        .methods("POST"_method)([](const crow::request& req) {
            auto body = crow::json::load(req.body);
            if (!body) return crow::response(400, "malformed json");
            if (body.count("statechain_id") == 0 || body.count("nonce") == 0 ||
                body.count("sig") == 0) {
                return crow::response(400, "required: statechain_id, nonce, sig");
            }
            const std::string statechain_id = body["statechain_id"].s();
            const auto nonce = utils::ParseHex(std::string(body["nonce"].s()));
            const auto sig = utils::ParseHex(std::string(body["sig"].s()));
            if (nonce.size() != 32) return crow::response(400, "nonce must be 32 bytes");
            if (sig.size() != 64) return crow::response(400, "sig must be 64 bytes");

            // FAIL CLOSED on an unlatched coin. A coin with no latch has no key the SE can hold a
            // release to, so accepting one would mean accepting an unauthenticated assertion that a
            // holder has been discharged — exactly the operator declaration REQ-67 says the absentee
            // cannot be asked to trust.
            std::vector<unsigned char> latch_key;
            bool has_latch = false;
            std::string err;
            if (!db_manager::get_latch(statechain_id, latch_key, has_latch, err)) {
                return crow::response(500, "could not read the latch: " + err);
            }
            if (!has_latch) {
                return crow::response(409, "this statechain has no owner latch; nothing can release it");
            }

            if (!auth::verify_release(latch_key, statechain_id, nonce, sig)) {
                return crow::response(401, "release signature does not verify under the owner latch");
            }

            if (!db_manager::consume_release_nonce(statechain_id, nonce, err)) {
                // Either a replay (the PRIMARY KEY collided) or a database failure. Both refuse: a
                // replayed release is the thing the nonce exists to stop.
                return crow::response(409, "release nonce refused: " + err);
            }

            if (!db_manager::record_release(statechain_id, err)) {
                return crow::response(500, "release verified but could not be recorded: " + err);
            }

            CROW_LOG_INFO << "LEAF_RELEASED statechain " << statechain_id;
            crow::json::wvalue result({{"released", true}});
            return crow::response{result};
        });

        // ── [REQ-54 R6 / REQ-56] COLLAPSE GRANT ─────────────────────────────────────────────────
        //
        // The only place the frontier is enforced. Everything it needs was authored by the SE
        // itself: the leaf set it recorded at co-signing, the aggregate it derived at keygen, and
        // the transaction it re-hashes here. Nothing the caller says about who is owed what is
        // believed — the caller supplies only the transaction, and the SE checks it against its own
        // records.
        //
        // ORDER IS THE DESIGN. Each step below refuses on its own, and the ordering exists so that
        // a refusal names the real cause rather than the first thing that happened to look wrong:
        // an unknown root is not an empty obligation set, a malformed set is not a satisfied one,
        // and a transaction that pays everyone but spends the wrong outpoint is still not a
        // collapse of THIS root.
        //
        // NOT BUILT HERE: the partial signature. R6 says the SE issues one, and it must be issued
        // in the same database transaction as the freeze so no leaf can appear between the check
        // and the ratchet (REQ-64). Wiring a signature to a route that has never been exercised
        // would be the more dangerous half shipped first, so this route establishes the VERDICT and
        // the freeze, and refuses to pretend it signed. The signing half is the next increment.
        CROW_ROUTE(app, "/collapse_grant")
        .methods("POST"_method)([](const crow::request& req) {
            auto body = crow::json::load(req.body);
            if (!body) return crow::response(400, "malformed json");
            if (body.count("root_statechain_id") == 0 || body.count("disclosure") == 0) {
                return crow::response(400, "required: root_statechain_id, disclosure");
            }
            const std::string root_sid = body["root_statechain_id"].s();

            // 1. THE TRANSACTION, AS THE SE READS IT. The disclosure is parsed from the same bytes
            //    a signing request would carry, so `C` is a transaction the SE has examined rather
            //    than a shape the caller described.
            const auto disclosure = witness::parse_disclosure(req.body);
            if (!disclosure.has_value()) {
                return crow::response(400, "disclosure missing or malformed");
            }
            const auto parsed = tx::parse_hex(disclosure->unsigned_tx_hex);
            if (!parsed.has_value()) {
                return crow::response(400, "the disclosed transaction does not parse");
            }

            // 2. REQ-55: EXACTLY ONE INPUT. A second input would need a depositor's signature, and
            //    a round that any depositor can stall by going quiet is not a round.
            if (parsed->vin.size() != 1) {
                return crow::response(400, "C must have exactly one input (REQ-55)");
            }

            // 3. REQ-56: `C` MUST SPEND THIS ROOT'S FUNDING OUTPUT — or, on the griefed branch (R8),
            //    the trigger's payload output, which is a transaction the SE co-signed under this
            //    root. Without this a `C` that pays every owed key out of somebody ELSE'S money
            //    would satisfy the arithmetic and be granted.
            std::string err;
            std::vector<unsigned char> fund_txid;
            int64_t fund_vout = -1;
            bool have_outpoint = false;
            if (!db_manager::leaf_funding_outpoint(root_sid, fund_txid, fund_vout, have_outpoint,
                                                   err)) {
                return crow::response(500, "could not read the root's funding outpoint: " + err);
            }
            const auto& in = parsed->vin[0].prevout;
            const std::vector<unsigned char> spent_txid(in.txid, in.txid + 32);
            bool spends_root_funding =
                have_outpoint && spent_txid == fund_txid &&
                static_cast<int64_t>(in.vout) == fund_vout;
            if (!spends_root_funding) {
                // R8: the trigger has been broadcast, so `C` spends ITS payload output instead. The
                // SE co-signed that trigger under this root, which is how it can tell the branch
                // from an unrelated transaction.
                std::string owner;
                bool found = false;
                if (!db_manager::signed_tx_owner(spent_txid, owner, found, err)) {
                    return crow::response(500, "could not resolve the spent outpoint: " + err);
                }
                spends_root_funding = found && owner == root_sid;
            }
            if (!spends_root_funding) {
                return crow::response(
                    403,
                    "C does not spend this root's funding output, nor an output of a transaction "
                    "the SE co-signed under it. Refusing: a collapse of THIS root has to spend THIS "
                    "root (REQ-56).");
            }

            // 4. THE LEAF SET. `validate_for_grant` FIRST, because an empty set satisfies an empty
            //    obligation vacuously — correct arithmetic, catastrophic answer. "I have no leaves
            //    for this root" means the SE does not know, never "this root owes nobody".
            std::vector<registry::Leaf> leaves;
            if (!db_manager::load_leaves(root_sid, leaves, err)) {
                return crow::response(500, "could not load the root's leaves: " + err);
            }
            if (const auto e = registry::validate_for_grant(leaves); e != registry::SetError::Ok) {
                return crow::response(409,
                                      std::string("the SE has no usable leaf set for this root: ") +
                                          registry::describe(e));
            }
            if (const auto e = registry::validate(leaves); e != registry::SetError::Ok) {
                return crow::response(409, std::string("the leaf set is malformed: ") +
                                               registry::describe(e));
            }

            // 5. THE PREDICATE. Every unreleased frontier leaf, its FULL funding value, at its OWN
            //    key, in outputs distinct per key.
            const auto obligations = registry::owed(leaves);
            std::string why;
            if (!registry::pays_all_owed(*parsed, obligations, &why)) {
                CROW_LOG_WARNING << "COLLAPSE_REFUSED root " << root_sid << ": " << why;
                return crow::response(403, "C does not pay every unreleased frontier leaf in full: " +
                                               why);
            }

            // 6. FREEZE, PROSPECTIVELY (INV-FREEZE / REQ-64). Only after the predicate passed: a
            //    root frozen by a REFUSED grant would be a denial of service any caller could
            //    trigger by submitting a transaction that pays nobody.
            const std::string successor =
                body.count("successor_root") ? std::string(body["successor_root"].s())
                                             : std::string();
            if (!db_manager::freeze_root(root_sid, successor, err)) {
                return crow::response(500, "the predicate passed but the freeze failed: " + err);
            }

            CROW_LOG_INFO << "COLLAPSE_GRANTED root " << root_sid << " obligations "
                          << obligations.size();
            crow::json::wvalue result;
            result["granted"] = true;
            result["frozen"] = true;
            result["obligations"] = static_cast<int>(obligations.size());
            // Stated rather than implied: a caller that treats a verdict as a signature would
            // broadcast an unsigned transaction and lose the round.
            result["partial_sig"] = nullptr;
            result["note"] = "predicate satisfied and root frozen; the partial signature is not "
                             "issued by this build";
            return crow::response{result};
        });

        CROW_ROUTE(app,"/attestation_identity")
        ([&seed](){
            crow::json::wvalue result;
            try {
                unsigned char xonly[32];
                enclave::attestation_identity_pubkey(seed.data(), xonly);
                result["attestation_identity_pubkey"] = utils::key_to_string(xonly, sizeof(xonly));
                result["domain"] = "utexo/attestation-identity/v1";
            } catch (std::exception const &e) {
                return crow::response(500, std::string("Failed to derive the attestation identity: ") + e.what());
            }
            return crow::response{result};
        });

        CROW_ROUTE(app,"/signature_count/<string>")
        ([&seed](const crow::request& req, std::string statechain_id){

            // [D76] INITIALISED, and "no such coin" answered as 404 rather than attested as 0.
            // This declared `int sig_count;` and `db_manager::signature_count` returned true without
            // assigning it whenever the row was missing or the column NULL — so the value SIGNED into
            // the attestation below was an uninitialised stack read.
            int sig_count = 0;
            bool count_found = false;
            std::string error_message;
            bool count_retrieved = db_manager::signature_count(statechain_id, sig_count, count_found);

            if (!count_retrieved) {
                error_message = "Failed to retrieve signature count: " + error_message;
                return crow::response(500, error_message);
            }
            if (!count_found) {
                return crow::response(404, "no signature count exists for this statechain id");
            }

            // [D8] ATTEST THE COUNT. Returning it bare is what let a coordinator under-report it by
            // k and hide k co-signed rival states while the receiver's exact-equality census still
            // balanced — theft, resting on coordinator honesty alone. The attestation is signed by
            // THIS coin's server key, whose public half the receiver already binds to the on-chain
            // tx0 output, so verification needs nothing the client does not already hold.
            //
            // PREIMAGE, defined here and mirrored exactly by the verifier:
            //     sha256("utexo/sig_count/v2" || statechain_id || u32_be(sig_count)
            //            || u8(has_budget) || u32_be(sig_budget) || nonce32)
            //
            // **v2 adds the BUDGET [D8(i)], and it belongs in the SAME signature rather than beside
            // it.** The count alone answers "how many co-signatures exist"; a receiver's real
            // question is "can another one ever be issued", and that is the count AND the budget
            // together. Two separate attestations could be mixed across time — a fresh count paired
            // with a stale budget — which is precisely the confusion an attestation exists to remove.
            //
            // `has_budget` is an explicit byte, not an in-band sentinel: "no budget" (co-signable
            // indefinitely) and "budget 0" (terminal, nothing may be signed) are opposite facts and
            // must not share an encoding.
            //
            // v1 is GONE rather than retained. Backward compatibility is not a constraint here
            // (D23), and leaving a v1 route open would leave a way to obtain an attestation that
            // says nothing about terminality — which is the gap being closed.
            //
            // `nonce` is a 32-byte hex challenge the CLIENT chooses and passes as a query parameter.
            // Without it the attestation is a static value: a coordinator could serve a genuine
            // signature captured when the count was legitimately lower, and replay it forever. The
            // nonce makes each attestation answer one specific question asked once. A caller that
            // omits it gets the count unattested rather than a forgeable-looking one — the field is
            // simply absent, so a verifier can tell "not attested" from "attested".
            auto nonce_hex = req.url_params.get("nonce");

            int sig_budget = 0;
            bool has_budget = false;
            if (!db_manager::get_sig_budget(statechain_id, sig_budget, has_budget)) {
                return crow::response(500, "Failed to retrieve the spend budget");
            }

            crow::json::wvalue result;
            result["sig_count"] = sig_count;
            result["has_sig_budget"] = has_budget;
            if (has_budget) {
                result["sig_budget"] = sig_budget;
                // Stated rather than left to the caller's arithmetic: this is the fact the receiver
                // actually wants, and computing it here means one definition instead of several.
                result["terminal"] = (sig_count >= sig_budget);
            }

            if (nonce_hex != nullptr) {
                std::string nonce_str(nonce_hex);
                if (nonce_str.size() != 64) {
                    return crow::response(400, "nonce must be 32 bytes of hex (64 characters)");
                }
                auto nonce_bytes = utils::ParseHex(nonce_str);
                if (nonce_bytes.size() != 32) {
                    return crow::response(400, "nonce must decode to exactly 32 bytes");
                }

                // [D69] The coin's own keypair is NO LONGER LOADED for this. The attestation is
                // signed by the enclave's long-term identity key, which is why the load below is
                // gone: nothing about the signature depends on which coin is being attested any
                // more, only the PREIMAGE does. See `enclave::attestation_identity_pubkey`.

                // Build the digest. `sig_count` is serialised big-endian and fixed-width so the
                // preimage is unambiguous — a length-varying decimal string would let two different
                // (id, count) pairs collide under concatenation.
                std::string domain = "utexo/sig_count/v2";
                std::vector<unsigned char> preimage(domain.begin(), domain.end());
                preimage.insert(preimage.end(), statechain_id.begin(), statechain_id.end());
                uint32_t c = static_cast<uint32_t>(sig_count);
                preimage.push_back((c >> 24) & 0xff);
                preimage.push_back((c >> 16) & 0xff);
                preimage.push_back((c >> 8) & 0xff);
                preimage.push_back(c & 0xff);
                preimage.push_back(has_budget ? 1 : 0);
                uint32_t b = has_budget ? static_cast<uint32_t>(sig_budget) : 0;
                preimage.push_back((b >> 24) & 0xff);
                preimage.push_back((b >> 16) & 0xff);
                preimage.push_back((b >> 8) & 0xff);
                preimage.push_back(b & 0xff);
                preimage.insert(preimage.end(), nonce_bytes.begin(), nonce_bytes.end());

                unsigned char digest[32];
                SHA256(preimage.data(), preimage.size(), digest);

                try {
                    auto att = enclave::attest_sig_count_identity(seed.data(), digest);
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