use secp256k1_zkp::{PublicKey, XOnlyPublicKey};
use sqlx::Row;

pub async fn get_token_status(pool: &sqlx::PgPool, token_id: &str) -> Option<bool> {

    let row = sqlx::query(
        "SELECT confirmed, spent \
        FROM public.tokens \
        WHERE token_id = $1")
        .bind(&token_id)
        .fetch_one(pool)
        .await;

    if row.is_err() {
        match row.err().unwrap() {
            sqlx::Error::RowNotFound => return None,
            _ => return None, // this case should be treated as unexpected error
        }
    }

    let row = row.unwrap();

    let confirmed: bool = row.get(0);
    let spent: bool = row.get(1);
    if confirmed && !spent {
        return Some(true);
    } else {
        return Some(false);
    }

}

pub struct TokenInfo {
    pub confirmed: bool,
    pub spent: bool,
}

pub async fn get_token_info(pool: &sqlx::PgPool, token_id: &str) -> Option<TokenInfo> {

    let row = sqlx::query(
        "SELECT confirmed, spent \
        FROM public.tokens \
        WHERE token_id = $1")
        .bind(&token_id)
        .fetch_optional(pool)
        .await;

    let row = row.unwrap();

    if row.is_none() {
        return None;
    }

    let row = row.unwrap();

    let confirmed: bool = row.get(0);
    let spent: bool = row.get(1);

    Some(TokenInfo {
        confirmed,
        spent,
    })
}

pub async fn set_token_spent(pool: &sqlx::PgPool, token_id: &str)  {

    let mut transaction = pool.begin().await.unwrap();

    let query = "UPDATE tokens \
        SET spent = true \
        WHERE token_id = $1";

    let _ = sqlx::query(query)
        .bind(token_id)
        .execute(&mut *transaction)
        .await
        .unwrap();

    transaction.commit().await.unwrap();
}

pub async fn check_existing_key(pool: &sqlx::PgPool, auth_key: &XOnlyPublicKey) -> bool {
    let row = sqlx::query(
        "SELECT 1 \
        FROM statechain_data \
        WHERE auth_xonly_public_key = $1")
        .bind(&auth_key.serialize())
        .fetch_one(pool)
        .await;

    match row {
        Ok(_) => true,
        Err(sqlx::Error::RowNotFound) => false,
        Err(_) => false,
    }
}

pub async fn insert_new_deposit(pool: &sqlx::PgPool, token_id: &str, auth_key: &XOnlyPublicKey, server_public_key: &PublicKey, statechain_id: &String, enclave_index: i32, single_use: bool, epoch_deadline: Option<i64>)  {

    let query = "INSERT INTO statechain_data (token_id, auth_xonly_public_key, server_public_key, statechain_id, enclave_index, single_use, epoch_deadline) VALUES ($1, $2, $3, $4, $5, $6, $7)";

    let _ = sqlx::query(query)
        .bind(token_id)
        .bind(&auth_key.serialize())
        .bind(&server_public_key.serialize())
        .bind(statechain_id)
        .bind(enclave_index)
        .bind(single_use)
        .bind(epoch_deadline)
        .execute(pool)
        .await
        .unwrap();
}

// Audit [1]: the fork-prevention guards below MUST fail CLOSED. They return `Result` so a DB error
// (e.g. pool exhaustion under load) propagates to `sign/first`, which then refuses to co-sign — a
// blind SE that cannot read a coin's single-use/budget/epoch state must never substitute a
// permissive default (which would let it co-sign a second conflicting spend => off-chain
// double-spend / INV-19 fork). A row being ABSENT is still the benign default (not single-use / no
// budget / no epoch); only an actual query error is fatal.

