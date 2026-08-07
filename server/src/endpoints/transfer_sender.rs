use std::str::FromStr;

use mercurylib::transfer::cancel::{decide_transfer_cancel, CancelDecision, RecipientConsent, TransferCancelRequestPayload, TransferCancelResponsePayload, TransferCancelState, CANCEL_RECIPIENT_ENDPOINT, CANCEL_SENDER_ENDPOINT};
use mercurylib::transfer::sender::{TransferSenderRequestPayload, TransferSenderResponsePayload, TransferUpdateMsgRequestPayload};
use rocket::{State, serde::json::Json, response::status, http::Status};
use secp256k1_zkp::{PublicKey, Scalar, SecretKey};
use serde_json::{Value, json};

use crate::server::StateChainEntity;

use super::is_batch_expired;

/// Enun to represent the possible results of the batch transfer validation
pub enum BatchTransferValidationResult {

    /// The statecoin batch is locked (not expired yet)
    StatecoinBatchLockedError (String),
    /// The batch_id sent by the user is expired
    ExpiredBatchTimeError (String),
    /// Success means there is no batch_id for the statecoin, 
    /// or the batch is complete or expired and the batch_id is different from the new_batch_id (or null)
    Success,
}

pub async fn validate_batch_transfer(statechain_entity: &State<StateChainEntity>, statechain_id: &str, new_batch_id: &Option<String>) -> BatchTransferValidationResult {

    // get an extistent batch according to the statecoin, in case the user sent a repeated statecoin
    let batch_info = crate::database::transfer::get_batch_id_and_time_by_statechain_id(&statechain_entity.pool, &statechain_id).await;

    if batch_info.is_some() {

        let (batch_id, batch_time) = batch_info.unwrap();

        if !is_batch_expired(batch_time) {

            let all_coins_unlocked = crate::database::transfer::is_all_coins_unlocked(&statechain_entity.pool, &batch_id).await;

            if all_coins_unlocked {
                return BatchTransferValidationResult::Success;
            }

            // the batch time has not expired
            return BatchTransferValidationResult::StatecoinBatchLockedError("Statecoin batch locked (the batch time has not expired).".to_string())
        } else {
            // the batch time has expired
            if new_batch_id.is_some() && new_batch_id.as_ref().unwrap().to_string() == batch_id {
                // if the new_batch_id is the same should return error
                return BatchTransferValidationResult::ExpiredBatchTimeError("Batch time has expired. Try a new batch id.".to_string());
            } else {
                // // if the new_batch_id is None or different should return success
                return BatchTransferValidationResult::Success;
            }
        }
    }

    // here the statecoin has no batch_id
    // then we check if the user sends a existing batch_id, trying to add a new transfer to this batch.
    if new_batch_id.is_some() {
        let new_batch_id = new_batch_id.as_ref().unwrap();
        
        let batch_time = crate::database::transfer_sender::get_batch_time_by_batch_id(&statechain_entity.pool, new_batch_id).await;

        // if the batch_id exists
        if batch_time.is_some() {
            let batch_time = batch_time.unwrap();

            // Audit [16] (batch poisoning): for a LIGHTNING-LATCH batch, only coins the originator
            // actually latched may join. Otherwise any owner who learns the batch_id could inject a
            // never-unlocked coin and keep is_all_coins_unlocked false until timeout, blocking every
            // honest receiver in the batch (griefing DoS + tied-up SSP capital). The SSP creates a
            // lightning_latch row per member, so require the joining coin to be one of them.
            let is_latch_batch = crate::database::lightning_latch::get_latch_expiry_by_batch_id(&statechain_entity.pool, new_batch_id).await.is_some();
            if is_latch_batch
                && !crate::database::lightning_latch::statechain_in_latch_batch(&statechain_entity.pool, statechain_id, new_batch_id).await
            {
                return BatchTransferValidationResult::StatecoinBatchLockedError(
                    "coin is not an authorized member of this lightning-latch batch".to_string(),
                );
            }

            if !is_batch_expired(batch_time) {
                // the batch time has not expired. It is possible to add a new coin to the batch.
                return BatchTransferValidationResult::Success
            } else {
                // the batch time has expired. New coins not allowed.
                return BatchTransferValidationResult::ExpiredBatchTimeError("Batch time has expired. Try a new batch id.".to_string());
            }
        }
    }

    // if the statecoin has no batch_id should return success
    BatchTransferValidationResult::Success
    
}

