use std::str::FromStr;

use bitcoin::hashes::sha256;
use mercurylib::transfer::receiver::{GetMsgAddrResponsePayload, StatechainInfoResponsePayload, TransferReceiverError, TransferReceiverErrorResponsePayload, TransferReceiverPostResponsePayload, TransferReceiverRequestPayload, TransferUnlockRequestPayload};
use rocket::{State, response::status, serde::json::Json, http::Status};
use secp256k1_zkp::{PublicKey, schnorr::Signature, Message, Secp256k1};
use serde_json::{Value, json};

use crate::server::StateChainEntity;

use super::is_batch_expired;

// [D8] `attestation_nonce` is an OPTIONAL query parameter, so the existing URL keeps working
// unchanged for clients that do not ask for an attestation.
#[get("/info/statechain/<statechain_id>?<attestation_nonce>")]
pub async fn statechain_info(
    statechain_entity: &State<StateChainEntity>,
    statechain_id: &str,
    // [D8] Optional 32-byte hex challenge. Present => ask the SE to attest the count against it;
    // absent => the count is returned unattested, exactly as before, so old clients keep working.
    attestation_nonce: Option<String>,
) -> status::Custom<Json<Value>> {

    let enclave_public_key = crate::database::transfer_receiver::get_enclave_pubkey(&statechain_entity.pool, &statechain_id).await;

    if enclave_public_key.is_none() {
        let response_body = json!({
            "message": "Statechain Id key not found."
        });
    
        return status::Custom(Status::NotFound, Json(response_body));
    }

    let enclave_public_key = enclave_public_key.unwrap();

    let config = crate::server_config::ServerConfig::load();

    let enclave_index = crate::database::utils::get_enclave_index_from_database(&statechain_entity.pool, &statechain_id).await;

    let enclave_index = match enclave_index {
        Some(index) => index,
        None => {
            let response_body = json!({
                "message": format!("Enclave index for statechain {} ID not found.", statechain_id)
            });
        
            return status::Custom(Status::InternalServerError, Json(response_body));
        }
    };

    let enclave_index = enclave_index as usize;

    let lockbox_endpoint = config.enclaves.get(enclave_index).unwrap().url.clone();
    let path = "signature_count";

    let client: reqwest::Client = reqwest::Client::new();
    // [D8] Forward the caller's attestation nonce to the SE. The count is the right-hand side of the
    // receiver's census, and a coordinator that under-reports it hides co-signed rival states while
    // the census still balances — so the client asks the SE to SIGN the count, against a nonce the
    // client chose. Passing the nonce through is all the coordinator does: it cannot forge the
    // signature, and it cannot replay an older (lower) attestation because that one answered a
    // different nonce. A request without a nonce gets the count unattested, exactly as before.
    let request = match attestation_nonce.as_deref() {
        Some(nonce) => client.get(&format!(
            "{}/{}/{}?nonce={}", lockbox_endpoint, path, statechain_id, nonce)),
        None => client.get(&format!("{}/{}/{}", lockbox_endpoint, path, statechain_id)),
    };

    let value = match request.send().await {
        Ok(response) => {
            let text = response.text().await.unwrap();
            text
        },
        Err(err) => {
            let response_body = json!({
                "error": "Internal Server Error",
                "message": err.to_string()
            });

            return status::Custom(Status::InternalServerError, Json(response_body));
        },
    };

    // [D8] A malformed SE reply used to `.expect()`/`.unwrap()` here, panicking the request handler
    // — a 500 with no body, and a panic per malformed reply. Both are now typed errors.
    let response: Value = match serde_json::from_str(value.as_str()) {
        Ok(v) => v,
        Err(err) => {
            return status::Custom(Status::InternalServerError, Json(json!({
                "error": "Internal Server Error",
                "message": format!("signature count reply from the enclave did not parse: {}", err)
            })));
        }
    };
    let num_sigs = match response["sig_count"].as_u64() {
        Some(n) => n,
        None => {
            return status::Custom(Status::InternalServerError, Json(json!({
                "error": "Internal Server Error",
                "message": "signature count reply from the enclave carried no numeric `sig_count`"
            })));
        }
    };
    // The attestation is passed through verbatim, or omitted. The coordinator neither creates nor
    // validates it — it cannot forge one, and a client that asked for one and gets none knows the
    // count is UNATTESTED rather than being handed something that merely looks verified.
    let sig_count_attestation = response["attestation"].as_str().map(|s| s.to_string());
    let sig_count_attestation_pubkey = response["attestation_pubkey"].as_str().map(|s| s.to_string());

    let statechain_info = crate::database::transfer_receiver::get_statechain_info(&statechain_entity.pool, &statechain_id).await;

    let x1_pubkey = crate::database::transfer_receiver::get_x1pub(&statechain_entity.pool, &statechain_id).await;

    let mut x1_pub: Option<String> = None;

    if x1_pubkey.is_some() {
        x1_pub = Some(x1_pubkey.unwrap().to_string());
    }

    let aggregate_pubkey = crate::database::transfer_receiver::get_aggregate_pubkey(&statechain_entity.pool, &statechain_id).await;

    let statechain_info_response_payload = StatechainInfoResponsePayload {
        enclave_public_key: enclave_public_key.to_string(),
        num_sigs: num_sigs as u32,
        statechain_info,
        x1_pub,
        aggregate_pubkey,
        sig_count_attestation,
        sig_count_attestation_pubkey,
    };
    
    let response_body = json!(statechain_info_response_payload);

    return status::Custom(Status::Ok, Json(response_body));
    
}

