use chrono::{DateTime, Utc};
use secp256k1_zkp::XOnlyPublicKey;
use sqlx::Row;

pub async fn insert_paymenthash(
    pool: &sqlx::PgPool, 
    statechain_id: &str, 
    sender_auth_key: &XOnlyPublicKey,
    batch_id: &str,
    pre_image: &str,
    expires_at: &DateTime<Utc>)  
{
    let query = "DELETE FROM lightning_latch WHERE expires_at < now()";

    let _ = sqlx::query(query)
        .execute(pool)
        .await
        .unwrap();

    let query = "INSERT INTO lightning_latch (statechain_id, sender_auth_xonly_public_key, batch_id, pre_image, expires_at) VALUES ($1, $2, $3, $4, $5)";

    let _ = sqlx::query(query)
        .bind(statechain_id)
        .bind(sender_auth_key.serialize())
        .bind(batch_id)
        .bind(pre_image)
        .bind(expires_at)
        .execute(pool)
        .await
        .unwrap();
}

pub async fn is_lightning_latch(pool: &sqlx::PgPool, statechain_id: &str, sender_auth_key: &XOnlyPublicKey, batch_id: &str) -> bool {
    let query = "SELECT EXISTS \
        (SELECT 1 FROM \
        lightning_latch \
        WHERE statechain_id = $1 \
        AND sender_auth_xonly_public_key = $2 \
        AND batch_id = $3)";

    let row = sqlx::query(query)
        .bind(statechain_id)
        .bind(sender_auth_key.serialize())
        .bind(batch_id)
        .fetch_one(pool)
        .await
        .unwrap();

    let exists: bool = row.get(0);

    exists
}

pub async fn get_preimage(pool: &sqlx::PgPool, statechain_id: &str, sender_auth_key: &XOnlyPublicKey, batch_id: &str) -> Option<String> {

    // Audit [2]/[5]: refuse the preimage once the latch has EXPIRED. The receiver's claim gate
    // (validate_batch) and this preimage gate are enforced against the SAME latch clock, and the
    // latch expires BEFORE the payer's HODL HTLC. So either the receiver claims in time (the SSP can
    // then retrieve the preimage and settle the HTLC) or the latch expires and BOTH the receiver's
    // claim AND the SSP's HTLC-claim fail — the coin never leaves the SSP while the payer is refunded.
    let query = "SELECT pre_image FROM \
        lightning_latch \
        WHERE statechain_id = $1 \
        AND sender_auth_xonly_public_key = $2 \
        AND batch_id = $3
        AND locked = false
        AND expires_at > now()";

    let row = sqlx::query(query)
        .bind(statechain_id)
        .bind(sender_auth_key.serialize())
        .bind(batch_id)
        .fetch_optional(pool)
        .await
        .unwrap();

    if row.is_none()
    {
        return None;
    }

    let row = row.unwrap();

    let pre_image: String = row.get(0);

    Some(pre_image)

}

/// Latch expiry for a batch (audit [2]): looked up by batch_id alone — no sender_auth_key needed, so
/// `validate_batch` can gate an LN-latch batch on the latch's OWN clock instead of the short
/// `batch_timeout`. Returns None when the batch has no lightning_latch row (a plain transfer batch).
pub async fn get_latch_expiry_by_batch_id(pool: &sqlx::PgPool, batch_id: &str) -> Option<DateTime<Utc>> {
    let query = "SELECT expires_at FROM lightning_latch WHERE batch_id = $1 LIMIT 1";
    let row = sqlx::query(query).bind(batch_id).fetch_optional(pool).await.ok()??;
    Some(row.get(0))
}

pub async fn get_preimage_by_batch_id(pool: &sqlx::PgPool, batch_id: &str) -> Option<String> {

    let query = "SELECT pre_image FROM \
        lightning_latch \
        WHERE batch_id = $1";

    let row = sqlx::query(query)
        .bind(batch_id)
        .fetch_optional(pool)
        .await
        .unwrap();

    if row.is_none()
    {
        return None;
    }

    let row = row.unwrap();

    let pre_image: String = row.get(0);

    Some(pre_image)
}

/// Insert a latch whose payment hash is EXTERNAL (e.g. a BOLT11 invoice hash): the SE stores the
/// hash only and the batch is unlocked by presenting the matching preimage.
pub async fn insert_paymenthash_external(
    pool: &sqlx::PgPool,
    statechain_id: &str,
    sender_auth_key: &XOnlyPublicKey,
    batch_id: &str,
    payment_hash: &str,
    expires_at: &DateTime<Utc>,
) {
    let query = "INSERT INTO lightning_latch (statechain_id, sender_auth_xonly_public_key, batch_id, pre_image, payment_hash, expires_at) VALUES ($1, $2, $3, NULL, $4, $5)";

    let _ = sqlx::query(query)
        .bind(statechain_id)
        .bind(sender_auth_key.serialize())
        .bind(batch_id)
        .bind(payment_hash)
        .bind(expires_at)
        .execute(pool)
        .await
        .unwrap();
}

/// Payment hash for a batch: the stored external hash, or None (caller hashes the SE preimage
/// for classic latches).
pub async fn get_external_payment_hash_by_batch_id(pool: &sqlx::PgPool, batch_id: &str) -> Option<String> {
    let query = "SELECT payment_hash FROM lightning_latch WHERE batch_id = $1 AND payment_hash IS NOT NULL";
    let row = sqlx::query(query).bind(batch_id).fetch_optional(pool).await.unwrap();
    row.and_then(|r| r.get::<Option<String>, _>(0))
}

/// All statechain ids latched under a batch (for preimage-triggered unlock).
pub async fn get_statechain_ids_by_batch_id(pool: &sqlx::PgPool, batch_id: &str) -> Vec<String> {
    let query = "SELECT statechain_id FROM lightning_latch WHERE batch_id = $1";
    let rows = sqlx::query(query).bind(batch_id).fetch_all(pool).await.unwrap();
    rows.iter().map(|r| r.get::<String, _>(0)).collect()
}