#[post("/transfer/sender", format = "json", data = "<transfer_sender_request_payload>")]
pub async fn transfer_sender(statechain_entity: &State<StateChainEntity>, transfer_sender_request_payload: Json<TransferSenderRequestPayload>) -> status::Custom<Json<Value>>  {

    let statechain_id = transfer_sender_request_payload.0.statechain_id.clone();
    let signed_statechain_id = transfer_sender_request_payload.0.auth_sig.clone();
    let batch_id = transfer_sender_request_payload.0.batch_id.clone();

    if !crate::endpoints::utils::validate_signature(&statechain_entity.pool, &signed_statechain_id, &statechain_id).await {

        let response_body = json!({
            "message": "Signature does not match authentication key."
        });
    
        return status::Custom(Status::InternalServerError, Json(response_body));
    }

    let batch_transfer_validation_result = validate_batch_transfer(&statechain_entity, &statechain_id, &batch_id).await;

    match batch_transfer_validation_result {
        BatchTransferValidationResult::StatecoinBatchLockedError(message) | BatchTransferValidationResult::ExpiredBatchTimeError(message) => {
            let response_body = json!({
                "message": message
            });
        
            return status::Custom(Status::BadRequest, Json(response_body));
        },
        BatchTransferValidationResult::Success => {
            // nothing to do. continue.
        }
    }

    let new_user_auth_key = PublicKey::from_str(&transfer_sender_request_payload.0.new_user_auth_key).unwrap();

    if crate::database::transfer_sender::exists_msg_for_same_statechain_id_and_new_user_auth_key(&statechain_entity.pool, &new_user_auth_key, &statechain_id, &batch_id).await {

        let message = if batch_id.is_some() {
            "Transfer message already exists for this statechain_id, new_user_auth_key and batch_id."
        } else {
            "Transfer message already exists for this statechain_id and new_user_auth_key."
        };

        let response_body = json!({
            "message": message
        });
    
        return status::Custom(Status::BadRequest, Json(response_body));
    }

    // [RE-ADDRESS GUARD — CHILDREN.md] Refuse to open a transfer to a DIFFERENT receiver when
    // this coin already has an OPEN (conveyed, uncompleted) transfer to another key. `insert_new_transfer`
    // DELETEs the prior row by statechain_id, so without this a still-owner sender could re-address an
    // already-conveyed (victim-accepted) transfer to an attacker key it controls — the self-reopen
    // re-address vector the child-firstclass review found. A same-auth retry is allowed (idempotent).
    // Fail CLOSED on a DB error.
    match crate::database::transfer_sender::has_open_transfer_to_other_auth(&statechain_entity.pool, &statechain_id, &new_user_auth_key, crate::server_config::ServerConfig::load().batch_timeout as i64).await {
        Ok(true) => {
            return status::Custom(
                Status::Conflict,
                Json(json!({ "message": "coin already has an open transfer to a different recipient; complete or let it expire before re-addressing" })),
            );
        }
        Ok(false) => {}
        Err(_) => {
            return status::Custom(
                Status::ServiceUnavailable,
                Json(json!({ "message": "transfer-state check unavailable; refusing to open a transfer (fail-closed)" })),
            );
        }
    }

    // [CANCELLED-KEY REUSE GUARD — migration 0010] Refuse to re-address this coin to a recipient key
    // that already had a transfer of it CANCELLED. Transfer addresses are single-use by construction
    // (the client mints a fresh coin, hence a fresh auth key, per `new_transfer_address`), so this
    // costs an honest sender nothing. What it removes: conveyed material is blinded against the x1
    // of the transfer it was built for, and a reopened transfer to the same key carries a FRESH x1.
    // A recipient still holding the old message that claimed against the new row would drive the
    // enclave to rotate its share by the wrong tweak — irreversible, and the coin is bricked for
    // everyone. Fail CLOSED on a DB error.
    match crate::database::transfer_cancel::recipient_key_was_cancelled(&statechain_entity.pool, &statechain_id, &new_user_auth_key).await {
        Ok(true) => {
            return status::Custom(
                Status::Conflict,
                Json(json!({ "message": "a transfer of this coin to that recipient key was cancelled; ask the recipient for a fresh transfer address" })),
            );
        }
        Ok(false) => {}
        Err(_) => {
            return status::Custom(
                Status::ServiceUnavailable,
                Json(json!({ "message": "transfer-state check unavailable; refusing to open a transfer (fail-closed)" })),
            );
        }
    }

    let secret_x1 = SecretKey::new(&mut rand::thread_rng());

    let s_x1 = Scalar::from(secret_x1);
    let x1 = s_x1.to_be_bytes();

    crate::database::transfer_sender::insert_new_transfer(&statechain_entity.pool, &new_user_auth_key, &x1, &statechain_id, &batch_id).await;

    let transfer_sender_response_payload = TransferSenderResponsePayload {
        x1: hex::encode(x1),
    };

    let response_body = json!(transfer_sender_response_payload);

    return status::Custom(Status::Ok, Json(response_body));
}

