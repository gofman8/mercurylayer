//! Coordinator state for transfer cancellation (migration 0010).
//!
//! The authorization RULE lives in `mercurylib::transfer::cancel` so it is one pure, exhaustively
//! tested function. This module only reads the facts that rule consumes and applies the transition
//! it returns. Nothing here talks to the enclave: a cancellation is coordinator bookkeeping, and the
//! SE never learns that a transfer existed.

use secp256k1_zkp::PublicKey;
use sqlx::Row;

/// How long a `/transfer/receiver` claim latch blocks a cancellation.
///
/// This is the window in which a claim may already have reached the enclave. It has to be long
/// enough to cover a lockbox round trip (the enclave's keyupdate is irreversible — once its share
/// has rotated, refusing the claim afterwards would brick the coin) and short enough that a claim
/// which died mid-flight does not wedge cancellation. `/transfer/receiver` releases the latch
/// explicitly on its error paths, so this bound only matters for a hard crash between the latch and
/// the enclave call.
pub const CLAIM_LATCH_SECS: i64 = 120;

/// The coordinator-observable facts about a coin's transfer row that
/// `mercurylib::transfer::cancel::decide_transfer_cancel` consumes.
pub struct TransferRowForCancel {
    pub recipient_auth_pub_key: PublicKey,
    pub key_updated: bool,
    pub cancelled: bool,
    pub batched: bool,
    pub message_posted: bool,
    pub claim_in_flight: bool,
}

/// Read the transfer row for `statechain_id`, or `None` if there is none.
///
/// `Err` is a real DB fault and MUST be treated as fail-closed by the caller (refuse the cancel) —
/// never as "no row", which would report a live transfer as absent.
pub async fn get_transfer_for_cancel(
    pool: &sqlx::PgPool,
    statechain_id: &str,
) -> Result<Option<TransferRowForCancel>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT new_user_auth_public_key, key_updated, cancelled_at, batch_id, \
                encrypted_transfer_msg IS NOT NULL AS message_posted, \
                (claim_started_at IS NOT NULL AND claim_started_at > NOW() - make_interval(secs => $2::int)) AS claim_in_flight \
         FROM statechain_transfer WHERE statechain_id = $1",
    )
    .bind(statechain_id)
    .bind(CLAIM_LATCH_SECS)
    .fetch_optional(pool)
    .await?;

    let row = match row {
        Some(r) => r,
        None => return Ok(None),
    };

    // A row with an unreadable recipient key is not a row we can authorize a cancellation against:
    // the consent rule is "the RECORDED recipient signs", and without the record there is nothing to
    // check a signature under. Report it as absent rather than panicking the worker.
    let key_bytes: Option<Vec<u8>> = row.try_get::<Option<Vec<u8>>, _>(0).ok().flatten();
    let recipient_auth_pub_key = match key_bytes.as_deref().map(PublicKey::from_slice) {
        Some(Ok(pk)) => pk,
        _ => return Ok(None),
    };

    let key_updated: Option<bool> = row.try_get(1).unwrap_or(Some(false));
    let cancelled_at: Option<chrono::DateTime<chrono::Utc>> = row.try_get(2).unwrap_or(None);
    let batch_id: Option<String> = row.try_get(3).unwrap_or(None);
    let message_posted: Option<bool> = row.try_get(4).unwrap_or(Some(false));
    let claim_in_flight: Option<bool> = row.try_get(5).unwrap_or(Some(false));

    Ok(Some(TransferRowForCancel {
        recipient_auth_pub_key,
        key_updated: key_updated.unwrap_or(false),
        cancelled: cancelled_at.is_some(),
        batched: batch_id.is_some(),
        message_posted: message_posted.unwrap_or(false),
        claim_in_flight: claim_in_flight.unwrap_or(false),
    }))
}

/// Apply an authorized cancellation.
///
/// The UPDATE re-asserts every guard the decision was made under (`key_updated = false`,
/// `cancelled_at IS NULL`, no fresh claim latch) so that a claim which landed between the read and
/// the write WINS: `rows_affected == 0` means the state moved under us and the caller must re-read
/// rather than report success. This is the coordinator half of the claim/cancel mutual exclusion —
/// the other half is `latch_claim` below, and under READ COMMITTED Postgres re-evaluates the WHERE
/// clause of whichever of the two blocks on the other, so exactly one can win.
///
/// `x1` and `encrypted_transfer_msg` are NULLed, not merely flagged: destroying the handover secret
/// means even a reader that forgot the `cancelled_at IS NULL` filter cannot complete the transfer.
///
/// The tombstone row is written in the same transaction, because `insert_new_transfer` DELETEs the
/// live row when the coin is re-addressed — without the tombstone a later transfer would erase the
/// cancelled recipient's only error signal, and would also let the sender re-address the coin back
/// to that stale key.
pub async fn apply_cancel(
    pool: &sqlx::PgPool,
    statechain_id: &str,
    recipient_auth_pub_key: &PublicKey,
    message_was_posted: bool,
    recipient_consented: bool,
) -> Result<bool, sqlx::Error> {
    let mut tx = pool.begin().await?;

    let updated = sqlx::query(
        "UPDATE statechain_transfer \
         SET cancelled_at = NOW(), x1 = NULL, encrypted_transfer_msg = NULL, updated_at = NOW() \
         WHERE statechain_id = $1 \
           AND key_updated = false \
           AND cancelled_at IS NULL \
           AND (claim_started_at IS NULL OR claim_started_at < NOW() - make_interval(secs => $2::int))",
    )
    .bind(statechain_id)
    .bind(CLAIM_LATCH_SECS)
    .execute(&mut *tx)
    .await?;

    if updated.rows_affected() == 0 {
        tx.rollback().await?;
        return Ok(false);
    }

    sqlx::query(
        "INSERT INTO statechain_transfer_cancelled \
            (statechain_id, new_user_auth_public_key, message_was_posted, recipient_consented) \
         VALUES ($1, $2, $3, $4)",
    )
    .bind(statechain_id)
    .bind(recipient_auth_pub_key.serialize().to_vec())
    .bind(message_was_posted)
    .bind(recipient_consented)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(true)
}

