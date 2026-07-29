use mercurylib::transaction::SignFirstRequestPayload;
use rocket::{http::Status, response::status, serde::json::Json, State};
use secp256k1_zkp::musig::MusigSession;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};


use crate::server::StateChainEntity;

#[post("/sign/first", format = "json", data = "<sign_first_request_payload>")]
pub async fn sign_first(statechain_entity: &State<StateChainEntity>, sign_first_request_payload: Json<SignFirstRequestPayload>) -> status::Custom<Json<Value>>  {

    let config = crate::server_config::ServerConfig::load();
    
    let statechain_id = sign_first_request_payload.0.statechain_id.clone();

    let statechain_entity = statechain_entity.inner();

    // Serialize sign/first for this coin (defence-in-depth for the nonce-reuse guard + stops the
    // in-flight-session stranding DoS). Held for the whole handler; a transaction-scoped advisory
    // lock auto-releases when `_signfirst_lock` drops on return.
    //
    // [S9] FAIL CLOSED. This was `.ok()` — a DB hiccup dropped the lock and let sign/first proceed
    // UNSERIALIZED, which permits two concurrent sign/first for one coin to each seal a fresh secnonce
    // (the second overwriting the first) and strand an in-flight session (a self/replay-inducible DoS).
    // The old comment rationalized this as "the enclave consume is authoritative" — but that consume was
    // itself MISSING on the SGX lane until 9cfe48f, and it does not address the stranding race at all. A
    // safety lock that silently no-ops on failure is not a safety lock: refuse (503, retryable) instead.
    let _signfirst_lock = match crate::database::sign::acquire_signfirst_lock(&statechain_entity.pool, &statechain_id).await {
        Ok(tx) => tx,
        Err(_) => {
            return status::Custom(
                Status::ServiceUnavailable,
                Json(json!({ "message": "sign/first serialization lock unavailable; retry (fail-closed)" })),
            );
        }
    };

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
    let path = "get_public_nonce";

    let client: reqwest::Client = reqwest::Client::new();
    let request = client.post(&format!("{}/{}", lockbox_endpoint, path));

    let signed_statechain_id = sign_first_request_payload.0.signed_statechain_id.clone();

    if !crate::endpoints::utils::validate_signature(&statechain_entity.pool, &signed_statechain_id, &statechain_id).await {

        let response_body = json!({
            "message": "Signature does not match authentication key."
        });

        return status::Custom(Status::Unauthorized, Json(response_body));
    }

    // SE single-use enforcement (off-chain RGB tree double-spend guard): a single-use coin may be
    // terminally spent only ONCE. The client skips the deposit unilateral-exit backup for single-use
    // coins, so the terminal spend is the coin's FIRST finalized signature and the first double-spend
    // attempt is the second — refuse from >= 1. Uniform across all single-use coins (fund-deposited
    // roots and split/combine sub-coins, broadcast or un-broadcast). Normal coins (single_use=false)
    // keep the existing re-sign behaviour.
    // Audit [1]: read the fork-prevention state, failing CLOSED (503) on ANY DB error. If the blind
    // SE cannot read a coin's single-use/budget/epoch state it MUST refuse to co-sign — substituting
    // a permissive default under pool exhaustion would let it co-sign a second conflicting spend of a
    // terminal node (off-chain double-spend / INV-19 fork). Read the finalized count ONCE and reuse.
    macro_rules! fail_closed {
        () => {{
            return status::Custom(
                Status::ServiceUnavailable,
                Json(json!({ "message": "SE co-sign state temporarily unavailable; refusing to co-sign (fail-closed)" })),
            );
        }};
    }
    let single_use = match crate::database::deposit::is_single_use(&statechain_entity.pool, &statechain_id).await {
        Ok(v) => v,
        Err(_) => fail_closed!(),
    };
    let finalized = match crate::database::deposit::count_finalized_signatures(&statechain_entity.pool, &statechain_id).await {
        Ok(v) => v,
        Err(_) => fail_closed!(),
    };
    if single_use && finalized >= 1 {
        let response_body = json!({
            "message": "single-use coin already spent (SE refuses a second spend)"
        });
        return status::Custom(Status::Gone, Json(response_body));
    }

    // Terminal-spend budget (off-chain tree nodes): once the owner sets a budget, no
    // co-signature beyond it — a split/combine node is one-shot regardless of who asks.
    let budget = match crate::database::deposit::get_sig_budget(&statechain_entity.pool, &statechain_id).await {
        Ok(v) => v,
        Err(_) => fail_closed!(),
    };
    if let Some(budget) = budget {
        if finalized >= budget as i64 {
            let response_body = json!({
                "message": "spend budget exhausted (terminal node: SE refuses further co-signatures)"
            });
            return status::Custom(Status::Gone, Json(response_body));
        }
    }

    // SE epoch-deadline enforcement (Stage 4 — off-chain RGB tree exit window): once the SE's own
    // clock passes a coin's epoch deadline, it refuses to co-sign ANY new spend. The owner must have
    // transacted or exited before then; unilateral exit needs no SE co-signature (the owner just
    // broadcasts an already-co-signed branch), so funds are never stuck. Coins without an epoch
    // (epoch_deadline = NULL) are co-signable indefinitely, exactly as before.
    let epoch_deadline = match crate::database::deposit::get_epoch_deadline(&statechain_entity.pool, &statechain_id).await {
        Ok(v) => v,
        Err(_) => fail_closed!(),
    };
    if let Some(deadline) = epoch_deadline {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        if now >= deadline {
            let response_body = json!({
                "message": format!("epoch deadline passed (now={} >= deadline={}); coin must be exited unilaterally, SE refuses new co-signatures", now, deadline)
            });
            return status::Custom(Status::Gone, Json(response_body));
        }
    }

    // [PENDING-TRANSFER LOCK — CHILDREN.md] While a transfer of this coin is OPEN (conveyed
    // but not yet completed by the receiver), the SE refuses ANY co-sign. After the transfer_sender
    // re-order every legitimate sender pre-sign (backups + Model-A S') completes BEFORE `get_new_x1`
    // opens the transfer, so no honest co-sign happens during the open window — refusing here closes the
    // TOCTOU where a still-owner sender co-signs a lower-CSV rival that out-races the receiver's conveyed
    // state, and the dangling sign/first + late sign/second variant. Fail CLOSED on a DB error like the
    // other gates. Expiry is inside the query (never-claimed transfers release after an hour; no victim).
    match crate::database::transfer_sender::has_open_transfer(&statechain_entity.pool, &statechain_id, crate::server_config::ServerConfig::load().batch_timeout as i64).await {
        Ok(true) => {
            return status::Custom(
                Status::Conflict,
                Json(json!({ "message": "coin has an open transfer (SE refuses co-signatures until it completes or expires)" })),
            );
        }
        Ok(false) => {}
        Err(_) => fail_closed!(),
    }

    // This situation should not happen, as this state is only possible if the client has called signFirst, but not signSecond
    // In this case, the server should have already stored server_pubnonce in the database and the challenge is still null because the client did not call signSecond
    let server_pubnonce_hex = crate::database::sign::get_server_pubnonce_from_null_challenge(&statechain_entity.pool, &statechain_id).await;

    if server_pubnonce_hex.is_some() {

        let response = mercurylib::transaction::SignFirstResponsePayload {
            server_pubnonce: server_pubnonce_hex.unwrap(),
        };

        let response_body = json!(response);
    
        return status::Custom(Status::Ok, Json(response_body));
    }

    let value = match request.json(&sign_first_request_payload.0).send().await {
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

    // Do NOT panic on an unparseable enclave body: `.expect(...)` here aborts the request handler and
    // returns a 500 with no typed error (adversarial-log review, MALFORM). Return a clean 502 so a
    // malformed/errored lockbox reply cannot crash the endpoint.
    let response: mercurylib::transaction::SignFirstResponsePayload = match serde_json::from_str(value.as_str()) {
        Ok(r) => r,
        Err(_) => {
            let response_body = json!({
                "message": format!("enclave returned an unparseable sign/first response: {}", value)
            });
            return status::Custom(Status::BadGateway, Json(response_body));
        }
    };

    let mut server_pubnonce_hex = response.server_pubnonce.clone();

    if server_pubnonce_hex.starts_with("0x") {
        server_pubnonce_hex = server_pubnonce_hex[2..].to_string();
    }

    crate::database::sign::insert_new_signature_data(&statechain_entity.pool, &server_pubnonce_hex, &statechain_id,).await;

    let response_body = json!(response);

    return status::Custom(Status::Ok, Json(response_body));
}

#[post("/sign/second", format = "json", data = "<partial_signature_request_payload>")]
pub async fn sign_second (statechain_entity: &State<StateChainEntity>, partial_signature_request_payload: Json<mercurylib::transaction::PartialSignatureRequestPayload>) -> status::Custom<Json<Value>>  {
    
    let statechain_id = partial_signature_request_payload.0.statechain_id.clone();

    let statechain_entity = statechain_entity.inner();

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
    let path = "get_partial_signature";

    let client: reqwest::Client = reqwest::Client::new();
    let request = client.post(&format!("{}/{}", lockbox_endpoint, path));

    let signed_statechain_id = partial_signature_request_payload.0.signed_statechain_id.clone();

    if !crate::endpoints::utils::validate_signature(&statechain_entity.pool, &signed_statechain_id, &statechain_id).await {

        let response_body = json!({
            "message": "Signature does not match authentication key."
        });
    
        return status::Custom(Status::Unauthorized, Json(response_body));
    }

    // [S1] RE-CHECK THE FORK GATES HERE, NOT ONLY IN sign/first.
    //
    // Every terminality gate (single-use, spend-budget, epoch-deadline) used to live exclusively in
    // sign/first. But a signing session is TWO calls and the first one's state is DURABLE: a
    // null-challenge row is deliberately re-served (`get_server_pubnonce_from_null_challenge`) and the
    // sealed secnonce persists in the lockbox. So an owner could open sign/first BEFORE the node became
    // terminal, let the node be terminalized (e.g. `set_spend_budget` at split time, or the node's
    // first single-use spend), and then complete the DANGLING session via sign/second — which checked
    // none of the gates — obtaining a SECOND conflicting co-signature of a terminal node. That defeats
    // exactly the guarantee the split/RGB tree rests on (INV-19 fork prevention) and is reachable on the
    // V1 single_use lane, not just V2. The gates must hold at the moment the signature is ISSUED.
    //
    // Fail CLOSED on any DB error, identically to sign/first: if the SE cannot read a coin's
    // fork-prevention state it must refuse to co-sign rather than assume a permissive default.
    macro_rules! fail_closed_second {
        () => {{
            return status::Custom(
                Status::ServiceUnavailable,
                Json(json!({ "message": "SE co-sign state temporarily unavailable; refusing to co-sign (fail-closed)" })),
            );
        }};
    }
    let single_use = match crate::database::deposit::is_single_use(&statechain_entity.pool, &statechain_id).await {
        Ok(v) => v,
        Err(_) => fail_closed_second!(),
    };
    let finalized = match crate::database::deposit::count_finalized_signatures(&statechain_entity.pool, &statechain_id).await {
        Ok(v) => v,
        Err(_) => fail_closed_second!(),
    };
    if single_use && finalized >= 1 {
        return status::Custom(
            Status::Gone,
            Json(json!({ "message": "single-use coin already spent (SE refuses a second spend)" })),
        );
    }
    match crate::database::deposit::get_sig_budget(&statechain_entity.pool, &statechain_id).await {
        Ok(Some(budget)) if finalized >= budget as i64 => {
            return status::Custom(
                Status::Gone,
                Json(json!({ "message": "spend budget exhausted (terminal node: SE refuses further co-signatures)" })),
            );
        }
        Ok(_) => {}
        Err(_) => fail_closed_second!(),
    }
    match crate::database::deposit::get_epoch_deadline(&statechain_entity.pool, &statechain_id).await {
        Ok(Some(deadline)) => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            if now >= deadline {
                return status::Custom(
                    Status::Gone,
                    Json(json!({ "message": format!("epoch deadline passed (now={} >= deadline={}); coin must be exited unilaterally, SE refuses new co-signatures", now, deadline) })),
                );
            }
        }
        Ok(None) => {}
        Err(_) => fail_closed_second!(),
    }

    // [PENDING-TRANSFER LOCK — CHILDREN.md] Refuse to FINALIZE a co-sign while a transfer of
    // this coin is open. Enforced at sign_second (not only sign_first) so a sender that opened sign_first
    // BEFORE the transfer cannot complete the dangling session AFTER it to slot a lower-CSV rival — the
    // gates must hold at the moment the signature is ISSUED. Fail CLOSED on a DB error.
    match crate::database::transfer_sender::has_open_transfer(&statechain_entity.pool, &statechain_id, crate::server_config::ServerConfig::load().batch_timeout as i64).await {
        Ok(true) => {
            return status::Custom(
                Status::Conflict,
                Json(json!({ "message": "coin has an open transfer (SE refuses to finalize a co-signature until it completes or expires)" })),
            );
        }
        Ok(false) => {}
        Err(_) => fail_closed_second!(),
    }

    let partial_signature_request_payload = partial_signature_request_payload.0.clone();
    let session = partial_signature_request_payload.session.clone();
    let server_pub_nonce = partial_signature_request_payload.server_pub_nonce.clone();

    // Audit [18]: return 400 on a malformed session instead of panicking (a bad hex string or wrong
    // length would otherwise abort the request handler). MusigSession::from_slice takes a fixed
    // [u8;133] and is infallible once the length is right.
    let session_bytes: [u8; 133] = match hex::decode(&session).ok().and_then(|b| b.try_into().ok()) {
        Some(b) => b,
        None => {
            return status::Custom(
                Status::BadRequest,
                Json(json!({ "message": "malformed MuSig session (expected 133-byte hex)" })),
            );
        }
    };
    let session = MusigSession::from_slice(session_bytes);
    let challenge = session.get_challenge_from_session();
    let challenge_str = hex::encode(challenge);

    // Bind this challenge to the server nonce atomically and ONCE. If the nonce was already
    // finalized with a DIFFERENT challenge, refuse before calling the enclave: signing twice with
    // one secnonce over two messages leaks the SE key share (MuSig2 nonce reuse) and would also let
    // an owner obtain two co-signed conflicting spends of a single-use/budgeted off-chain node while
    // the finalized-signature counter shows only one (INV-19 fork-prevention bypass).
    let challenge_bound = crate::database::sign::update_signature_data_challenge(&statechain_entity.pool, &server_pub_nonce, &challenge_str, &statechain_id).await;
    if !challenge_bound {
        let response_body = json!({
            "message": "server nonce already finalized with a different challenge — refusing to co-sign again (nonce reuse would leak the SE key share). Call sign/first for a fresh nonce."
        });
        return status::Custom(Status::Conflict, Json(response_body));
    }

    let value = match request.json(&partial_signature_request_payload).send().await {
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

    #[derive(Serialize, Deserialize, Debug, Clone)]
    pub struct PartialSignatureResponsePayload<'r> {
        partial_sig: &'r str,
    }

    // Do NOT panic on an unparseable enclave body. This path is reachable by an ordinary client
    // RETRY of sign/second: the challenge re-binds idempotently (update_signature_data_challenge)
    // and the enclave is called again, but it has already consumed that secnonce and returns an
    // error body rather than a partial signature — `.expect(...)` would 500-crash the handler
    // (adversarial-log review, REPLAY/MALFORM). Return a clean 502 instead.
    let response: PartialSignatureResponsePayload = match serde_json::from_str(value.as_str()) {
        Ok(r) => r,
        Err(_) => {
            // Lockbox failure AFTER the challenge write: with the finality split (finding 2) the
            // spend budget is NOT consumed, because `challenge IS NOT NULL` no longer counts as
            // finalized — only `partial_sig_issued`, which we have not set. The owner can retry.
            let response_body = json!({
                "message": format!("enclave returned an unparseable sign/second response: {}", value)
            });
            return status::Custom(Status::BadGateway, Json(response_body));
        }
    };

    // A valid partial signature was issued: NOW record it as finalized (external review finding 2).
    // Fail CLOSED on a DB error — the count increment happens after the enclave co-signs, so a
    // silently-dropped marker would UNDER-count finalized signatures and could re-open a terminal
    // single-use/budgeted node to a second conflicting co-signature (INV-19 fork). Returning an
    // error makes the client retry the whole round rather than believe an unrecorded sign succeeded.
    match crate::database::sign::set_partial_sig_issued(&statechain_entity.pool, &server_pub_nonce, &statechain_id).await {
        Ok(true) => {}
        Ok(false) => {
            return status::Custom(
                Status::InternalServerError,
                Json(json!({ "message": "could not record the issued signature (no matching signing row) — retry" })),
            );
        }
        Err(_) => {
            return status::Custom(
                Status::InternalServerError,
                Json(json!({ "message": "could not record the issued signature — retry" })),
            );
        }
    }

    let response_body = json!(response);

    return status::Custom(Status::Ok, Json(response_body));
}
   