/// Epoch deadline (unix seconds) for a coin, or None if it has no epoch (Stage 4). The SE refuses to
/// co-sign a new spend once its own clock passes this deadline; unilateral exit needs no SE signature.
pub async fn get_epoch_deadline(pool: &sqlx::PgPool, statechain_id: &str) -> Result<Option<i64>, sqlx::Error> {
    let row: Option<(Option<i64>,)> =
        sqlx::query_as("SELECT epoch_deadline FROM statechain_data WHERE statechain_id = $1")
            .bind(statechain_id)
            .fetch_optional(pool)
            .await?;
    Ok(row.and_then(|r| r.0))
}

/// True if the statechain coin was created as single-use (an off-chain RGB split/combine tree node).
pub async fn is_single_use(pool: &sqlx::PgPool, statechain_id: &str) -> Result<bool, sqlx::Error> {
    let row: Option<(bool,)> =
        sqlx::query_as("SELECT single_use FROM statechain_data WHERE statechain_id = $1")
            .bind(statechain_id)
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|r| r.0).unwrap_or(false))
}

/// Count of FINALIZED signatures (challenge set, i.e. a sign_second completed) for a coin. A
/// single-use coin with >= 1 finalized signature has already been terminally spent, so the SE must
/// refuse any further spend.
pub async fn count_finalized_signatures(pool: &sqlx::PgPool, statechain_id: &str) -> Result<i64, sqlx::Error> {
    let row: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM statechain_signature_data WHERE statechain_id = $1 AND challenge IS NOT NULL")
        .bind(statechain_id)
        .fetch_one(pool)
        .await?;
    Ok(row.0)
}

/// Count of outstanding (unspent) deposit tokens (audit [26]): used to cap the unauthenticated
/// non-mainnet get_token faucet so it cannot amplify DB writes unboundedly.
pub async fn count_unspent_tokens(pool: &sqlx::PgPool) -> i64 {
    sqlx::query_as::<_, (i64,)>("SELECT COUNT(*) FROM tokens WHERE spent = false")
        .fetch_one(pool)
        .await
        .map(|r| r.0)
        // Fail CLOSED (return the cap) so a read error throttles rather than opens the faucet.
        .unwrap_or(i64::MAX)
}

pub async fn insert_new_token(pool: &sqlx::PgPool, token_id: &str)  {

    let query = "INSERT INTO tokens (token_id, confirmed, spent) VALUES ($1, $2, $3)";

    let _ = sqlx::query(query)
        .bind(token_id)
        .bind(true)
        .bind(false)
        .execute(pool)
        .await
        .unwrap();
}

/// Set an absolute co-signature budget: current finalized count + `remaining`. The SE refuses
/// sign_first once the count reaches the budget.
pub async fn set_sig_budget(pool: &sqlx::PgPool, statechain_id: &str, remaining: i32) -> Result<i64, sqlx::Error> {
    let count = count_finalized_signatures(pool, statechain_id).await?;
    let mut budget = count + remaining as i64;
    // MONOTONIC: a budget may only TIGHTEN, never loosen. Without this, an owner who already
    // terminally spent a node (finalized == budget) could call set_spend_budget again — the
    // relative `count + remaining` would compute a HIGHER budget and re-open the node for a second,
    // conflicting spend (off-chain double-spend / INV-19 fork). Clamp to any existing budget. Fail
    // closed on a read error (audit [1]) — never write a loosened budget from a partial read.
    if let Some(existing) = get_sig_budget(pool, statechain_id).await? {
        budget = budget.min(existing as i64);
    }
    let query = "UPDATE statechain_data SET sig_budget = $1 WHERE statechain_id = $2";
    sqlx::query(query)
        .bind(budget as i32)
        .bind(statechain_id)
        .execute(pool)
        .await?;
    Ok(budget)
}

pub async fn get_sig_budget(pool: &sqlx::PgPool, statechain_id: &str) -> Result<Option<i32>, sqlx::Error> {
    let query = "SELECT sig_budget FROM statechain_data WHERE statechain_id = $1";
    let row = sqlx::query(query)
        .bind(statechain_id)
        .fetch_optional(pool)
        .await?;
    Ok(row.and_then(|r| sqlx::Row::get::<Option<i32>, _>(&r, 0)))
}