/// True if this coin already had a transfer to `new_user_auth_key` cancelled.
///
/// `POST /transfer/sender` refuses such a re-address. Transfer addresses are single-use by
/// construction (the client's `new_transfer_address` mints a fresh coin, hence a fresh auth key, on
/// every call), so this costs an honest sender nothing. What it buys: stale conveyed material is
/// blinded against the x1 of the transfer it was built for, and a re-opened transfer to the same key
/// carries a FRESH x1. A recipient that still held the old message and claimed against the new row
/// would drive the enclave to rotate its share by the wrong tweak — an irreversible brick. Refusing
/// the key reuse removes that path entirely.
///
/// Fails CLOSED (returns `Err`) on a DB fault; the caller must refuse rather than admit the reuse.
pub async fn recipient_key_was_cancelled(
    pool: &sqlx::PgPool,
    statechain_id: &str,
    new_user_auth_key: &PublicKey,
) -> Result<bool, sqlx::Error> {
    let row = sqlx::query(
        "SELECT EXISTS(SELECT 1 FROM statechain_transfer_cancelled \
         WHERE statechain_id = $1 AND new_user_auth_public_key = $2)",
    )
    .bind(statechain_id)
    .bind(new_user_auth_key.serialize().to_vec())
    .fetch_one(pool)
    .await?;
    Ok(row.get::<bool, _>(0))
}

/// Every recipient key that had a cancelled transfer of this coin.
///
/// Used by `/transfer/receiver` to turn a would-be silent 404 into a typed "cancelled" answer, but
/// ONLY for a caller that produces a valid signature under one of these keys — the caller proves
/// possession first, so this is never an existence oracle for a third party.
pub async fn cancelled_recipient_keys(
    pool: &sqlx::PgPool,
    statechain_id: &str,
) -> Result<Vec<PublicKey>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT new_user_auth_public_key FROM statechain_transfer_cancelled \
         WHERE statechain_id = $1 ORDER BY cancelled_at DESC",
    )
    .bind(statechain_id)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .iter()
        .filter_map(|r| r.try_get::<Option<Vec<u8>>, _>(0).ok().flatten())
        .filter_map(|b| PublicKey::from_slice(&b).ok())
        .collect())
}

/// Latch this transfer for a claim that is about to call the enclave.
///
/// Returns `Ok(true)` when the latch was taken. `Ok(false)` means the row is cancelled, gone, or
/// another claim latched it within [`CLAIM_LATCH_SECS`] — in every one of those cases the caller
/// must NOT proceed to the enclave, because the keyupdate it would trigger is irreversible.
///
/// This is the claim half of the claim/cancel mutual exclusion described on [`apply_cancel`].
pub async fn latch_claim(pool: &sqlx::PgPool, statechain_id: &str) -> Result<bool, sqlx::Error> {
    let updated = sqlx::query(
        "UPDATE statechain_transfer SET claim_started_at = NOW() \
         WHERE statechain_id = $1 \
           AND cancelled_at IS NULL \
           AND key_updated = false \
           AND (claim_started_at IS NULL OR claim_started_at < NOW() - make_interval(secs => $2::int))",
    )
    .bind(statechain_id)
    .bind(CLAIM_LATCH_SECS)
    .execute(pool)
    .await?;
    Ok(updated.rows_affected() == 1)
}

/// Release a claim latch that will not reach the enclave after all (validation failed, the lockbox
/// call errored, its answer did not parse).
///
/// Best-effort: a failure here only means the next retry waits out [`CLAIM_LATCH_SECS`], it can
/// never admit an unauthorized transition. Never called after a successful keyupdate — at that
/// point `key_updated = true` is the terminal state and the latch is irrelevant.
pub async fn release_claim_latch(pool: &sqlx::PgPool, statechain_id: &str) {
    let _ = sqlx::query(
        "UPDATE statechain_transfer SET claim_started_at = NULL \
         WHERE statechain_id = $1 AND key_updated = false",
    )
    .bind(statechain_id)
    .execute(pool)
    .await;
}
