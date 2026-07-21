use mercurylib::transfer::receiver::StatechainInfo;
use secp256k1_zkp::{PublicKey, Secp256k1, XOnlyPublicKey, SecretKey};

use sqlx::Row;

pub async fn get_statechain_info(pool: &sqlx::PgPool, statechain_id: &str) -> Vec::<StatechainInfo> {

    let mut result = Vec::<StatechainInfo>::new();

    // Only FINALISED sessions belong in receiver-verification info. A row whose `challenge` is NULL is a
    // dangling session — sign/first ran but sign/second has not — so it carries no finalised signature
    // and no conveyed backup corresponds to it. Excluding it (a) matches count_finalized_signatures'
    // `partial_sig_issued = true` semantics and (b) removes a griefing DoS: a `row.get::<String>()` on a
    // NULL `challenge` panics the whole request worker, so anyone (or any interrupted honest flow) that
    // left a dangling sign/first would make /info/statechain un-fetchable and the coin unverifiable.
    let query = "\
        SELECT statechain_id, server_pubnonce, challenge, tx_n \
        FROM statechain_signature_data \
        WHERE statechain_id = $1 AND challenge IS NOT NULL \
        ORDER BY created_at ASC";

    let rows = match sqlx::query(query).bind(statechain_id).fetch_all(pool).await {
        Ok(rows) => rows,
        // Fail closed: a DB error here must not panic the worker. An empty info set makes the receiver
        // refuse the coin (safe) rather than crash the endpoint for every caller.
        Err(_) => return result,
    };

    for row in rows {
        // Defensive second line against the panic: even with the SQL filter, read `challenge` as nullable
        // and skip any NULL rather than `get`-panicking.
        let challenge: String = match row.try_get(2) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let statechain_id: String = row.get(0);
        let server_pubnonce: String = row.get(1);
        let tx_n: i32 = row.get(3);

        let statechain_transfer = StatechainInfo {
            statechain_id,
            server_pubnonce,
            challenge,
            tx_n: tx_n as u32,
        };

        result.push(statechain_transfer);
    }

    result.sort_by(|a, b| a.tx_n.cmp(&b.tx_n));

    result
}

pub async fn get_enclave_pubkey(pool: &sqlx::PgPool, statechain_id: &str) -> Option<PublicKey> {

    let query = "SELECT server_public_key \
        FROM statechain_data \
        WHERE statechain_id = $1";

    let row = sqlx::query(query)
        .bind(statechain_id)
        .fetch_optional(pool)
        .await
        .unwrap();

    if row.is_none() {
        return None;
    }

    let row = row.unwrap();

    let enclave_public_key_bytes = row.get::<Vec<u8>, _>("server_public_key");
    let enclave_public_key = PublicKey::from_slice(&enclave_public_key_bytes).unwrap();

    Some(enclave_public_key)
}

pub async fn get_x1pub(pool: &sqlx::PgPool, statechain_id: &str) -> Option<PublicKey> {

    let query = "SELECT x1 \
        FROM statechain_transfer \
        WHERE statechain_id = $1";

    let row = sqlx::query(query)
        .bind(statechain_id)
        .fetch_optional(pool)
        .await
        .unwrap();

    if row.is_none() {
        return None;
    }

    let row = row.unwrap();

    let x1_secret_bytes = row.get::<Vec<u8>, _>("x1");
    let secret_x1 = SecretKey::from_slice(&x1_secret_bytes).unwrap();

    Some(secret_x1.public_key(&Secp256k1::new()))
}

pub async fn get_statechain_transfer_messages(pool: &sqlx::PgPool, new_user_auth_key: &PublicKey) -> Vec::<String> {

    let query = "\
        SELECT encrypted_transfer_msg \
        FROM statechain_transfer \
        WHERE new_user_auth_public_key = $1
        AND encrypted_transfer_msg IS NOT NULL \
        ORDER BY updated_at ASC";

    let rows = sqlx::query(query)
        .bind(new_user_auth_key.serialize())
        .fetch_all(pool)
        .await
        .unwrap();

    let mut result = Vec::<String>::new();

    for row in rows {
        let encrypted_transfer_msg: Vec<u8> = row.get(0);
        result.push(hex::encode(encrypted_transfer_msg));
    }

    result
}

pub async fn get_auth_pubkey_and_x1(pool: &sqlx::PgPool, statechain_id: &str) -> Option<(PublicKey, Vec<u8>)> {

    let query = "\
        SELECT new_user_auth_public_key, x1 \
        FROM statechain_transfer \
        WHERE statechain_id = $1";

    // fetch_optional, NOT fetch_one().unwrap(): fetch_one returns Err(RowNotFound) for an unknown
    // statechain_id, and this function runs BEFORE auth in POST /transfer/receiver, so the old
    // .unwrap() let any unauthenticated caller panic the handler (500) with a random statechain_id —
    // a pre-auth DoS and an existence oracle (a coin WITH a pending transfer answered differently
    // from one without). Returning None makes the endpoint's existing `is_none()` guard emit a
    // uniform 404 (adversarial-log review, MALFORM/REORDER).
    let row = match sqlx::query(query)
        .bind(statechain_id)
        .fetch_optional(pool)
        .await
    {
        Ok(Some(r)) => r,
        _ => return None,
    };

    let new_user_auth_public_key_bytes = row.get::<Vec<u8>, _>(0);
    let new_user_auth_public_key = PublicKey::from_slice(&new_user_auth_public_key_bytes).unwrap();

    let x1_bytes = row.get::<Vec<u8>, _>(1);

    Some((new_user_auth_public_key, x1_bytes))
}