#[get("/transfer/get_msg_addr/<new_auth_key>")]
pub async fn get_msg_addr(statechain_entity: &State<StateChainEntity>, new_auth_key: &str) -> status::Custom<Json<Value>>  {

    let new_user_auth_public_key = PublicKey::from_str(new_auth_key);

    if new_user_auth_public_key.is_err() {
        let response_body = json!({
            "error": "Internal Server Error",
            "message": "Invalid authentication public key"
        });
    
        return status::Custom(Status::InternalServerError, Json(response_body));
    }

    let new_user_auth_public_key = new_user_auth_public_key.unwrap();
    
    let result = crate::database::transfer_receiver::get_statechain_transfer_messages(&statechain_entity.pool, &new_user_auth_public_key).await;

    let get_msg_addr_response_payload = GetMsgAddrResponsePayload {
        list_enc_transfer_msg:result
    };

    let response_body = json!(get_msg_addr_response_payload);

    return status::Custom(Status::Ok, Json(response_body));
}

#[post("/transfer/unlock", format = "json", data = "<transfer_unlock_request_payload>")]
pub async fn transfer_unlock(statechain_entity: &State<StateChainEntity>, transfer_unlock_request_payload: Json<TransferUnlockRequestPayload>) -> status::Custom<Json<Value>> {

    let statechain_id = transfer_unlock_request_payload.0.statechain_id.clone();
    let signed_statechain_id = transfer_unlock_request_payload.0.auth_sig.clone();
    let auth_pub_key = transfer_unlock_request_payload.0.auth_pub_key.clone();

    let is_current_owner_signature = crate::endpoints::utils::validate_signature(&statechain_entity.pool, &signed_statechain_id, &statechain_id).await;

    // Authorize the unlock (external review finding 1): accept ONLY when the current-owner signature
    // validates (registered auth key — clears `locked2`, the owner side) OR an `auth_pub_key` is
    // supplied AND the signature validates under it (the receiver signs with their not-yet-registered
    // new key — clears `locked`, the receiver side; see transfer_receiver.rs::unlock_statecoin).
    // The previous condition only rejected when `auth_pub_key` was *present* and invalid, so a
    // MISSING `auth_pub_key` with a bad `auth_sig` fell straight through to the DB write — letting
    // anyone who knows a statechain_id clear the receiver-side lock with no valid signature at all.
    let is_new_owner_signature = match &auth_pub_key {
        Some(pk) => crate::endpoints::utils::validate_signature_given_public_key(&signed_statechain_id, &statechain_id, pk).await,
        None => false,
    };
    if !is_current_owner_signature && !is_new_owner_signature {
        let response_body = json!({
            "message": "Signature does not match authentication key."
        });

        return status::Custom(Status::Forbidden, Json(response_body));
    }

    // `is_current_owner_signature` still selects WHICH lock bit is cleared (owner → locked2,
    // receiver → locked); we now only reach here once one of the two signatures is valid.
    if let Err(e) = crate::database::transfer_receiver::update_unlock_transfer(&statechain_entity.pool, is_current_owner_signature, &statechain_id).await {
        // Typed not-found / DB error instead of a panic on an unknown statechain_id (the old
        // fetch_one().unwrap() 500-crashed the handler on any id that does not exist).
        return status::Custom(Status::NotFound, Json(json!({ "message": e })));
    }

    let response_body = json!({
        "message": "Success"
    });

    status::Custom(Status::Ok, Json(response_body))
}

