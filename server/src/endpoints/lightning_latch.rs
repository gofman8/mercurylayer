use std::str::FromStr;

use chrono::Duration;
use mercurylib::transfer::sender::{PaymentHashRequestPayload, PaymentHashResponsePayload, TransferPreimageRequestPayload, TransferPreimageResponsePayload};
use rand::Rng;
use rocket::{State, serde::json::Json, response::status, http::Status};
use secp256k1_zkp::PublicKey;
use serde_json::{json, Value};

use sha2::{Sha256, Digest};

use crate::server::StateChainEntity;

#[get("/transfer/paymenthash/<batch_id>")]
pub async fn get_paymenthash(statechain_entity: &State<StateChainEntity>, batch_id: &str) -> status::Custom<Json<Value>> {

    // External latches store the payment hash directly (no SE preimage).
    if let Some(hash) = crate::database::lightning_latch::get_external_payment_hash_by_batch_id(&statechain_entity.pool, batch_id).await {
        let response_body = json!(PaymentHashResponsePayload { hash });
        return status::Custom(Status::Ok, Json(response_body));
    }

    let pre_image = crate::database::lightning_latch::get_preimage_by_batch_id(&statechain_entity.pool, batch_id).await;

    if pre_image.is_none() {
        let response_body = json!({
            "message": "Pre-image not found"
        });

        return status::Custom(Status::NotFound, Json(response_body));
    }

    let pre_image = pre_image.unwrap();
    let buffer = hex::decode(pre_image).unwrap();

    let mut hasher = Sha256::new();
    hasher.update(buffer);
    let result = hasher.finalize();

    let payment_hash = hex::encode(result);

    let payment_hash_response_payload = PaymentHashResponsePayload {
        hash: payment_hash,
    };

    let response_body = json!(payment_hash_response_payload);

    return status::Custom(Status::Ok, Json(response_body));
}

/// The statechain ids latched under a batch. The SSP uses this to identify WHICH coin(s) a
/// pay-invoice swap will hand it, so it can verify — before paying — that each is addressed to it
/// and worth at least the invoice + fee (review C2/C3). Batch ids are random UUIDs shared only
/// between the swap participants, so exposing the mapping is not a meaningful information leak.
#[get("/transfer/batch_statechains/<batch_id>")]
pub async fn get_batch_statechains(statechain_entity: &State<StateChainEntity>, batch_id: &str) -> status::Custom<Json<Value>> {
    let statechain_ids = crate::database::lightning_latch::get_statechain_ids_by_batch_id(&statechain_entity.pool, batch_id).await;
    status::Custom(Status::Ok, Json(json!({ "statechain_ids": statechain_ids })))
}

