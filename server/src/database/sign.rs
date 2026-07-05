use sqlx::Row;

/// Serialize the whole `sign/first` critical section per `statechain_id`. Two concurrent `sign/first`
/// calls otherwise both pass the NULL-challenge dedup and each make the enclave seal a fresh secnonce,
/// the second overwriting the first — leaving two server pubnonce rows mapped to one sealed secnonce.
/// The enclave-side single-use consume already prevents that from being SIGNED with twice (the real
/// nonce-reuse defence), but serializing here additionally stops the wasted generation and the
/// in-flight-session stranding (a self/replay-inducible DoS). A transaction-scoped advisory lock keyed
/// by `hashtext('signfirst', statechain_id)` auto-releases on commit/rollback, so a crashed handler
/// never leaks the lock. Returns the held transaction; keep it alive until the section is done.
pub async fn acquire_signfirst_lock<'a>(
    pool: &'a sqlx::PgPool,
    statechain_id: &str,
) -> Result<sqlx::Transaction<'a, sqlx::Postgres>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('signfirst'), hashtext($1))")
        .bind(statechain_id)
        .execute(&mut *tx)
        .await?;
    Ok(tx)
}

pub async fn get_server_pubnonce_from_null_challenge(pool: &sqlx::PgPool, statechain_id: &str) -> Option<String> {

    let query = "SELECT server_pubnonce \
        FROM statechain_signature_data \
        WHERE statechain_id = $1 \
        AND challenge is NULL \
        ORDER BY created_at ASC";

    let row = sqlx::query(query)
        .bind(statechain_id)
        .fetch_optional(pool)
        .await
        .unwrap();

    if row.is_none()
    {
        return None;
    }

    let row = row.unwrap();

    let server_pubnonce: String = row.get(0);

    Some(server_pubnonce)
}

pub async fn insert_new_signature_data(pool: &sqlx::PgPool, server_pubnonce: &str, statechain_id: &str)  {

    let mut transaction = pool.begin().await.unwrap();

    // FOR UPDATE is used to lock the row for the duration of the transaction
    // It is not allowed with aggregate functions (MAX in this case), so we need to wrap it in a subquery
    let max_tx_k_query = "\
        SELECT COALESCE(MAX(tx_n), 0) \
        FROM (\
            SELECT * \
            FROM statechain_signature_data \
            WHERE statechain_id = $1 FOR UPDATE) AS result";

    let row = sqlx::query(max_tx_k_query)
        .bind(statechain_id)
        .fetch_one(&mut *transaction)
        .await
        .unwrap();

    let mut new_tx_n = row.get::<i32, _>(0);
    new_tx_n = new_tx_n + 1;

    let query = "\
        INSERT INTO statechain_signature_data \
        (server_pubnonce, statechain_id, tx_n) \
        VALUES ($1, $2, $3)";

    let _ = sqlx::query(query)
        .bind(server_pubnonce)
        .bind(statechain_id)
        .bind(new_tx_n)
        .execute(&mut *transaction)
        .await
        .unwrap();

    transaction.commit().await.unwrap();
}

/// Bind a challenge (message) to a server nonce, ONCE. Returns true iff the challenge was set —
/// either because it was previously NULL, or because it exactly equals the same challenge (an
/// idempotent client retry). Returns FALSE if the row already carries a DIFFERENT challenge: that
/// is a second sign_second reusing one nonce over a different message, which would let the enclave
/// produce two MuSig2 partial signatures with the same secnonce and leak the SE's key share
/// (catastrophic — defeats the 2-of-2 and every off-chain single-use/budget guard). The `WHERE`
/// clause makes the check-and-set atomic, so concurrent finalizes cannot both win.
pub async fn update_signature_data_challenge(pool: &sqlx::PgPool, server_pub_nonce: &str, challenge: &str, statechain_id: &str) -> bool {

    let query = "\
        UPDATE statechain_signature_data \
        SET challenge = $1 \
        WHERE statechain_id = $2 AND server_pubnonce = $3 \
        AND (challenge IS NULL OR challenge = $1)";

    let result = sqlx::query(query)
        .bind(challenge)
        .bind(statechain_id)
        .bind(server_pub_nonce)
        .execute(pool)
        .await
        .unwrap();

    result.rows_affected() == 1
}
