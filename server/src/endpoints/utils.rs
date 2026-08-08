use std::str::FromStr;

use bitcoin::hashes::sha256;
use rocket::{State, response::status, http::Status, serde::json::Json};
use secp256k1_zkp::{schnorr::Signature, Message, Secp256k1, XOnlyPublicKey};
use serde_json::{json, Value};
use sqlx::Row;
use secp256k1_zkp::PublicKey;

use crate::server::StateChainEntity;

pub async fn get_auth_key_by_statechain_id(pool: &sqlx::PgPool, statechain_id: &str) -> Result<XOnlyPublicKey, sqlx::Error> {

    let row = sqlx::query(
        "SELECT auth_xonly_public_key \
        FROM public.statechain_data \
        WHERE statechain_id = $1")
        .bind(&statechain_id)
        .fetch_one(pool)
        .await;

    match row {
        Ok(row) => {
            // A NULL/invalid key column must not panic the worker: treat it as not-found.
            let public_key_bytes = match row.get::<Option<Vec<u8>>, _>("auth_xonly_public_key") {
                Some(b) => b,
                None => return Err(sqlx::Error::RowNotFound),
            };
            match XOnlyPublicKey::from_slice(&public_key_bytes) {
                Ok(pk) => Ok(pk),
                Err(_) => Err(sqlx::Error::RowNotFound),
            }
        },
        Err(err) => {
            Err(err)
        }
    }

}

pub async fn validate_signature_given_public_key(signed_message_hex: &str, statechain_id: &str, auth_key: &str) -> bool {

    // Attacker-controlled inputs must never panic the worker (review L2): a malformed key or
    // signature is simply an invalid (unauthorized) request -> return false.
    let auth_key = match PublicKey::from_str(auth_key) {
        Ok(k) => k.x_only_public_key().0,
        Err(_) => return false,
    };
    let signed_message = match Signature::from_str(signed_message_hex) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let msg = Message::from_hashed_data::<sha256::Hash>(statechain_id.to_string().as_bytes());

    let secp = Secp256k1::new();
    secp.verify_schnorr(&signed_message, &msg, &auth_key).is_ok()
}

pub async fn validate_signature(pool: &sqlx::PgPool, signed_message_hex: &str, statechain_id: &str) -> bool {

    // Do NOT unwrap: get_auth_key can fail with RowNotFound OR PoolTimedOut (under load). Either way
    // the request is unauthorized/unservable -> return false instead of panicking the worker thread.
    let auth_key = match get_auth_key_by_statechain_id(pool, statechain_id).await {
        Ok(k) => k,
        Err(_) => return false,
    };
    let signed_message = match Signature::from_str(signed_message_hex) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let msg = Message::from_hashed_data::<sha256::Hash>(statechain_id.to_string().as_bytes());

    let secp = Secp256k1::new();
    secp.verify_schnorr(&signed_message, &msg, &auth_key).is_ok()
}

/// Single-use, endpoint-bound owner-auth validation (audit [15]). `auth_field` is `"<nonce>:<sig>"`
/// where `<sig>` is a schnorr signature by the coin's auth key over `sha256(nonce|endpoint)`. The
/// signature is verified AND the nonce is atomically consumed (issued for this statechain_id,
/// unexpired, not previously used) — so a captured signature cannot be replayed against an
/// irreversible owner operation, nor redirected to a different endpoint. Fails closed on any
/// malformed input, bad signature, or already-used/expired/unknown nonce.
pub async fn validate_signature_nonce(
    pool: &sqlx::PgPool,
    auth_field: &str,
    statechain_id: &str,
    endpoint: &str,
) -> bool {
    let (nonce, signed_message_hex) = match auth_field.split_once(':') {
        Some((n, s)) if !n.is_empty() && !s.is_empty() => (n, s),
        _ => return false, // legacy/malformed auth is not accepted on a nonce-protected endpoint
    };
    let auth_key = match get_auth_key_by_statechain_id(pool, statechain_id).await {
        Ok(k) => k,
        Err(_) => return false,
    };
    let signed_message = match Signature::from_str(signed_message_hex) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let msg = Message::from_hashed_data::<sha256::Hash>(format!("{nonce}|{endpoint}").as_bytes());
    let secp = Secp256k1::new();
    if !secp.verify_schnorr(&signed_message, &msg, &auth_key).is_ok() {
        return false;
    }
    // Consume the nonce only after the signature is valid (a bad sig cannot burn an owner's nonce).
    // The consume is atomic + single-use; a replay of a valid (nonce,sig) fails here (already used).
    crate::database::auth_nonce::consume_auth_nonce(pool, nonce, statechain_id).await
}

/// Single-use, endpoint-bound validation against a GIVEN public key rather than the coin's
/// registered owner key.
///
/// Needed by transfer cancellation, where the co-signer is the transfer's RECIPIENT — a key that is
/// deliberately not yet registered on the coin (registering it is what claiming does). The caller
/// must pass the key the coordinator RECORDED as `new_user_auth_public_key`; passing a
/// caller-supplied key would make the check vacuous.
///
/// Why not the static `sha256(statechain_id)` signature that `transfer/unlock` accepts: that value
/// never changes, so one recipient signature would be a REUSABLE cancel token for that coin and key.
/// A sender could bank a cooperative cancellation's signature, later open a second transfer to the
/// same recipient key, post the message so the recipient believes they were paid, replay the
/// signature to cancel, and convey to a third party — two victims, one coin. A single-use nonce
/// makes each consent authorize exactly one cancellation.
pub async fn validate_signature_nonce_given_public_key(
    pool: &sqlx::PgPool,
    auth_field: &str,
    statechain_id: &str,
    endpoint: &str,
    auth_key: &PublicKey,
) -> bool {
    validate_signature_nonce_given_xonly_key(
        pool,
        auth_field,
        statechain_id,
        endpoint,
        &auth_key.x_only_public_key().0,
    )
    .await
}