#[post("/transfer/paymenthash", format = "json", data = "<payment_hash_payload>")]
pub async fn post_paymenthash(statechain_entity: &State<StateChainEntity>, payment_hash_payload: Json<PaymentHashRequestPayload>) -> status::Custom<Json<Value>>  {

    let statechain_id = payment_hash_payload.0.statechain_id.clone();
    let signed_statechain_id = payment_hash_payload.0.auth_sig.clone();
    let batch_id = payment_hash_payload.0.batch_id.clone();

    if !crate::endpoints::utils::validate_signature(&statechain_entity.pool, &signed_statechain_id, &statechain_id).await {

        let response_body = json!({
            "message": "Signature does not match authentication key."
        });

        return status::Custom(Status::InternalServerError, Json(response_body));
    }

    let sender_auth_key = super::utils::get_auth_key_by_statechain_id(&statechain_entity.pool, &statechain_id).await.unwrap();

    let buffer = rand::thread_rng().gen::<[u8; 32]>();
    let pre_image = hex::encode(buffer.clone());

    // Audit [2]/[5]: the RECEIVE latch clock must be SHORTER than the payer's HODL HTLC deadline so
    // that if the receiver does not claim in time the coin reverts to the SSP (and the payer is
    // refunded) rather than the receiver keeping the coin while the HTLC times out. It must also be
    // LONGER than honest multi-hop LN settlement. The SDK issues the HODL invoice with a 3600 s
    // horizon; default the latch to 3000 s (50 min) — ample for settlement, ~600 s below the HTLC.
    // validate_batch and get_preimage both gate on THIS clock. Overridable via RECEIVE_LATCH_TIMEOUT.
    let latch_secs: i64 = std::env::var("RECEIVE_LATCH_TIMEOUT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3000);
    let now = chrono::Utc::now();
    let expires_at = now + Duration::seconds(latch_secs);

    crate::database::lightning_latch::insert_paymenthash(&statechain_entity.pool, &statechain_id, &sender_auth_key, &batch_id, &pre_image, &expires_at).await;

    let mut hasher = Sha256::new();
    hasher.update(buffer);
    let result = hasher.finalize();

    let payment_hash = hex::encode(result);

    let payment_hash_response_payload = PaymentHashResponsePayload {
        hash: payment_hash,
    };

    let response_body = json!(payment_hash_response_payload);

    return status::Custom(Status::Ok, Json(response_body));
    
}


#[post("/transfer/transfer_preimage", format = "json", data = "<transfer_preimage_request_payload>")]
pub async fn transfer_preimage(statechain_entity: &State<StateChainEntity>, transfer_preimage_request_payload: Json<TransferPreimageRequestPayload>) -> status::Custom<Json<Value>>  {

    let statechain_id = transfer_preimage_request_payload.0.statechain_id.clone();
    let signed_statechain_id = transfer_preimage_request_payload.0.auth_sig.clone();
    let previous_user_auth_key = transfer_preimage_request_payload.0.previous_user_auth_key.clone();
    let batch_id = transfer_preimage_request_payload.0.batch_id.clone();

    if !crate::endpoints::utils::validate_signature_given_public_key(&signed_statechain_id, &statechain_id, &previous_user_auth_key).await {

        let response_body = json!({
            "message": "Signature does not match authentication key."
        });
    
        return status::Custom(Status::Forbidden, Json(response_body));
    }

    let previous_user_auth_key = PublicKey::from_str(&previous_user_auth_key).unwrap();
    let previous_user_auth_key = previous_user_auth_key.x_only_public_key().0;

    // Audit [2] (H4) completeness: refuse to release the preimage once the latch has FULLY expired.
    // `validate_batch` closes the receiver's claim window a grace period BEFORE expiry and lets the
    // SSP fetch the preimage right up to expiry (so an honest SSP always has time to settle after
    // the receiver's last claim). But AFTER expiry the receiver can no longer claim at all, so
    // handing the preimage out here would let the SSP settle the HTLC (get paid) while the coin was
    // never transferred — the exact atomicity break the finding describes. Fail CLOSED: if the
    // expiry cannot be read, refuse (a preimage leaked past the window is unrecoverable).
    match crate::database::lightning_latch::get_latch_expiry_by_batch_id(&statechain_entity.pool, &batch_id).await {
        Some(exp) if chrono::Utc::now() <= exp => {}
        Some(_) => {
            return status::Custom(Status::Gone, Json(json!({
                "message": "latch has expired — the preimage is no longer released (the swap window closed)"
            })));
        }
        None => {
            return status::Custom(Status::ServiceUnavailable, Json(json!({
                "message": "could not read latch expiry; refusing to release the preimage"
            })));
        }
    }

    let pre_image = crate::database::lightning_latch::get_preimage(&statechain_entity.pool, &statechain_id, &previous_user_auth_key, &batch_id).await;

    if pre_image.is_none() {
        let message = format!("Pre-image for statechain {} not available. The transaction may still be locked", statechain_id);
        let response_body = json!({
            "message": message
        });

        return status::Custom(Status::NotFound, Json(response_body));
    }

    let pre_image = pre_image.unwrap();

    let response_body = json!(TransferPreimageResponsePayload {
        preimage: pre_image
    });

    return status::Custom(Status::Ok, Json(response_body));

}

/// Lightning latch v2 — register a latch bound to an EXTERNAL payment hash (e.g. the hash of a
/// BOLT11 invoice the counterparty will pay). The SE never learns the preimage in advance; the
/// batch unlocks when someone presents it (`/transfer/unlock/preimage`). This is the submarine
/// half of the pay-invoice swap: the coin becomes claimable by the receiver exactly when the
/// Lightning payment (whose settlement reveals the preimage) has been made.
#[post("/transfer/paymenthash/external", format = "json", data = "<payload>")]
pub async fn post_paymenthash_external(statechain_entity: &State<StateChainEntity>, payload: Json<mercurylib::transfer::sender::ExternalPaymentHashRequestPayload>) -> status::Custom<Json<Value>> {

    let statechain_id = payload.0.statechain_id.clone();
    let signed_statechain_id = payload.0.auth_sig.clone();
    let batch_id = payload.0.batch_id.clone();
    let payment_hash = payload.0.payment_hash.clone();

    if payment_hash.len() != 64 || hex::decode(&payment_hash).is_err() {
        let response_body = json!({ "message": "payment_hash must be 32 bytes hex" });
        return status::Custom(Status::BadRequest, Json(response_body));
    }

    if !crate::endpoints::utils::validate_signature(&statechain_entity.pool, &signed_statechain_id, &statechain_id).await {
        let response_body = json!({ "message": "Signature does not match authentication key." });
        return status::Custom(Status::InternalServerError, Json(response_body));
    }

    let sender_auth_key = super::utils::get_auth_key_by_statechain_id(&statechain_entity.pool, &statechain_id).await.unwrap();

    let now = chrono::Utc::now();
    let expires_at = now + Duration::seconds(90000); // 25h, same window as classic latches

    crate::database::lightning_latch::insert_paymenthash_external(&statechain_entity.pool, &statechain_id, &sender_auth_key, &batch_id, &payment_hash, &expires_at).await;

    let response_body = json!(PaymentHashResponsePayload { hash: payment_hash });
    return status::Custom(Status::Ok, Json(response_body));
}

/// Lightning latch v2 — unlock a latched batch by presenting the preimage of its (external)
/// payment hash. No signature needed: knowledge of the preimage IS the authorization (it can
/// only have come from settling the corresponding Lightning payment). Equivalent to the sender's
/// confirm for every coin in the batch.
#[post("/transfer/unlock/preimage", format = "json", data = "<payload>")]
pub async fn unlock_by_preimage(statechain_entity: &State<StateChainEntity>, payload: Json<mercurylib::transfer::sender::UnlockByPreimageRequestPayload>) -> status::Custom<Json<Value>> {

    let batch_id = payload.0.batch_id.clone();
    let preimage = payload.0.preimage.clone();

    let stored_hash = crate::database::lightning_latch::get_external_payment_hash_by_batch_id(&statechain_entity.pool, &batch_id).await;

    let stored_hash = match stored_hash {
        Some(h) => h,
        None => {
            let response_body = json!({ "message": "No external latch for this batch" });
            return status::Custom(Status::NotFound, Json(response_body));
        }
    };

    let preimage_bytes = match hex::decode(&preimage) {
        Ok(b) => b,
        Err(_) => {
            let response_body = json!({ "message": "preimage must be hex" });
            return status::Custom(Status::BadRequest, Json(response_body));
        }
    };
    let mut hasher = Sha256::new();
    hasher.update(&preimage_bytes);
    let digest = hex::encode(hasher.finalize());

    if digest != stored_hash {
        let response_body = json!({ "message": "preimage does not match the payment hash" });
        return status::Custom(Status::Forbidden, Json(response_body));
    }

    let ids = crate::database::lightning_latch::get_statechain_ids_by_batch_id(&statechain_entity.pool, &batch_id).await;
    if ids.is_empty() {
        // No coins latched under this batch: a correct preimage that flips NOTHING must not read as
        // Success (review M3), or a caller (e.g. the SSP) can believe a swap settled when no coin
        // was ever unlocked.
        let response_body = json!({ "message": "No coins latched under this batch (nothing to unlock)" });
        return status::Custom(Status::NotFound, Json(response_body));
    }
    // Fail CLOSED on a per-coin unlock failure. The `Result` of `update_unlock_transfer` used to be
    // DROPPED here (an unused-`must_use` warning), so a DB fault — or a batch row naming a
    // statechain_id with no transfer — left the coin LATCHED while this endpoint answered
    // `"Success", "unlocked": [<every id>]`. That is the same defect class as review M3 two lines
    // above (a preimage that flips nothing must not read as Success), just one level finer: a
    // preimage that flips only SOME of the batch must not read as Success either, or the SSP
    // believes a Lightning swap settled against coins that are still locked.
    let mut unlocked: Vec<String> = Vec::new();
    let mut failed: Vec<Value> = Vec::new();
    for statechain_id in &ids {
        // Sender-side confirm (locked2), exactly like the classic sender unlock.
        match crate::database::transfer_receiver::update_unlock_transfer(&statechain_entity.pool, true, statechain_id).await {
            Ok(()) => unlocked.push(statechain_id.clone()),
            Err(e) => failed.push(json!({ "statechain_id": statechain_id, "error": e })),
        }
    }
    if !failed.is_empty() {
        // Retryable, and safe to retry: the preimage check above is stateless and each unlock is
        // idempotent, so the caller re-presents the same preimage until every coin is reported.
        let response_body = json!({
            "message": "Batch only partially unlocked",
            "unlocked": unlocked,
            "failed": failed,
        });
        return status::Custom(Status::ServiceUnavailable, Json(response_body));
    }

    let response_body = json!({ "message": "Success", "unlocked": unlocked });
    status::Custom(Status::Ok, Json(response_body))
}

#[derive(serde::Deserialize)]
pub struct SpendBudgetRequest {
    pub statechain_id: String,
    pub auth_sig: String,
    pub remaining: i32,
}

/// Mark a coin terminal: after `remaining` more co-signatures (typically 1 — the structural
/// split/combine spend), the SE refuses everything. Owner-signed; irreversible.
#[post("/statechain/spend_budget", format = "json", data = "<payload>")]
pub async fn set_spend_budget(statechain_entity: &State<StateChainEntity>, payload: Json<SpendBudgetRequest>) -> status::Custom<Json<Value>> {
    let statechain_id = payload.0.statechain_id.clone();
    if payload.0.remaining < 0 || payload.0.remaining > 1 {
        let response_body = json!({ "message": "remaining must be 0 or 1" });
        return status::Custom(Status::BadRequest, Json(response_body));
    }
    // Audit [15]: set_spend_budget is IRREVERSIBLE (bricks the node to exit-only) — require a
    // single-use, endpoint-bound owner-auth token so a captured signature cannot be replayed.
    if !crate::endpoints::utils::validate_signature_nonce(&statechain_entity.pool, &payload.0.auth_sig, &statechain_id, "statechain/spend_budget").await {
        let response_body = json!({ "message": "Signature does not match authentication key." });
        return status::Custom(Status::Forbidden, Json(response_body));
    }
    let budget = match crate::database::deposit::set_sig_budget(&statechain_entity.pool, &statechain_id, payload.0.remaining).await {
        Ok(b) => b,
        Err(_) => {
            // Fail closed (audit [1]): never report success on a budget write we could not complete.
            return status::Custom(
                Status::ServiceUnavailable,
                Json(json!({ "message": "spend-budget update temporarily unavailable" })),
            );
        }
    };
    // ── [D8(i)] MIRROR IT INTO THE ENCLAVE, AND FAIL IF THAT DOES NOT LAND ────────────────────────
    //
    // The write above makes THIS coordinator refuse further co-signatures. That was never enough for
    // a receiver: their census argument rests on the node being terminal, and the only witness to it
    // was the coordinator — the party they are being protected from. The enclave now holds and
    // enforces its own copy, and signs it into the `utexo/sig_count/v2` attestation, so terminality
    // becomes a fact a receiver can check rather than a claim it must accept.
    //
    // Reported as a FAILURE when the mirror does not land, even though the local write succeeded.
    // A coin whose coordinator says "terminal" and whose enclave says "no budget" is the exact state
    // the receiver's verification will refuse, so reporting success here would hand the caller a coin
    // nobody can claim. The enclave's setter is a monotone ratchet, so retrying is safe: setting the
    // same budget again is idempotent.
    let enclave_index =
        crate::database::utils::get_enclave_index_from_database(&statechain_entity.pool, &statechain_id).await;
    if let Some(idx) = enclave_index {
        let config = crate::server_config::ServerConfig::load();
        if let Some(enclave) = config.enclaves.get(idx as usize) {
            let client = reqwest::Client::new();
            // The refusal REASON is carried through, not collapsed to a bool. "the enclave did not
            // accept it" and "the enclave is unreachable" are different operational problems, and
            // the operator reading this is the one who has to tell them apart.
            let why: Option<String> = match client
                .post(&format!("{}/sig_budget", enclave.url))
                .json(&json!({ "statechain_id": statechain_id, "sig_budget": payload.0.remaining }))
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => None,
                Ok(r) => {
                    let status = r.status();
                    // AUDITED-SWALLOW: the direction is toward MORE protection. `status` already
                    // decided the outcome — a non-2xx is a refusal whatever the body says — so an
                    // unreadable body costs detail in an error message that is being returned
                    // either way. It can never turn this into a success.
                    let body = r.text().await.unwrap_or_default();
                    Some(format!("the enclave answered {status}: {body}"))
                }
                Err(e) => Some(format!("the enclave could not be reached: {e}")),
            };
            if let Some(why) = why {
                return status::Custom(
                    Status::ServiceUnavailable,
                    Json(json!({
                        "message": format!(
                            "the spend budget was recorded by the coordinator but the ENCLAVE did \
                             not accept it ({why}). Terminality that only the coordinator knows \
                             about is not terminality a receiver can verify, so this is reported as \
                             a failure rather than a success. Retry — the enclave's setter is a \
                             ratchet, so setting the same budget again is idempotent."
                        )
                    })),
                );
            }
        }
    }

    let response_body = json!({ "message": "Success", "sig_budget": budget });
    status::Custom(Status::Ok, Json(response_body))
}