pub enum BatchTransferReceiveValidationResult {

    /// The statecoin batch is locked (not expired yet and not all coins are unlocked)
    StatecoinBatchLockedError (String),
    /// The batch_id sent by the user is expired
    ExpiredBatchTimeError (String),
    /// Success means there is no batch_id for the statecoin or all the coins of the batch are unlocked.
    Success,
}

pub async fn validate_batch(statechain_entity: &State<StateChainEntity>, statechain_id: &str)  -> BatchTransferReceiveValidationResult{

    let batch_info = crate::database::transfer::get_batch_id_and_time_by_statechain_id(&statechain_entity.pool, statechain_id).await;

    // batch exists
    if batch_info.is_some() {

        let (batch_id, batch_time) = batch_info.unwrap();

        // Audit [2] (H4): a Lightning-latch batch is governed by the latch's OWN expiry, NOT the
        // short `batch_timeout` (deployed as low as 20 s). Keying the claim gate on `batch_timeout`
        // makes an honest receiver miss the window on any LN settlement slower than that. Gate an
        // LN-latch batch on `lightning_latch.expires_at` (the coordinated clock, shorter than the
        // payer's HODL HTLC); a plain transfer batch still uses `batch_timeout`.
        let latch_expiry =
            crate::database::lightning_latch::get_latch_expiry_by_batch_id(&statechain_entity.pool, &batch_id).await;
        let expired = match latch_expiry {
            // The receiver's claim window closes a GRACE period BEFORE the latch's own expiry, while
            // the SSP's preimage retrieval (get_preimage) is allowed right up to expiry. This
            // guarantees the SSP always has `grace` seconds to settle the HTLC after the receiver's
            // last possible claim — closing the boundary race where a claim lands microseconds before
            // expiry but the SSP's settle lands just after.
            Some(exp) => {
                let grace_secs: i64 = std::env::var("RECEIVE_LATCH_GRACE")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(300);
                chrono::Utc::now() > (exp - chrono::Duration::seconds(grace_secs))
            }
            None => is_batch_expired(batch_time),
        };

        if expired {
            return BatchTransferReceiveValidationResult::ExpiredBatchTimeError("Batch/latch time has expired".to_string());
        } else {

            // not expired. Check if all coins are unlocked.
            let all_coins_unlocked = crate::database::transfer::is_all_coins_unlocked(&statechain_entity.pool, &batch_id).await;

            if all_coins_unlocked {
                return BatchTransferReceiveValidationResult::Success;
            } else {
                return BatchTransferReceiveValidationResult::StatecoinBatchLockedError("Statecoin batch is locked".to_string());
            }
        }
    }

    BatchTransferReceiveValidationResult::Success
}

