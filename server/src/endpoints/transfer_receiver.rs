use std::str::FromStr;

use bitcoin::hashes::sha256;
use mercurylib::transfer::receiver::{GetMsgAddrResponsePayload, StatechainInfoResponsePayload, TransferReceiverError, TransferReceiverErrorResponsePayload, TransferReceiverPostResponsePayload, TransferReceiverRequestPayload, TransferUnlockRequestPayload};
use rocket::{State, response::status, serde::json::Json, http::Status};
use secp256k1_zkp::{PublicKey, schnorr::Signature, Message, Secp256k1};
use serde_json::{Value, json};

use crate::server::StateChainEntity;

use super::is_batch_expired;

#[get("/info/statechain/<statechain_id>")]
pub async fn statechain_info(statechain_entity: &State<StateChainEntity>, statechain_id: &str) -> status::Custom<Json<Value>> {

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
    let request = client.get(&format!("{}/{}/{}", lockbox_endpoint, path, statechain_id));

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

    let response: Value = serde_json::from_str(value.as_str()).expect(&format!("failed to parse: {}", value.as_str()));
    let num_sigs = response["sig_count"].as_u64().unwrap();

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