#[post("/transfer/update_msg", format = "json", data = "<transfer_update_msg_request_payload>")]
pub async fn transfer_update_msg(statechain_entity: &State<StateChainEntity>, transfer_update_msg_request_payload: Json<TransferUpdateMsgRequestPayload>) -> status::Custom<Json<Value>>  {

    let statechain_id = transfer_update_msg_request_payload.0.statechain_id.clone();
    let signed_statechain_id = transfer_update_msg_request_payload.0.auth_sig.clone();

    if !crate::endpoints::utils::validate_signature(&statechain_entity.pool, &signed_statechain_id, &statechain_id).await {

        let response_body = json!({
            "error": "Internal Server Error",
            "message": "Signature does not match authentication key."
        });
    
        return status::Custom(Status::InternalServerError, Json(response_body));
    }

    let new_user_auth_key = PublicKey::from_str(&transfer_update_msg_request_payload.0.new_user_auth_key).unwrap();
    let enc_transfer_msg_hex =  transfer_update_msg_request_payload.0.enc_transfer_msg;
    let enc_transfer_msg = hex::decode(enc_transfer_msg_hex).unwrap();

    crate::database::transfer_sender::update_transfer_msg(&statechain_entity.pool, &new_user_auth_key, &enc_transfer_msg, &statechain_id).await;

    let response_body = json!({
        "updated": true,
    });

    return status::Custom(Status::Ok, Json(response_body));
}

/// Map a cancellation decision to its HTTP status.
///
/// `AlreadyCancelled` is a 200, not an error: cancellation is irreversible, so a client that retries
/// after a dropped response must not be told something different the second time.
fn cancel_status(decision: CancelDecision) -> Status {
    match decision {
        CancelDecision::Allow | CancelDecision::AlreadyCancelled => Status::Ok,
        CancelDecision::NoSuchTransfer => Status::NotFound,
        CancelDecision::AlreadyClaimed => Status::Gone,
        CancelDecision::Batched | CancelDecision::ClaimInFlight | CancelDecision::RecipientConsentRequired => Status::Conflict,
        CancelDecision::RecipientSignatureInvalid => Status::Forbidden,
    }
}