/// The x-only form of [`validate_signature_nonce_given_public_key`], and its implementation.
///
/// Needed by transfer cancellation's SENDER leg, where the key to check against is read from
/// `statechain_transfer.sender_auth_xonly_public_key` (migration 0011) and is therefore already
/// x-only — the same 32-byte form `statechain_data.auth_xonly_public_key` is stored in.
///
/// This is the SAME check `validate_signature_nonce` performs; the only difference is where the key
/// comes from. `validate_signature_nonce` looks it up on the coin, which is wrong for a cancellation
/// once a claim has ROTATED that key to the recipient's — see the ordering block on
/// `endpoints::transfer_sender::transfer_cancel` for why authenticating against the RECORDED opener
/// cannot admit anyone the live-key check would have refused.
pub async fn validate_signature_nonce_given_xonly_key(
    pool: &sqlx::PgPool,
    auth_field: &str,
    statechain_id: &str,
    endpoint: &str,
    auth_key: &XOnlyPublicKey,
) -> bool {
    let (nonce, signed_message_hex) = match auth_field.split_once(':') {
        Some((n, s)) if !n.is_empty() && !s.is_empty() => (n, s),
        _ => return false,
    };
    let signed_message = match Signature::from_str(signed_message_hex) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let msg = Message::from_hashed_data::<sha256::Hash>(format!("{nonce}|{endpoint}").as_bytes());
    let secp = Secp256k1::new();
    if !secp.verify_schnorr(&signed_message, &msg, auth_key).is_ok() {
        return false;
    }
    // Consume only after the signature verifies, so a bogus signature cannot burn a real nonce.
    crate::database::auth_nonce::consume_auth_nonce(pool, nonce, statechain_id).await
}

/// Issue a fresh single-use owner-auth challenge for a coin (audit [15]).
#[get("/auth/challenge/<statechain_id>")]
pub async fn auth_challenge(statechain_entity: &State<StateChainEntity>, statechain_id: &str) -> status::Custom<Json<Value>> {
    match crate::database::auth_nonce::issue_auth_nonce(&statechain_entity.pool, statechain_id).await {
        Some(nonce) => status::Custom(Status::Ok, Json(json!({ "nonce": nonce }))),
        None => status::Custom(Status::ServiceUnavailable, Json(json!({ "message": "could not issue auth challenge" }))),
    }
}

#[get("/info/config")]
pub async fn info_config() -> status::Custom<Json<Value>> {

    let config = crate::server_config::ServerConfig::load();

    let version: &str = env!("CARGO_PKG_VERSION");

    let server_config = mercurylib::utils::ServerConfig {
        initlock: config.lockheight_init,
        interval: config.lh_decrement,
        batchtimeout: config.batch_timeout,
        version: version.to_string(),
    };

    let response_body = json!(server_config);

    return status::Custom(Status::Ok, Json(response_body));
}

#[get("/info/keylist")]
pub async fn info_keylist(statechain_entity: &State<StateChainEntity>) -> status::Custom<Json<Value>> {

    let query = "\
    SELECT server_public_key, statechain_id \
    FROM statechain_data";  

    // Audit [21]: degrade gracefully (empty) instead of panicking a request handler on a pool error.
    let rows = sqlx::query(query)
    .fetch_all(&statechain_entity.pool)
    .await
    .unwrap_or_default();

    let query_sigs = "\
    SELECT tx_n, statechain_id, created_at::TEXT \
    FROM statechain_signature_data";

    let rows_sigs = sqlx::query(query_sigs)
    .fetch_all(&statechain_entity.pool)
    .await
    .unwrap_or_default();

    let mut result = Vec::<mercurylib::utils::PubKeyInfo>::new();

    for row in rows {

        let server_public_key_bytes = row.get::<Vec<u8>, _>(0);
        let server_pubkey = PublicKey::from_slice(&server_public_key_bytes).unwrap();
        let statechain_id: String = row.get(1);

        let mut keyinfo: mercurylib::utils::PubKeyInfo = mercurylib::utils::PubKeyInfo {
            server_pubkey: server_pubkey.to_string(),
            tx_n: 0,
            created_at: "".to_string(),
        };

        for row_sig in &rows_sigs {
            let tx_n_i: i32 = row_sig.get(0);
            let statechain_id_sig: String = row_sig.get(1);
            let updated_at: String = row_sig.get(2);

            if statechain_id == statechain_id_sig {
                keyinfo.tx_n = tx_n_i as u32;
                keyinfo.created_at = updated_at;
            }
        }
        result.push(keyinfo);
    }

    let key_list_response_payload = mercurylib::utils::KeyListResponsePayload {
        list_keyinfo:result
    };

    let response_body = json!(key_list_response_payload);

    return status::Custom(Status::Ok, Json(response_body));

}