pub async fn is_key_already_updated(pool: &sqlx::PgPool, statechain_id: &str) -> bool {

    let query = "\
        SELECT key_updated \
        FROM statechain_transfer \
        WHERE statechain_id = $1";

    // fetch_optional, not fetch_one().unwrap(): a missing row must not panic the handler. Absent
    // row → treat as not-yet-updated (adversarial-log review, panic-hardening).
    let row = match sqlx::query(query)
        .bind(statechain_id)
        .fetch_optional(pool)
        .await
    {
        Ok(Some(r)) => r,
        _ => return false,
    };

    let key_updated: bool = row.get(0);

    key_updated
}

pub async fn get_server_public_key(pool: &sqlx::PgPool, statechain_id: &str) -> Option<PublicKey> {

    let query = "\
        SELECT server_public_key \
        FROM statechain_data \
        WHERE statechain_id = $1";

    // fetch_optional, not fetch_one().unwrap(): a missing statechain_data row must not panic the
    // handler — return None so the endpoint reports it cleanly (adversarial-log review).
    let row = match sqlx::query(query)
        .bind(statechain_id)
        .fetch_optional(pool)
        .await
    {
        Ok(Some(r)) => r,
        _ => return None,
    };

    let server_public_key_bytes: Vec<u8> = row.get(0);

    if server_public_key_bytes.len() == 0 {
        return None;
    }

    let server_public_key = PublicKey::from_slice(&server_public_key_bytes).unwrap();

    Some(server_public_key)
}

pub async fn update_statechain(pool: &sqlx::PgPool, auth_key: &XOnlyPublicKey, server_public_key: &PublicKey, statechain_id: &str)  {

    let mut transaction = pool.begin().await.unwrap();

    let query = "UPDATE statechain_data \
        SET auth_xonly_public_key = $1, server_public_key = $2 \
        WHERE statechain_id = $3";

    let _ = sqlx::query(query)
        .bind(&auth_key.serialize())
        .bind(&server_public_key.serialize())
        .bind(statechain_id)
        .execute(&mut *transaction)
        .await
        .unwrap();

    let query = "UPDATE statechain_transfer \
        SET key_updated = true \
        WHERE statechain_id = $1";

    let _ = sqlx::query(query)
        .bind(statechain_id)
        .execute(&mut *transaction)
        .await
        .unwrap();

    transaction.commit().await.unwrap();
}

/// Clear one lock bit of a transfer (owner → `locked2`, receiver → `locked`) and, once both are
/// clear, release any associated Lightning latch. Returns a typed `Err(message)` instead of
/// panicking when the statechain_id has no transfer row (external review finding 1: the old
/// `fetch_one().unwrap()` 500-crashed the handler on an unknown id) or on any DB error.
pub async fn update_unlock_transfer(pool: &sqlx::PgPool, is_current_owner: bool, statechain_id: &str) -> Result<(), String> {

    let locked_field = if is_current_owner { "locked2" } else { "locked" };

    let query = format!("UPDATE statechain_transfer \
        SET {} = false, updated_at = NOW() \
        WHERE statechain_id = $1", locked_field);

    let updated = sqlx::query(&query)
        .bind(statechain_id)
        .execute(pool)
        .await
        .map_err(|e| format!("could not update transfer lock: {e}"))?;

    // No row for this statechain_id: nothing to unlock — report cleanly rather than reading back a
    // row that does not exist.
    if updated.rows_affected() == 0 {
        return Err("no transfer found for this statechain id".to_string());
    }

    let query = "SELECT locked, locked2, batch_id \
        FROM statechain_transfer \
        WHERE statechain_id = $1";

    let row = match sqlx::query(query)
        .bind(statechain_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| format!("could not read transfer lock state: {e}"))?
    {
        Some(r) => r,
        None => return Err("no transfer found for this statechain id".to_string()),
    };

    let locked: bool = row.get(0);
    let locked2: bool = row.get(1);
    let batch_id: Option<String> = row.get(2);

    // if there is no lightning latch operation, the update below will have no effect

    if let Some(batch_id) = batch_id {
        if !locked && !locked2 {
            let query = "UPDATE lightning_latch \
                SET locked = false, updated_at = NOW() \
                WHERE statechain_id = $1
                AND batch_id = $2";

            sqlx::query(query)
                .bind(statechain_id)
                .bind(batch_id)
                .execute(pool)
                .await
                .map_err(|e| format!("could not release lightning latch: {e}"))?;
        }
    }

    Ok(())
}