/// If a transfer of `statechain_id` to the key that signed `t2` was CANCELLED, build the typed 410.
///
/// Returns `None` when no cancellation record matches the caller — in which case the caller falls
/// through to whatever generic answer it was already going to give.
///
/// The caller PROVES possession first: we verify their `auth_sig` over `t2` against each tombstoned
/// recipient key and only answer for a key that verifies. So this discloses nothing to a third
/// party who merely knows a statechain id — it tells a recipient, and only the recipient, that the
/// payment they were expecting was withdrawn rather than that their mailbox is quiet. Without this,
/// a cancelled payment is indistinguishable from nothing having arrived, and the client's
/// `println!` + `continue` loop would swallow it.
async fn cancelled_transfer_response(
    statechain_entity: &State<StateChainEntity>,
    statechain_id: &str,
    t2: &str,
    auth_sig: &str,
) -> Option<status::Custom<Json<Value>>> {

    // AUDITED-SWALLOW: a malformed `auth_sig` is not a fault, it is a caller that has not PROVEN
    // possession of the recipient key — so it gets exactly the generic not-found any stranger gets.
    // Direction: strictly LESS disclosure, never less protection. Spelled as a match, not `.ok()?`,
    // so the direction is visible at the site.
    let signed_message = match Signature::from_str(auth_sig) {
        Ok(sig) => sig,
        Err(_) => return None,
    };
    let msg = Message::from_hashed_data::<sha256::Hash>(t2.as_bytes());
    let secp = Secp256k1::new();

    // A DB FAULT IS NOT "NO CANCELLATION RECORD". This used to be `.ok()?`, which handed the
    // recipient a bare 404 whenever the tombstone table could not be read — i.e. the cancelled
    // payment looked exactly like an idle mailbox, the one thing this function exists to prevent.
    // Answer 503: "I could not tell" is distinguishable from "there is nothing here", and the
    // client retries instead of concluding the payment simply never arrived.
    let keys = match crate::database::transfer_cancel::cancelled_recipient_keys(
        &statechain_entity.pool,
        statechain_id,
    )
    .await
    {
        Ok(keys) => keys,
        Err(_) => {
            return Some(status::Custom(
                Status::ServiceUnavailable,
                Json(json!({
                    "message": "could not determine whether this transfer was cancelled; retry"
                })),
            ));
        }
    };

    for key in keys {
        if secp.verify_schnorr(&signed_message, &msg, &key.x_only_public_key().0).is_ok() {
            let response_body = json!(TransferReceiverErrorResponsePayload {
                code: TransferReceiverError::TransferCancelledError,
                message: "this transfer was cancelled by the sender with the recipient's consent; the payment did not complete".to_string(),
            });
            return Some(status::Custom(Status::Gone, Json(response_body)));
        }
    }

    None
}