fn cancel_response(decision: CancelDecision, recipient_auth_pub_key: Option<String>) -> status::Custom<Json<Value>> {
    let body = TransferCancelResponsePayload {
        code: decision.code().to_string(),
        message: decision.message().to_string(),
        recipient_auth_pub_key,
    };
    status::Custom(cancel_status(decision), Json(json!(body)))
}

/// `POST /transfer/cancel` — withdraw an opened transfer that nobody has claimed, so the coin
/// becomes spendable again.
///
/// # What this is releasing
///
/// The pending-transfer lock (`has_open_transfer`) is the only thing that stops a still-owner sender
/// from co-signing a rival state while a conveyed recipient holds claimable material. Mechanically
/// this endpoint is "expire now" — but expiry today carries NO authorization at all (it is pure
/// time), and that is exactly why it is safe: an hour of not-being-claimed is evidence nobody was
/// paid. Cancellation replaces that hour with an authorization, and the authorization has to be at
/// least as strong.
///
/// It is: the rule in `mercurylib::transfer::cancel` requires the RECORDED recipient's co-signature
/// the moment the mailbox message has been posted. The sender alone can only withdraw a transfer
/// whose message never left the coordinator. `RecipientConsentRequired` is the named refusal for
/// everything else, and this endpoint has no override for it — not a force flag, not an operator
/// path. A sender whose recipient is conveyed and offline still waits out the hour, and must.
///
/// # Authorization
///
/// Both legs are single-use `"<nonce>:<sig>"` tokens over `sha256(nonce|endpoint)` with distinct
/// endpoint strings, never the static replayable `sha256(statechain_id)` signature. Cancellation is
/// irreversible, so it joins `withdraw/complete` on the nonce-protected list. See
/// `validate_signature_nonce_given_public_key` for why the recipient leg in particular must not be
/// static.
///
/// # The enclave
///
/// Is not involved. A cancellation is coordinator bookkeeping: no amount, no colour, no leg identity
/// is revealed to the SE, and no SE state changes. The one place the enclave DOES matter is the
/// claim latch — once `/transfer/receiver` has latched, its keyupdate may already have rotated the
/// enclave's share, and no coordinator flag can undo that. So a latched claim beats a cancellation
/// (`ClaimInFlight`), never the other way round.
#[post("/transfer/cancel", format = "json", data = "<transfer_cancel_request_payload>")]
pub async fn transfer_cancel(statechain_entity: &State<StateChainEntity>, transfer_cancel_request_payload: Json<TransferCancelRequestPayload>) -> status::Custom<Json<Value>> {

    let statechain_id = transfer_cancel_request_payload.0.statechain_id.clone();

    // Sender auth FIRST, before any row lookup, so an unauthenticated caller cannot use this
    // endpoint as an oracle for whether a coin has a pending transfer (or exists at all): every
    // unauthenticated request gets the same answer regardless of state.
    if !crate::endpoints::utils::validate_signature_nonce(
        &statechain_entity.pool,
        &transfer_cancel_request_payload.0.auth_sig,
        &statechain_id,
        CANCEL_SENDER_ENDPOINT,
    ).await {
        return status::Custom(
            Status::Forbidden,
            Json(json!({ "message": "Signature does not match authentication key." })),
        );
    }

    let row = match crate::database::transfer_cancel::get_transfer_for_cancel(&statechain_entity.pool, &statechain_id).await {
        Ok(r) => r,
        // Fail CLOSED. Reporting a DB fault as "no such transfer" would tell a sender its coin is
        // free while the lock is in fact still held.
        Err(_) => {
            return status::Custom(
                Status::ServiceUnavailable,
                Json(json!({ "message": "transfer-state lookup unavailable; refusing to cancel (fail-closed)" })),
            );
        }
    };

    let row = match row {
        Some(r) => r,
        None => return cancel_response(CancelDecision::NoSuchTransfer, None),
    };

    // The recipient leg. It counts as consent ONLY if it verifies under the key the coordinator
    // recorded for this transfer — a signature under any other key, including one the sender picked,
    // is `Invalid`. `recipient_auth_pub_key` must therefore MATCH the record; it is an assertion the
    // caller makes and the server checks, never a substitute for the record.
    let recipient_consent = match (
        &transfer_cancel_request_payload.0.recipient_auth_sig,
        &transfer_cancel_request_payload.0.recipient_auth_pub_key,
    ) {
        (None, _) => RecipientConsent::Absent,
        (Some(sig), claimed_key) => {
            let key_matches = match claimed_key {
                Some(k) => PublicKey::from_str(k).map(|pk| pk == row.recipient_auth_pub_key).unwrap_or(false),
                // A signature with no key named is still checked against the record — naming the key
                // is a convenience, not the authorization.
                None => true,
            };
            if !key_matches {
                RecipientConsent::Invalid
            } else if crate::endpoints::utils::validate_signature_nonce_given_public_key(
                &statechain_entity.pool,
                sig,
                &statechain_id,
                CANCEL_RECIPIENT_ENDPOINT,
                &row.recipient_auth_pub_key,
            ).await {
                RecipientConsent::Valid
            } else {
                RecipientConsent::Invalid
            }
        }
    };

    let state = TransferCancelState {
        row_exists: true,
        key_updated: row.key_updated,
        already_cancelled: row.cancelled,
        is_batched: row.batched,
        message_posted: row.message_posted,
        claim_in_flight: row.claim_in_flight,
        recipient_consent,
    };

    let decision = decide_transfer_cancel(&state);

    if !decision.releases_lock() {
        // Tell an AUTHENTICATED sender which key must co-sign — it cannot act on the refusal
        // otherwise. Only on this decision, and only after the sender's own signature verified: the
        // coin -> recipient-key link is not published anywhere unauthenticated, because it would let
        // an observer correlate a coin with its next owner's mailbox.
        let key = if decision == CancelDecision::RecipientConsentRequired {
            Some(row.recipient_auth_pub_key.to_string())
        } else {
            None
        };
        return cancel_response(decision, key);
    }

    match crate::database::transfer_cancel::apply_cancel(
        &statechain_entity.pool,
        &statechain_id,
        &row.recipient_auth_pub_key,
        row.message_posted,
        recipient_consent == RecipientConsent::Valid,
    ).await {
        Ok(true) => cancel_response(CancelDecision::Allow, None),
        // The guarded UPDATE affected no row: a claim or another cancel landed between our read and
        // our write. Re-read and report what actually happened rather than claiming a success that
        // did not occur.
        Ok(false) => {
            match crate::database::transfer_cancel::get_transfer_for_cancel(&statechain_entity.pool, &statechain_id).await {
                Ok(Some(fresh)) => {
                    let fresh_state = TransferCancelState {
                        row_exists: true,
                        key_updated: fresh.key_updated,
                        already_cancelled: fresh.cancelled,
                        is_batched: fresh.batched,
                        message_posted: fresh.message_posted,
                        claim_in_flight: fresh.claim_in_flight,
                        // The consent nonce is spent; do not re-credit it on the retry path.
                        recipient_consent: RecipientConsent::Absent,
                    };
                    let fresh_decision = decide_transfer_cancel(&fresh_state);
                    // If the fresh state would still allow, the race resolved into a state we can no
                    // longer distinguish safely — report the conservative refusal.
                    if fresh_decision.releases_lock() {
                        cancel_response(CancelDecision::ClaimInFlight, None)
                    } else {
                        cancel_response(fresh_decision, None)
                    }
                }
                Ok(None) => cancel_response(CancelDecision::NoSuchTransfer, None),
                Err(_) => status::Custom(
                    Status::ServiceUnavailable,
                    Json(json!({ "message": "transfer-state lookup unavailable; refusing to cancel (fail-closed)" })),
                ),
            }
        }
        Err(_) => status::Custom(
            Status::ServiceUnavailable,
            Json(json!({ "message": "could not record the cancellation; the transfer is unchanged" })),
        ),
    }
}