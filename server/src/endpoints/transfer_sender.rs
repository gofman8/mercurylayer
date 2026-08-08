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
        // Stale joins the CONFLICT family, not the FORBIDDEN one: the signature is good and the
        // caller is authorized — the transfer under the consent moved. The remedy is another
        // request, so it must not read as an authentication failure.
        CancelDecision::Batched
        | CancelDecision::ClaimInFlight
        | CancelDecision::RecipientConsentRequired
        | CancelDecision::RecipientConsentStale => Status::Conflict,
        CancelDecision::RecipientSignatureInvalid => Status::Forbidden,
    }
}

/// The ONE answer a cancel request that did not authenticate receives.
///
/// It is a function, and it is the only unauthenticated exit in `transfer_cancel`, because that is
/// what makes the disclosure property checkable: an unauthenticated caller learns the SAME thing for
/// a coin that has an open transfer, a coin whose transfer was claimed, a coin in a lightning-latch
/// batch, a coin with no transfer at all, and a statechain id the coordinator has never seen. One
/// status, one body, no `code`, nothing derived from the row.
///
/// That matters more than it looks. `GET /info/keylist` PUBLISHES every statechain_id, so the id
/// space is not a secret and an oracle here would be pollable for the entire coordinator: who is
/// mid-payment, whose payment completed, which coins are in a lightning latch. `cancel_status`
/// deliberately does not map anything to this response — a decision is never delivered to a caller
/// who proved nothing.
///
/// It is a 403 and not a 404 for the same reason: a 404 would itself be a statement about the coin.
fn cancel_unauthenticated() -> status::Custom<Json<Value>> {
    status::Custom(
        Status::Forbidden,
        Json(json!({ "message": "Signature does not match authentication key." })),
    )
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

    // The row is read FIRST, but nothing derived from it is emitted before the sender-auth gate
    // below. It is read first for exactly one reason: the key the sender is authenticated against is
    // RECORDED ON IT. See the `cancel_sender_auth_ordering_tests` block at the foot of this file for
    // the full argument — in short, claiming rotates the coin's live auth key to the RECIPIENT's, so
    // checking the sender against the live key locked a sender out of any answer about its own
    // transfer the moment it was claimed, and made `AlreadyClaimed` unreachable.
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

    // THE SENDER-AUTH GATE. Everything state-dependent lives behind it, and every caller that does
    // not clear it gets the one identical `cancel_unauthenticated()` answer — same status, same
    // bytes — whatever the coordinator knows about the coin, including that it has never heard of it.
    //
    // The key: the transfer's RECORDED OPENER (migration 0011), falling back to the coin's live key
    // when the row predates 0011 or does not exist. The fallback IS the pre-0011 check, so it can
    // only ever be as strict.
    let sender_authenticated = match row.as_ref().and_then(|r| r.sender_auth_pub_key) {
        Some(opener) => crate::endpoints::utils::validate_signature_nonce_given_xonly_key(
            &statechain_entity.pool,
            &transfer_cancel_request_payload.0.auth_sig,
            &statechain_id,
            CANCEL_SENDER_ENDPOINT,
            &opener,
        ).await,
        None => crate::endpoints::utils::validate_signature_nonce(
            &statechain_entity.pool,
            &transfer_cancel_request_payload.0.auth_sig,
            &statechain_id,
            CANCEL_SENDER_ENDPOINT,
        ).await,
    };

    if !sender_authenticated {
        return cancel_unauthenticated();
    }

    let row = match row {
        Some(r) => r,
        None => return cancel_response(CancelDecision::NoSuchTransfer, None),
    };

    // The recipient leg. It counts as consent ONLY if it verifies under the key the coordinator
    // recorded for this transfer — a signature under any other key, including one the sender picked,
    // is `Invalid`. `recipient_auth_pub_key` must therefore MATCH the record; it is an assertion the
    // caller makes and the server checks, never a substitute for the record.
    // The transfer-INSTANCE identity, recomputed FROM THE ROW. The caller's
    // `recipient_transfer_digest` only states which instance its signature claims to be for; this is
    // the authority it is checked against. Recomputing (rather than trusting) is what makes the
    // binding meaningful — a sender relaying its recipient's token cannot restate what that token
    // covers.
    let row_digest = row.encrypted_transfer_msg.as_deref().map(|msg| {
        mercurylib::transfer::cancel::transfer_consent_digest(
            &statechain_id,
            &row.recipient_auth_pub_key.to_string(),
            msg,
        )
    });

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
            // [INSTANCE BINDING] A sender may re-address a coin to the SAME recipient key
            // (`has_open_transfer_to_other_auth` refuses only a DIFFERENT key), and
            // `insert_new_transfer` implements that as DELETE-then-INSERT with a fresh x1. So
            // (coin, recipient key, nonce) does not identify a transfer across time: without this
            // check a consent the recipient gave up on transfer T1 would withdraw its replacement
            // T2, which the recipient can see in its mailbox and believes is live. The tombstone
            // does not cover this ordering — no cancellation has happened yet when the re-address
            // occurs. Checked BEFORE the signature so a stale replay cannot consume the recipient's
            // fresh nonce as a side effect, and so `stale` stays distinguishable from `bad
            // signature`.
            let stale = mercurylib::transfer::cancel::consent_binding_is_stale(
                transfer_cancel_request_payload.0.recipient_transfer_digest.as_deref(),
                row_digest.as_deref(),
            );
            if !key_matches {
                RecipientConsent::Invalid
            } else if stale {
                RecipientConsent::Stale
            } else if crate::endpoints::utils::validate_signature_nonce_given_public_key(
                &statechain_entity.pool,
                sig,
                &statechain_id,
                // The digest rides INSIDE the endpoint string, so the existing
                // `sha256(nonce|endpoint)` construction carries the binding with no second code
                // path to drift out of step with the client's.
                &mercurylib::transfer::cancel::recipient_consent_endpoint(
                    row_digest.as_deref().unwrap_or_default(),
                ),
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

// ==================================================================================================
// THE CONSENT BINDING'S AUTHORITY, asserted on the endpoint's own source.
//
// `transfer_cancel` cannot be CALLED from a unit test: it needs a Rocket `State<StateChainEntity>`
// carrying a live `sqlx::PgPool`, and this repository has no test database. So the one property that
// makes the transfer-instance binding worth anything is pinned on the source text instead — stated
// plainly, because a source-shape assertion is weaker than a behavioural one and pretending
// otherwise would be the same sin it guards against.
//
// THE PROPERTY: the digest the recipient's signature is verified against must be the one RECOMPUTED
// FROM THE ROW. The payload's `recipient_transfer_digest` is the CALLER's copy — it says which
// transfer instance the signature claims to be for, and the sender relaying its recipient's token
// controls it completely. If it ever reaches the verification path the sender simply restates what
// its recipient signed, and the binding evaporates while every other test stays green: `mercurylib`'s
// adversarial Coordinator carries its own mirror of this classification, and the mirror would remain
// honest while the server went blind.
//
// The checker is exercised against DELIBERATELY BROKEN fixtures as well as the real source, because
// a pin that has quietly stopped discriminating is worse than no pin — it reports green.
// ==================================================================================================
#[cfg(test)]
mod cancel_binding_authority_tests {
    /// The caller-supplied field. Its ONLY legitimate use is as the `supplied` argument of
    /// `consent_binding_is_stale`, where it is compared AGAINST the row's value.
    const CALLER_FIELD: &str = "recipient_transfer_digest";

    /// Code with comments removed and whitespace collapsed.
    ///
    /// Comments MUST go: this file explains the binding at length and names the caller's field while
    /// doing so, and a checker that counts prose is measuring the wrong thing — it would fire on a
    /// clarified comment and stay silent on a changed argument.
    fn code_only(body: &str) -> String {
        body.lines()
            .map(|l| match l.find("//") {
                Some(i) => &l[..i],
                None => l,
            })
            .collect::<Vec<_>>()
            .join(" ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Every way the binding can lose its authority, in one place. `Err` carries the reason.
    fn check_row_is_the_authority(body: &str) -> Result<(), String> {
        let code = code_only(body);

        // 1. The authority must be DERIVED FROM THE ROW's conveyed material. Without this the rest
        //    is theatre: a `row_digest` computed from the payload would satisfy every later check.
        if !code.contains("row.encrypted_transfer_msg") {
            return Err("the digest must be computed from `row.encrypted_transfer_msg` — the DB's \
                        own copy of the conveyed ciphertext"
                .to_string());
        }

        // 2. THE ONE THAT CATCHES THE SUBSTITUTION. The caller's field may appear EXACTLY ONCE in
        //    the whole handler. One use is the comparison; a second use is, by construction, the
        //    caller's copy being consulted as though it were authoritative.
        let uses = code.matches(CALLER_FIELD).count();
        if uses != 1 {
            return Err(format!(
                "`{CALLER_FIELD}` (the CALLER's copy) is used {uses} times in code; it may be used \
                 exactly once, as the `supplied` argument of `consent_binding_is_stale`. Every \
                 other use is the sender telling the coordinator what its recipient consented to."
            ));
        }

        // 3. ... and that one use must be the FIRST argument of the staleness comparison, with the
        //    row's value second. Argument order is the difference between "check the caller against
        //    the row" and "check the row against the caller".
        let comparison = format!(
            "consent_binding_is_stale( transfer_cancel_request_payload.0.{CALLER_FIELD}.as_deref(), \
             row_digest.as_deref(), )"
        );
        if !code.contains(&comparison) {
            return Err(format!(
                "the single use of `{CALLER_FIELD}` must be the `supplied` argument of \
                 `consent_binding_is_stale`, checked against `row_digest`"
            ));
        }

        // 4. The endpoint string the signature is verified against must be built from the ROW's
        //    digest — this is where the binding actually enters the signed bytes.
        if !code.contains("recipient_consent_endpoint( row_digest") {
            return Err("the endpoint string the recipient signature is verified against must be \
                        built from `row_digest`; anything else lets the sender restate what its \
                        recipient signed"
                .to_string());
        }

        // 5. The staleness test must precede the nonce-consuming signature check, so replaying a
        //    stale token cannot burn a nonce the recipient minted for a transfer that is still live.
        let stale_at = code.find("consent_binding_is_stale").expect("checked above");
        let verify_at = code
            .find("validate_signature_nonce_given_public_key")
            .ok_or("the recipient leg must be verified with a single-use nonce")?;
        if stale_at >= verify_at {
            return Err("the staleness check must come BEFORE \
                        `validate_signature_nonce_given_public_key`, otherwise a sender replaying a \
                        stale token consumes a nonce its recipient minted for a live transfer"
                .to_string());
        }

        Ok(())
    }

    /// The body of `transfer_cancel`, as it is actually compiled.
    fn transfer_cancel_body() -> &'static str {
        const SIGNATURE: &str = "pub async fn transfer_cancel(";
        let src = include_str!("transfer_sender.rs");
        let start = src.find(SIGNATURE).expect("transfer_cancel must exist");
        let rest = &src[start..];
        &rest[..rest.find("\n}\n").expect("transfer_cancel must be terminated")]
    }

    /// **THE PIN.** The coordinator verifies the consent against the row, never against the caller.
    #[test]
    fn the_row_not_the_caller_is_the_authority_on_what_was_consented_to() {
        if let Err(why) = check_row_is_the_authority(transfer_cancel_body()) {
            panic!("transfer_cancel no longer binds consent to the ROW's transfer instance: {why}");
        }
    }

    /// **THE PIN'S OWN TEST.** Each fixture is a real way the binding dies, derived from the real
    /// body so the only difference is the defect itself.
    #[test]
    fn the_pin_rejects_every_way_the_binding_can_die() {
        let real = transfer_cancel_body();
        assert!(check_row_is_the_authority(real).is_ok(), "precondition: the real body passes");

        // (a) THE SUBSTITUTION: the signature is verified against the CALLER's digest. One
        //     identifier wide, and invisible to every other test in this repository.
        let substituted = real.replacen(
            "recipient_consent_endpoint(\n                    row_digest",
            &format!(
                "recipient_consent_endpoint(\n                    \
                 transfer_cancel_request_payload.0.{CALLER_FIELD}"
            ),
            1,
        );
        assert_ne!(substituted, real, "the fixture must actually differ from the real body");
        let err = check_row_is_the_authority(&substituted)
            .expect_err("verifying against the CALLER's digest must be rejected");
        assert!(err.contains("used 2 times"), "wrong reason: {err}");

        // (b) THE AUTHORITY IS NOT THE ROW: the digest is computed from something else entirely.
        let not_from_row = real.replace("row.encrypted_transfer_msg", "some_other_source");
        assert!(check_row_is_the_authority(&not_from_row).is_err());

        // (c) NO COMPARISON AT ALL — the shape this endpoint shipped with, where a consent for the
        //     coin cancelled any transfer of it.
        let unbound = real.replace("consent_binding_is_stale", "never_stale_at_all");
        assert!(check_row_is_the_authority(&unbound).is_err());

        // (d) THE ARGUMENTS SWAPPED: the row checked against the caller reads the same to a careless
        //     eye. (`consent_binding_is_stale` is symmetric, so only this pin catches it — it is a
        //     readability defect today and the seed of a real one the moment the two sides stop
        //     being treated identically.)
        let swapped = real.replace(
            &format!(
                "consent_binding_is_stale(\n                transfer_cancel_request_payload.0.{CALLER_FIELD}.as_deref(),\n                row_digest.as_deref(),"
            ),
            &format!(
                "consent_binding_is_stale(\n                row_digest.as_deref(),\n                transfer_cancel_request_payload.0.{CALLER_FIELD}.as_deref(),"
            ),
        );
        assert_ne!(swapped, real, "the swapped fixture must actually differ");
        assert!(check_row_is_the_authority(&swapped).is_err());

        // (e) THE ORDERING: staleness checked only AFTER the nonce is consumed, so a stale replay
        //     burns a nonce the recipient minted for a transfer that is still live.
        let late = real
            .replace("consent_binding_is_stale", "ZZZ_SWAP")
            .replace("validate_signature_nonce_given_public_key", "consent_binding_is_stale")
            .replace("ZZZ_SWAP", "validate_signature_nonce_given_public_key");
        assert!(check_row_is_the_authority(&late).is_err());

        // (f) A COMMENT-ONLY edit must NOT be reported as a defect: a pin that fires on prose gets
        //     switched off by the next person who touches the file.
        let recommented = format!("{real}\n    // {CALLER_FIELD} {CALLER_FIELD} {CALLER_FIELD}");
        assert!(
            check_row_is_the_authority(&recommented).is_ok(),
            "the pin must measure code, not comments"
        );
    }
}

// ==================================================================================================
// WHICH KEY AUTHENTICATES THE SENDER LEG OF A CANCELLATION — and why the ordering is what it is.
//
// THE DEFECT THIS PINS. `transfer_cancel` used to verify the sender's signature against the COIN's
// live auth key and return early on failure, before it ever read the transfer row. But claiming
// ROTATES that key: `database::transfer_receiver::update_statechain` sets
// `statechain_data.auth_xonly_public_key` to the RECIPIENT's key. So the moment a recipient claimed,
// the sender's own signature stopped matching, and a sender asking about its own transfer got the
// generic "Signature does not match authentication key." instead of the answer.
// `CancelDecision::AlreadyClaimed` — row 3 of the four-row authorization table this whole feature is
// specified around, mapped to 410 — could therefore NEVER FIRE for the ordinary case it exists to
// describe. The row was dead code that read as live specification.
//
// THE OPTIONS, AND WHY THIS ONE.
//
//   (a) Read the row and decide BEFORE authenticating. Rejected. `GET /info/keylist` publishes
//       every statechain_id, and a transfer address hands one to a counterparty in the clear, so the
//       id space is public. Deciding first would turn this endpoint into an unauthenticated oracle
//       over that public space: anyone could poll it and learn, for any coin, whether a transfer is
//       open, whether it has been claimed, and whether it is in a lightning-latch batch. That is a
//       live payment-status feed for the whole coordinator, and it is exactly what the ordering
//       comment on the auth gate was there to prevent.
//   (b) Authenticate first, and on MISMATCH consult the row for a more specific answer. Rejected for
//       the same reason, only narrower: the caller still proves nothing, it merely has to send a
//       signature that fails. The oracle survives with one extra step.
//   (c) CHOSEN. Authenticate the sender against the key that OPENED THE TRANSFER, recorded on the
//       row when `insert_new_transfer` wrote it, rather than against the coin's live key. The sender
//       stays authenticated to its own transfer after that transfer is claimed, and the decision
//       then runs normally and says `AlreadyClaimed`.
//
// WHY (c) DISCLOSES NOTHING NEW. Only a caller holding the OPENER's private key gets any answer at
// all; every other caller gets the identical `cancel_unauthenticated()` response it got before —
// same status, same bytes, on every state of the coin, including a coin that does not exist. The row
// is now READ before the gate, but nothing derived from it is emitted before the gate: the decision
// stays behind it, and `check_sender_leg_is_bound_to_the_row` fails the build if it ever moves.
//
// WHY (c) IS NOT MORE PERMISSIVE — the argument in full, because this endpoint releases the
// pending-transfer lock, which is the only thing standing between a conveyed-but-unclaimed recipient
// and a sender who co-signs a rival state:
//
//   1. A cancellation releases the lock ONLY on `CancelDecision::Allow` (`releases_lock`).
//   2. `decide_transfer_cancel` returns `Allow` only when `key_updated == false`; `key_updated` is
//      tested second, immediately after `row_exists`, and short-circuits to `AlreadyClaimed`.
//      (Pinned behaviourally in `mercurylib::transfer::cancel`:
//      `every_lock_releasing_decision_requires_an_unclaimed_row`.)
//   3. The coin's auth key rotates in exactly ONE place — `update_statechain` — which sets
//      `auth_xonly_public_key` and `key_updated = true` in the SAME transaction. (Pinned in
//      `crate::database::transfer_receiver`: `the_key_rotation_and_the_claim_flag_are_one_transaction`.)
//   4. Therefore `key_updated == false` implies the coin's live key has not moved since the row was
//      opened, which implies the row's recorded opener IS the live key.
//   5. Therefore the set of callers who can reach a lock-RELEASING decision is byte-for-byte the set
//      that could reach one before. A superseded key can only ever reach a decision that refuses.
//
// The same argument covers the one refusal that discloses anything — `RecipientConsentRequired`
// names the recipient's auth key — because it too requires `key_updated == false`, so it is never
// reachable through a superseded key either.
//
// WHAT IS ACTUALLY NEW, stated plainly: an unauthenticated request now costs one indexed SELECT
// before it is rejected, where it used to cost one. `GET /auth/challenge/<id>` is already
// unauthenticated and already writes a row, so this is not a new class of exposure, but it is a
// change and it is not nothing.
//
// A row written before migration 0011 has no recorded opener. It falls back to the coin's live key —
// EXACTLY the pre-0011 behaviour, and pinned as such, so the fallback can never be quietly widened
// into "no opener recorded means anyone may ask".
// ==================================================================================================
#[cfg(test)]
mod cancel_sender_auth_ordering_tests {
    /// Code with comments removed and whitespace collapsed. Comments MUST go: the block above names
    /// every identifier these checks look for, and a pin that counts prose measures the wrong thing.
    fn code_only(body: &str) -> String {
        body.lines()
            .map(|l| match l.find("//") {
                Some(i) => &l[..i],
                None => l,
            })
            .collect::<Vec<_>>()
            .join(" ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn transfer_cancel_body() -> &'static str {
        const SIGNATURE: &str = "pub async fn transfer_cancel(";
        let src = include_str!("transfer_sender.rs");
        let start = src.find(SIGNATURE).expect("transfer_cancel must exist");
        let rest = &src[start..];
        &rest[..rest.find("\n}\n").expect("transfer_cancel must be terminated")]
    }

    /// Every way the sender leg's ordering can go wrong, in one place. `Err` carries the reason.
    fn check_sender_leg_is_bound_to_the_row(body: &str) -> Result<(), String> {
        let code = code_only(body);

        // 1. There is exactly ONE unauthenticated exit, and it is a single shared response. Two
        //    exits is how a state-dependent answer gets in: one of them starts carrying a reason.
        let unauth_uses = code.matches("cancel_unauthenticated()").count();
        if unauth_uses != 1 {
            return Err(format!(
                "the unauthenticated answer must come from exactly ONE place \
                 (`cancel_unauthenticated()`), found {unauth_uses}: every extra exit is somewhere a \
                 state-dependent answer can be given to a caller who proved nothing"
            ));
        }
        let gate = code.find("cancel_unauthenticated()").expect("counted above");

        // 2. The row is read BEFORE the gate — that is the whole point of option (c): the key the
        //    sender is checked against comes off the row.
        let row_read = code
            .find("get_transfer_for_cancel")
            .ok_or("the transfer row must be read")?;
        if row_read >= gate {
            return Err("the transfer row must be read BEFORE the sender-auth gate; the key the \
                        sender is authenticated against is recorded ON that row"
                .to_string());
        }

        // 3. THE ONE THAT MAKES ROW 3 REACHABLE. The sender leg verifies against the key that OPENED
        //    the transfer, taken off the row — not against the coin's live key, which a claim has by
        //    then rotated to the RECIPIENT's.
        if !code.contains("validate_signature_nonce_given_xonly_key(") {
            return Err("the sender leg must be verified against a NAMED key (the row's recorded \
                        opener), not against whatever key the coin currently carries — a claim \
                        rotates that key to the recipient's, which is what made `AlreadyClaimed` \
                        unreachable"
                .to_string());
        }
        if !code.contains("sender_auth_pub_key") {
            return Err("the key verified against must come from the ROW's `sender_auth_pub_key`"
                .to_string());
        }

        // 4. THE DISCLOSURE PIN. The decision stays BEHIND the gate. Moving it in front is option
        //    (a), which hands transfer and claim status to any caller who can guess a statechain_id
        //    — and `/info/keylist` publishes them, so guessing is not required.
        let decide = code
            .find("decide_transfer_cancel(")
            .ok_or("the decision must be taken")?;
        if decide < gate {
            return Err("`decide_transfer_cancel` must stay BEHIND the sender-auth gate: a decision \
                        returned to a caller who proved nothing turns this endpoint into a \
                        claim-status oracle over a PUBLISHED statechain-id space"
                .to_string());
        }

        // 5. ... and so must every other read of the row's contents. `recipient_auth_pub_key` is the
        //    coin -> next-owner link; it is not published anywhere unauthenticated and must not
        //    start being.
        let key_read = code
            .find("row.recipient_auth_pub_key")
            .ok_or("the recorded recipient key must be read from the row")?;
        if key_read < gate {
            return Err("the row's recipient key must not be touched before the sender-auth gate: \
                        the coin -> next-owner link is not published unauthenticated"
                .to_string());
        }

        // 6. The pre-0011 fallback must be exactly the OLD check, and nothing weaker. A row with no
        //    recorded opener falls back to the coin's live key — not to "anyone may ask".
        if !code.contains("validate_signature_nonce(") {
            return Err("a row with no recorded opener (written before migration 0011) must fall \
                        back to the coin's live key — the pre-0011 behaviour — not to no check at all"
                .to_string());
        }

        Ok(())
    }

    /// **THE PIN.**
    #[test]
    fn the_sender_leg_is_authenticated_against_the_key_that_opened_the_transfer() {
        if let Err(why) = check_sender_leg_is_bound_to_the_row(transfer_cancel_body()) {
            panic!("transfer_cancel's sender leg is no longer bound to the transfer row: {why}");
        }
    }

    /// **THE PIN'S OWN TEST.** A pin that has quietly stopped discriminating reports green, which is
    /// worse than no pin — so each fixture is a real way this ordering dies, derived from the real
    /// body so the only difference is the defect itself.
    #[test]
    fn the_ordering_pin_rejects_every_way_it_can_die() {
        let real = transfer_cancel_body();
        assert!(
            check_sender_leg_is_bound_to_the_row(real).is_ok(),
            "precondition: the real body passes"
        );

        // (a) OPTION (a) SMUGGLED IN: the decision taken before the gate. The endpoint still works
        //     for honest callers, and now answers everyone.
        let decide_first = format!(
            "pub async fn transfer_cancel( decide_transfer_cancel(&state); {}",
            &real["pub async fn transfer_cancel(".len()..]
        );
        assert!(
            check_sender_leg_is_bound_to_the_row(&decide_first).is_err(),
            "deciding before the gate must be rejected"
        );

        // (b) THE ORIGINAL DEFECT: the sender checked against the coin's live key only.
        let live_key_only = real.replace("validate_signature_nonce_given_xonly_key(", "validate_signature_nonce(");
        assert_ne!(live_key_only, real, "the fixture must actually differ");
        assert!(check_sender_leg_is_bound_to_the_row(&live_key_only).is_err());

        // (c) THE KEY NO LONGER COMES OFF THE ROW.
        let not_from_row = real.replace("sender_auth_pub_key", "some_other_key");
        assert_ne!(not_from_row, real, "the fixture must actually differ");
        assert!(check_sender_leg_is_bound_to_the_row(&not_from_row).is_err());

        // (d) A SECOND UNAUTHENTICATED EXIT — the shape in which a state-dependent answer creeps
        //     back in ahead of the gate.
        let two_exits = real.replace(
            "return cancel_unauthenticated();",
            "return cancel_unauthenticated(); let _ = cancel_unauthenticated();",
        );
        assert_ne!(two_exits, real, "the fixture must actually differ");
        assert!(check_sender_leg_is_bound_to_the_row(&two_exits).is_err());

        // (e) THE FALLBACK WIDENED: a row with no recorded opener waved through.
        let no_fallback = real.replace("validate_signature_nonce(", "always_authorized(");
        assert_ne!(no_fallback, real, "the fixture must actually differ");
        assert!(check_sender_leg_is_bound_to_the_row(&no_fallback).is_err());

        // (f) A COMMENT-ONLY edit must NOT be reported as a defect: a pin that fires on prose gets
        //     switched off by the next person who touches the file.
        let recommented =
            format!("{real}\n    // decide_transfer_cancel( cancel_unauthenticated() sender_auth_pub_key");
        assert!(
            check_sender_leg_is_bound_to_the_row(&recommented).is_ok(),
            "the pin must measure code, not comments"
        );
    }
}

// ==================================================================================================
// WHAT AN UNAUTHENTICATED CALLER MAY LEARN, AND THE FOUR ROWS' STATUSES.
//
// The disclosure half is pinned BEHAVIOURALLY (these functions take no pool), so it cannot rot into
// a source-shape approximation: `cancel_unauthenticated()` is called and its status and body are
// compared against every decision response the endpoint can produce.
// ==================================================================================================
#[cfg(test)]
mod cancel_disclosure_tests {
    use super::{cancel_response, cancel_status, cancel_unauthenticated};
    use mercurylib::transfer::cancel::CancelDecision;
    use rocket::http::Status;

    const EVERY_DECISION: [CancelDecision; 9] = [
        CancelDecision::Allow,
        CancelDecision::AlreadyCancelled,
        CancelDecision::NoSuchTransfer,
        CancelDecision::AlreadyClaimed,
        CancelDecision::Batched,
        CancelDecision::ClaimInFlight,
        CancelDecision::RecipientConsentRequired,
        CancelDecision::RecipientSignatureInvalid,
        CancelDecision::RecipientConsentStale,
    ];

    fn body_of(r: &rocket::response::status::Custom<rocket::serde::json::Json<serde_json::Value>>) -> serde_json::Value {
        r.1.0.clone()
    }

    /// **THE DISCLOSURE PIN.** The unauthenticated answer states ONE thing — that the signature did
    /// not match — and nothing about the coin. No `code`, no recipient key, no extra field a later
    /// change could quietly start filling in.
    #[test]
    fn an_unauthenticated_caller_learns_only_that_its_signature_did_not_match() {
        let r = cancel_unauthenticated();
        assert_eq!(r.0, Status::Forbidden, "a 404 would itself be a statement about the coin");

        let body = body_of(&r);
        let obj = body.as_object().expect("the body is a JSON object");
        assert_eq!(
            obj.keys().collect::<Vec<_>>(),
            vec!["message"],
            "the unauthenticated body must carry EXACTLY one field: {body}"
        );
        assert_eq!(obj["message"], "Signature does not match authentication key.");

        // Nothing in it can be read as a rule that fired, or as a key.
        for d in EVERY_DECISION {
            assert!(!body.to_string().contains(d.code()), "{} leaked into it: {body}", d.code());
        }
        assert!(obj.get("recipient_auth_pub_key").is_none());
    }

    /// ... and it is DISTINGUISHABLE from every authenticated answer, so a caller can always tell
    /// "you are not the sender" from "here is the state of your transfer". Every decision body
    /// carries a `code`; the unauthenticated one never does.
    #[test]
    fn the_unauthenticated_answer_is_never_confusable_with_a_decision() {
        let unauth = body_of(&cancel_unauthenticated());
        for d in EVERY_DECISION {
            let decided = body_of(&cancel_response(d, None));
            assert!(
                decided.get("code").is_some(),
                "{d:?} must name the rule that fired: {decided}"
            );
            assert_ne!(decided, unauth, "{d:?} is indistinguishable from the unauthenticated answer");
        }
    }

    /// No decision is EVER delivered through the unauthenticated response — i.e. the two paths do
    /// not share a body. (They may share the 403 status: `RecipientSignatureInvalid` is a 403 too,
    /// but it is delivered only to a caller whose OWN signature verified, and it carries a code.)
    #[test]
    fn no_decision_is_delivered_as_the_unauthenticated_answer() {
        let unauth = cancel_unauthenticated();
        for d in EVERY_DECISION {
            let r = cancel_response(d, None);
            assert!(
                r.0 != unauth.0 || body_of(&r) != body_of(&unauth),
                "{d:?} is served as the unauthenticated answer"
            );
        }
    }

    /// The one refusal that names the recipient key names it ONLY on that refusal — the coin ->
    /// next-owner link is not published anywhere else, because it would let an observer correlate a
    /// coin with its next owner's mailbox.
    #[test]
    fn only_the_consent_refusal_ever_names_the_recipient_key() {
        for d in EVERY_DECISION {
            let named = body_of(&cancel_response(
                d,
                (d == CancelDecision::RecipientConsentRequired).then(|| "02abc".to_string()),
            ));
            let key = named.get("recipient_auth_pub_key").and_then(|v| v.as_str());
            if d == CancelDecision::RecipientConsentRequired {
                assert_eq!(key, Some("02abc"), "the sender cannot act on this refusal otherwise");
            } else {
                assert_eq!(key, None, "{d:?} must not name the recipient key: {named}");
            }
        }
    }
}

// ==================================================================================================
// THE FOUR-ROW TABLE'S HTTP STATUSES. The decisions themselves are pinned in
// `mercurylib::transfer::cancel`; this is the coordinator's half of the same table.
// ==================================================================================================
#[cfg(test)]
mod cancel_table_row_tests {
    use super::cancel_status;
    use mercurylib::transfer::cancel::{decide_transfer_cancel, CancelDecision, RecipientConsent, TransferCancelState};
    use rocket::http::Status;

    fn row(f: impl FnOnce(&mut TransferCancelState)) -> TransferCancelState {
        let mut s = TransferCancelState { row_exists: true, ..TransferCancelState::empty() };
        f(&mut s);
        s
    }

    /// Each documented row, reached from a state a real coordinator read produces, all the way to
    /// the status on the wire. Row 3 is the one this file's ordering block exists for: before
    /// migration 0011 a sender could not authenticate to reach it at all, so the 410 was dead.
    #[test]
    fn every_row_of_the_table_is_reachable_and_returns_its_documented_status() {
        let cases: [(&str, TransferCancelState, CancelDecision, Status); 4] = [
            ("opened, never conveyed — sender alone", row(|_| {}), CancelDecision::Allow, Status::Ok),
            (
                "conveyed, unclaimed — recipient must co-sign",
                row(|s| s.message_posted = true),
                CancelDecision::RecipientConsentRequired,
                Status::Conflict,
            ),
            (
                "claimed — terminal",
                row(|s| {
                    s.message_posted = true;
                    s.key_updated = true;
                }),
                CancelDecision::AlreadyClaimed,
                Status::Gone,
            ),
            (
                "batched (lightning latch) — terminal",
                row(|s| {
                    s.message_posted = true;
                    s.is_batched = true;
                }),
                CancelDecision::Batched,
                Status::Conflict,
            ),
        ];

        for (what, state, expected, status) in cases {
            let d = decide_transfer_cancel(&state);
            assert_eq!(d, expected, "row '{what}' no longer decides as documented");
            assert_eq!(cancel_status(d), status, "row '{what}' no longer maps to its status");
        }
    }

    /// A cooperative cancellation of row 2 is the one path that turns a refusal into a release, and
    /// it needs the RECORDED recipient's signature — nothing else in the table can be talked round.
    #[test]
    fn only_row_2_can_be_turned_into_a_release_and_only_by_the_recipient() {
        let consented = row(|s| {
            s.message_posted = true;
            s.recipient_consent = RecipientConsent::Valid;
        });
        assert_eq!(decide_transfer_cancel(&consented), CancelDecision::Allow);

        for terminal in [
            row(|s| {
                s.message_posted = true;
                s.key_updated = true;
                s.recipient_consent = RecipientConsent::Valid;
            }),
            row(|s| {
                s.message_posted = true;
                s.is_batched = true;
                s.recipient_consent = RecipientConsent::Valid;
            }),
        ] {
            assert!(
                !decide_transfer_cancel(&terminal).releases_lock(),
                "a terminal row must not be released by consent: {terminal:?}"
            );
        }
    }
}