#[post("/transfer/receiver", format = "json", data = "<transfer_receiver_request_payload>")]
pub async fn transfer_receiver(statechain_entity: &State<StateChainEntity>, transfer_receiver_request_payload: Json<TransferReceiverRequestPayload>) -> status::Custom<Json<Value>> {

    // Batch gate: if this statechain is in a batch, reject unless the batch is open with all coins
    // unlocked (and not expired). Implemented by validate_batch below (incl. the Lightning-latch
    // expiry-minus-grace refinement, audit [2]/H4).
    let batch_validation_result = validate_batch(&statechain_entity, &transfer_receiver_request_payload.statechain_id).await;

    match batch_validation_result {
        BatchTransferReceiveValidationResult::StatecoinBatchLockedError(msg) => {

            let response_body = json!(TransferReceiverErrorResponsePayload {
                code: TransferReceiverError::StatecoinBatchLockedError,
                message: msg
            });
        
            return status::Custom(Status::BadRequest, Json(response_body));
        },
        BatchTransferReceiveValidationResult::ExpiredBatchTimeError(msg) => {
            
            let response_body = json!(TransferReceiverErrorResponsePayload {
                code: TransferReceiverError::ExpiredBatchTimeError,
                message: msg
            });
        
            return status::Custom(Status::BadRequest, Json(response_body));
        },
        BatchTransferReceiveValidationResult::Success => {},
    }

    let auth_pubkey_x1 = crate::database::transfer_receiver::get_auth_pubkey_and_x1(&statechain_entity.pool, &transfer_receiver_request_payload.statechain_id).await;

    if auth_pubkey_x1.is_none() {
        // Before the generic not-found: was a transfer of this coin to THIS caller CANCELLED?
        // A cancelled payment that looks like an idle mailbox is the silent-degradation shape a
        // recipient must never be shown, so answer it by name — but only to a caller that PROVES it
        // holds the recorded recipient key, so this is never an existence oracle for a third party.
        if let Some(response) = cancelled_transfer_response(
            &statechain_entity,
            &transfer_receiver_request_payload.statechain_id,
            &transfer_receiver_request_payload.t2,
            &transfer_receiver_request_payload.auth_sig,
        ).await {
            return response;
        }

        let response_body = json!({
            "message": "No transfer messages found for this statechain_id"
        });

        return status::Custom(Status::NotFound, Json(response_body));
    }

    let auth_pubkey_x1 = auth_pubkey_x1.unwrap();
    let auth_pubkey = auth_pubkey_x1.0;
    let x1 = auth_pubkey_x1.1;

    let auth_pubkey = auth_pubkey.x_only_public_key().0;

    let statechain_id = transfer_receiver_request_payload.statechain_id.clone();
    let t2 = transfer_receiver_request_payload.t2.clone();
    let auth_sign = transfer_receiver_request_payload.auth_sig.clone();

    // Audit [19]: 400 on a malformed signature instead of panicking.
    let signed_message = match Signature::from_str(&auth_sign) {
        Ok(s) => s,
        Err(_) => return status::Custom(Status::BadRequest, Json(json!({ "message": "invalid auth_sig" }))),
    };
    let msg = Message::from_hashed_data::<sha256::Hash>(t2.as_bytes());

    let secp = Secp256k1::new();
    
    if !secp.verify_schnorr(&signed_message, &msg, &auth_pubkey).is_ok() {

        // The signature does not match the CURRENT transfer's recipient. That is also what a
        // recipient sees after their transfer was cancelled and the coin re-addressed to someone
        // else — `insert_new_transfer` DELETEs the old row, so the tombstone is the only remaining
        // record. Check it, and answer by name to whoever can prove they held the cancelled key.
        if let Some(response) = cancelled_transfer_response(
            &statechain_entity,
            &statechain_id,
            &t2,
            &auth_sign,
        ).await {
            return response;
        }

        let response_body = json!({
            "message": "Signature does not match authentication key."
        });

        return status::Custom(Status::InternalServerError, Json(response_body));

    }

    if crate::database::transfer_receiver::is_key_already_updated(&statechain_entity.pool, &statechain_id).await {

        let server_public_key = crate::database::transfer_receiver::get_server_public_key(&statechain_entity.pool, &statechain_id).await;

        if server_public_key.is_none() {
            let response_body = json!({
                "message": "Server public key not found."
            });
        
            return status::Custom(Status::InternalServerError, Json(response_body));
        }

        let server_public_key = server_public_key.unwrap();

        let response_body = json!({
            "server_pubkey": server_public_key.to_string(),
        });

        return status::Custom(Status::Ok, Json(response_body));
    }

    // [CLAIM LATCH — migration 0010] From here on the enclave may rotate its key share, and an
    // enclave keyupdate is IRREVERSIBLE: no coordinator flag can undo it, and refusing the claim
    // afterwards would leave the coin's aggregate unreachable by anyone. So take the latch BEFORE
    // the enclave call. It is one atomic UPDATE whose WHERE clause is exactly complementary to the
    // cancellation's, so under READ COMMITTED exactly one of {this claim, a concurrent cancel} can
    // win — closing the race where a recipient co-signs a cancellation and then pipelines a claim,
    // which would otherwise let a LATER recipient's row be marked claimed while the coin went to the
    // earlier one.
    match crate::database::transfer_cancel::latch_claim(&statechain_entity.pool, &statechain_id).await {
        Ok(true) => {}
        Ok(false) => {
            // Either the transfer was cancelled under us, or another claim is already in flight.
            // Both mean: do not touch the enclave.
            if let Some(response) = cancelled_transfer_response(&statechain_entity, &statechain_id, &t2, &auth_sign).await {
                return response;
            }
            return status::Custom(
                Status::Conflict,
                Json(json!({ "message": "a claim for this transfer is already in progress; retry shortly" })),
            );
        }
        Err(_) => {
            return status::Custom(
                Status::ServiceUnavailable,
                Json(json!({ "message": "transfer-state check unavailable; refusing to complete the transfer (fail-closed)" })),
            );
        }
    }

    let x1_hex = hex::encode(x1);

    let key_update_response_payload = mercurylib::transfer::receiver::KeyUpdateResponsePayload {
        statechain_id: statechain_id.clone(),
        t2,
        x1: x1_hex,
    };

    let config = crate::server_config::ServerConfig::load();

    let enclave_index = crate::database::utils::get_enclave_index_from_database(&statechain_entity.pool, &statechain_id).await;

    let enclave_index = match enclave_index {
        Some(index) => index,
        None => {
            // Never reached the enclave: release the latch so a retry is not made to wait out
            // CLAIM_LATCH_SECS. Releasing is always safe — the latch only ever DENIES transitions.
            crate::database::transfer_cancel::release_claim_latch(&statechain_entity.pool, &statechain_id).await;
            let response_body = json!({
                "message": format!("Enclave index for statechain {} ID not found.", statechain_id)
            });

            return status::Custom(Status::InternalServerError, Json(response_body));
        }
    };

    let enclave_index = enclave_index as usize;

    let lockbox_endpoint = config.enclaves.get(enclave_index).unwrap().url.clone();
    let path = "keyupdate";

    let client: reqwest::Client = reqwest::Client::new();
    let request = client.post(&format!("{}/{}", lockbox_endpoint, path));

    let value = match request.json(&key_update_response_payload).send().await {
        Ok(response) => {
            let text = response.text().await.unwrap();
            text
        },
        Err(err) => {
            // Deliberately do NOT release the claim latch here. A transport error means we do not
            // know whether the enclave applied the keyupdate — and if it did, its share has already
            // rotated. Holding the latch for CLAIM_LATCH_SECS keeps a cancellation from releasing
            // the coin under a rotation that may have happened, and equally keeps an immediate claim
            // retry from asking the enclave to rotate a second time. The ambiguous case must wait;
            // it must not be resolved optimistically in either direction.
            let response_body = json!({
                "error": "Internal Server Error",
                "message": err.to_string()
            });

            return status::Custom(Status::InternalServerError, Json(response_body));
        },
    };

    let response: TransferReceiverPostResponsePayload = serde_json::from_str(value.as_str()).expect(&format!("failed to parse: {}", value.as_str()));

    let mut server_pubkey_hex = response.server_pubkey.clone();

    if server_pubkey_hex.starts_with("0x") {
        server_pubkey_hex = server_pubkey_hex[2..].to_string();
    }

    let server_pubkey = PublicKey::from_str(&server_pubkey_hex).unwrap();

    crate::database::transfer_receiver::update_statechain(&statechain_entity.pool, &auth_pubkey, &server_pubkey, &statechain_id).await;

    let response_body = json!(TransferReceiverPostResponsePayload {
        server_pubkey: server_pubkey.to_string(),
    });

    status::Custom(Status::Ok, Json(response_body))
}
