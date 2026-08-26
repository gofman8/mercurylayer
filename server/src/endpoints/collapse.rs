//! **[REQ-56 / REQ-82] The two routes a tree close needs, and why neither could be `sign/*`.**
//!
//! The SE has had a working `collapse_grant` since #169: it checks that `C` pays every unreleased
//! frontier leaf its full funding value out of THIS root's funding output, checks the aggregate
//! before it touches the secnonce, binds the session to the disclosed transaction, and freezes the
//! root in the same database transaction that issues the signature. Its refusals are measured — the
//! probe's eight cases resolve to six distinct gates.
//!
//! **Its ACCEPT path had never run once, and the reason was two missing links measured rather than
//! guessed at.**
//!
//! 1. **Nothing could call it.** There was no client-side caller and no server route: the lockbox
//!    listens on its own port, which no client reaches. Every measurement of `collapse_grant` came
//!    from a Python probe seeding the SE's database directly.
//! 2. **And had there been one, it would have been refused for want of a nonce.** `collapse_grant`
//!    consumes a secnonce that only `sign/first` mints, and `sign/first` refuses `410 Gone` once a
//!    coin's spend budget is exhausted. A root worth collapsing is a root that has been SPLIT, and
//!    splitting is precisely what exhausts the budget. Measured on this environment's live database:
//!    of the **14** roots holding more than one leaf — the only genuine trees present — **13 are
//!    known to the server and every one has an exhausted budget. Not one could have a nonce minted.**
//!    (The 67 roots that could are single-leaf: a coin that is its own only leaf, where "collapse"
//!    means paying yourself your own coin.)
//!
//! So the accept path was unreachable for exactly the roots the collapse exists to close, and no
//! amount of reading the route would have said so — the gate that blocks it lives in a different
//! process, in a different language, behind a different requirement.
//!
//! **WHY A BUDGET-EXEMPT NONCE IS SAFE, which is the only real question here.** The budget gate
//! exists to stop a terminal node making a SECOND conflicting ordinary spend (INV-19). A collapse is
//! deliberately a second spend of `F` — that is what closing a tree IS — so the gate cannot be
//! applied to it without forbidding the operation entirely. What makes the exemption safe is that the
//! nonce it mints cannot be diverted:
//!
//! * `sign/second` re-checks the budget itself ([S1] in `sign.rs`), so a collapse nonce presented to
//!   the ordinary route is refused there, by that route's own gate, on the same exhausted budget;
//! * `collapse_grant` will only spend it on a transaction that pays every unreleased leaf in full out
//!   of this root's own funding output, under this root's own aggregate, bound to this exact session;
//! * and it can happen at most once, because the freeze is written in the same transaction as the
//!   signature and no path clears it.
//!
//! The collapse's gates are therefore strictly more specific than the one being skipped, not weaker
//! than it. Every OTHER gate `sign/first` applies is kept, including the pending-transfer lock and
//! fail-closed behaviour on any database error.

use rocket::serde::json::{json, Json, Value};
use rocket::response::status;
use rocket::http::Status;
use rocket::State;

use crate::server::StateChainEntity;

/// Map a lockbox HTTP status onto rocket's, so a refusal reaches the client as ITSELF.
///
/// Flattening the SE's status into a generic 500 would erase the whole point of the route: its
/// refusals are six distinct, named gates, and a client that cannot tell `403 does not pay every
/// leaf` from `400 session does not reproduce` cannot act on either.
fn passthrough_status(code: u16) -> Status {
    Status::from_code(code).unwrap_or(Status::BadGateway)
}

/// Resolve the enclave holding a coin, or the error to return.
async fn enclave_url_for(
    statechain_entity: &StateChainEntity,
    statechain_id: &str,
) -> Result<String, status::Custom<Json<Value>>> {
    let config = crate::server_config::ServerConfig::load();
    match crate::database::utils::get_enclave_index_from_database(&statechain_entity.pool, statechain_id).await {
        Some(index) => match config.enclaves.get(index as usize) {
            Some(e) => Ok(e.url.clone()),
            None => Err(status::Custom(
                Status::InternalServerError,
                Json(json!({ "message": format!("enclave index {} is out of range", index) })),
            )),
        },
        None => Err(status::Custom(
            Status::InternalServerError,
            Json(json!({ "message": format!("Enclave index for statechain {} ID not found.", statechain_id) })),
        )),
    }
}

