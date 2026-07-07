//! Single-use, endpoint-bound owner-auth challenges (audit [15]). An owner requests a fresh nonce
//! and signs `sha256(nonce|endpoint)`; the nonce is atomically consumed on first valid use so a
//! captured signature cannot be replayed against an irreversible owner operation.

use chrono::{Duration, Utc};

/// Issue a fresh auth nonce for `statechain_id` (best-effort GC of expired rows first). Returns the
/// nonce hex, or None on a DB error.
pub async fn issue_auth_nonce(pool: &sqlx::PgPool, statechain_id: &str) -> Option<String> {
    // opportunistic cleanup of expired/consumed rows
    let _ = sqlx::query("DELETE FROM server_auth_nonce WHERE expires_at < now() OR consumed = true")
        .execute(pool)
        .await;

    let nonce = uuid::Uuid::new_v4().simple().to_string() + &uuid::Uuid::new_v4().simple().to_string();
    let expires_at = Utc::now() + Duration::seconds(300); // 5-minute window to sign+submit
    match sqlx::query(
        "INSERT INTO server_auth_nonce (nonce, statechain_id, expires_at, consumed) VALUES ($1, $2, $3, false)",
    )
    .bind(&nonce)
    .bind(statechain_id)
    .bind(expires_at)
    .execute(pool)
    .await
    {
        Ok(_) => Some(nonce),
        Err(_) => None,
    }
}

/// Atomically consume `nonce` for `statechain_id`: succeeds ONCE, iff the nonce exists, was issued
/// for this statechain_id, is unexpired, and is not already consumed. Concurrent replays race on the
/// `consumed = false` guard so only one wins (rows_affected == 1). Returns true on a successful
/// consume (the caller may proceed), false otherwise (unknown / expired / already used / DB error).
pub async fn consume_auth_nonce(pool: &sqlx::PgPool, nonce: &str, statechain_id: &str) -> bool {
    match sqlx::query(
        "UPDATE server_auth_nonce SET consumed = true \
         WHERE nonce = $1 AND statechain_id = $2 AND consumed = false AND expires_at > now()",
    )
    .bind(nonce)
    .bind(statechain_id)
    .execute(pool)
    .await
    {
        Ok(res) => res.rows_affected() == 1,
        // Fail CLOSED: a DB error must not admit an unauthenticated op.
        Err(_) => false,
    }
}