/// Public terminal-state query: receivers of branch transfers verify the parent node cannot be
/// co-signed again (budget set and exhausted).
#[get("/statechain/spend_budget/<statechain_id>")]
pub async fn get_spend_budget(statechain_entity: &State<StateChainEntity>, statechain_id: &str) -> status::Custom<Json<Value>> {
    // A receiver relies on this to prove a parent node is terminal (audit [1]): if the SE cannot
    // read the state it must NOT report a permissive `terminal=false` / `budget=null` that could let
    // the receiver accept a still-spendable ancestor. Fail closed (503) on a DB error.
    let budget = match crate::database::deposit::get_sig_budget(&statechain_entity.pool, statechain_id).await {
        Ok(b) => b,
        Err(_) => return status::Custom(Status::ServiceUnavailable, Json(json!({ "message": "spend-budget state temporarily unavailable" }))),
    };
    let finalized = match crate::database::deposit::count_finalized_signatures(&statechain_entity.pool, statechain_id).await {
        Ok(f) => f,
        Err(_) => return status::Custom(Status::ServiceUnavailable, Json(json!({ "message": "spend-budget state temporarily unavailable" }))),
    };
    let exhausted = budget.map(|b| finalized >= b as i64).unwrap_or(false);
    let response_body = json!({
        "sig_budget": budget,
        "finalized": finalized,
        "terminal": exhausted,
    });
    status::Custom(Status::Ok, Json(response_body))
}