/// **Mint the secnonce a collapse will consume.** See the module header for why this route exists at
/// all rather than the caller using `sign/first`.
#[post("/collapse/first", format = "json", data = "<sign_first_request_payload>")]
pub async fn collapse_first(
    statechain_entity: &State<StateChainEntity>,
    sign_first_request_payload: Json<mercurylib::transaction::SignFirstRequestPayload>,
) -> status::Custom<Json<Value>> {
    let statechain_id = sign_first_request_payload.0.statechain_id.clone();
    let signed_statechain_id = sign_first_request_payload.0.signed_statechain_id.clone();
    let statechain_entity = statechain_entity.inner();

    if !crate::endpoints::utils::validate_signature(&statechain_entity.pool, &signed_statechain_id, &statechain_id).await {
        return status::Custom(
            Status::Unauthorized,
            Json(json!({ "message": "Signature does not match authentication key." })),
        );
    }

    // The pending-transfer lock is KEPT, and fails closed. A coin whose transfer is open has a
    // receiver who may be about to own it; issuing collapse material against it would be the same
    // TOCTOU the ordinary route closes, with the same victim.
    match crate::database::transfer_sender::has_open_transfer(
        &statechain_entity.pool,
        &statechain_id,
        crate::server_config::ServerConfig::load().batch_timeout as i64,
    ).await {
        Ok(true) => {
            return status::Custom(
                Status::Conflict,
                Json(json!({ "message": "coin has an open transfer (SE refuses collapse material until it completes or expires)" })),
            );
        }
        Ok(false) => {}
        Err(_) => {
            return status::Custom(
                Status::ServiceUnavailable,
                Json(json!({ "message": "SE co-sign state temporarily unavailable; refusing (fail-closed)" })),
            );
        }
    }

    let lockbox_endpoint = match enclave_url_for(statechain_entity, &statechain_id).await {
        Ok(u) => u,
        Err(e) => return e,
    };

    // A dangling sign/first is re-served rather than minting a second nonce, exactly as on the
    // ordinary route: two nonces for one coin is the reuse hazard this design refuses everywhere.
    if let Some(existing) =
        crate::database::sign::get_server_pubnonce_from_null_challenge(&statechain_entity.pool, &statechain_id).await
    {
        return status::Custom(
            Status::Ok,
            Json(json!(mercurylib::transaction::SignFirstResponsePayload { server_pubnonce: existing })),
        );
    }

    let client = reqwest::Client::new();
    let value = match client
        .post(&format!("{}/{}", lockbox_endpoint, "get_public_nonce"))
        .json(&sign_first_request_payload.0)
        .send()
        .await
    {
        Ok(r) => match r.text().await {
            Ok(t) => t,
            Err(err) => {
                return status::Custom(
                    Status::BadGateway,
                    Json(json!({ "message": format!("could not read the enclave's reply: {}", err) })),
                )
            }
        },
        Err(err) => {
            return status::Custom(
                Status::InternalServerError,
                Json(json!({ "error": "Internal Server Error", "message": err.to_string() })),
            )
        }
    };

    let response: mercurylib::transaction::SignFirstResponsePayload = match serde_json::from_str(&value) {
        Ok(r) => r,
        Err(_) => {
            return status::Custom(
                Status::BadGateway,
                Json(json!({ "message": format!("enclave returned an unparseable sign/first response: {}", value) })),
            )
        }
    };

    let mut server_pubnonce_hex = response.server_pubnonce.clone();
    if server_pubnonce_hex.starts_with("0x") {
        server_pubnonce_hex = server_pubnonce_hex[2..].to_string();
    }
    crate::database::sign::insert_new_signature_data(&statechain_entity.pool, &server_pubnonce_hex, &statechain_id).await;

    status::Custom(Status::Ok, Json(json!(response)))
}

/// **Ask the SE for its half of a collapse.** Forwards the request whole and returns the SE's answer
/// — status and body — unchanged.
///
/// Nothing is decided here. Every gate that matters is the SE's, and the SE is the only party that
/// can see the leaf set, the funding outpoint, the aggregate and the freeze. A server-side
/// pre-judgement would be a second opinion that can only ever disagree with the one that counts.
#[post("/collapse_grant", format = "json", data = "<payload>")]
pub async fn collapse_grant(
    statechain_entity: &State<StateChainEntity>,
    payload: Json<mercurylib::transaction::CollapseGrantRequestPayload>,
) -> status::Custom<Json<Value>> {
    let statechain_entity = statechain_entity.inner();
    let signer_id = payload.0.statechain_id.clone();
    let root_id = payload.0.root_statechain_id.clone();
    let signed_statechain_id = payload.0.signed_statechain_id.clone();

    if !crate::endpoints::utils::validate_signature(&statechain_entity.pool, &signed_statechain_id, &signer_id).await {
        return status::Custom(
            Status::Unauthorized,
            Json(json!({ "message": "Signature does not match authentication key." })),
        );
    }

    // Routed by the ROOT, because the root is the coin whose leaves, aggregate and freeze the SE will
    // read. Resolving by the signer would send the request to whichever enclave holds the CLOSER —
    // the same enclave today (REQ-82: the closer IS the root owner), and the wrong one the moment a
    // delegated closer exists.
    let lockbox_endpoint = match enclave_url_for(statechain_entity, &root_id).await {
        Ok(u) => u,
        Err(e) => return e,
    };

    let client = reqwest::Client::new();
    let resp = match client
        .post(&format!("{}/{}", lockbox_endpoint, "collapse_grant"))
        .json(&payload.0)
        .send()
        .await
    {
        Ok(r) => r,
        Err(err) => {
            return status::Custom(
                Status::InternalServerError,
                Json(json!({ "error": "Internal Server Error", "message": err.to_string() })),
            )
        }
    };

    let code = resp.status().as_u16();
    let text = match resp.text().await {
        Ok(t) => t,
        Err(err) => {
            return status::Custom(
                Status::BadGateway,
                Json(json!({ "message": format!("could not read the enclave's reply: {}", err) })),
            )
        }
    };

    // The SE's refusals are plain strings, its grant is JSON. Both are returned as-is; a refusal
    // wrapped in `{"message": ...}` keeps the client's error path uniform without editing the reason.
    match serde_json::from_str::<Value>(&text) {
        Ok(v) => status::Custom(passthrough_status(code), Json(v)),
        Err(_) => status::Custom(passthrough_status(code), Json(json!({ "message": text }))),
    }
}
