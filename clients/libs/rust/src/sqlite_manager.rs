use mercurylib::wallet::{Wallet, BackupTx};
use serde_json::json;
use sqlx::{Pool, Sqlite, Row};
use anyhow::{anyhow, Result};

pub async fn insert_wallet(pool: &Pool<Sqlite>, wallet: &Wallet) -> Result<()> {

    let wallet_json = json!(wallet).to_string();

    let query = "INSERT INTO wallet (wallet_name, wallet_json) VALUES ($1, $2)";

    let _ = sqlx::query(query)
            .bind(wallet.name.clone())
            .bind(wallet_json)
            .execute(pool)
            .await?;
    
    Ok(())
}

pub async fn get_wallet(pool: &Pool<Sqlite>, wallet_name: &str) -> Result<Wallet> {
    
    let query = "SELECT wallet_json FROM wallet WHERE wallet_name = $1";

    let row = sqlx::query(query)
        .bind(wallet_name)
        .fetch_one(pool)
        .await?;

    if row.is_empty() {
        return Err(anyhow!("Wallet not found"));
    }

    let wallet_json: String = row.get(0);

    let wallet: Wallet = serde_json::from_str(&wallet_json)?;

    Ok(wallet)
}

pub async fn update_wallet(pool: &Pool<Sqlite>, wallet: &Wallet) -> Result<()> {
    
    let wallet_json = json!(wallet).to_string();

    let query = "UPDATE wallet SET wallet_json = $1 WHERE wallet_name = $2";

    let _ = sqlx::query(query)
            .bind(wallet_json)
            .bind(wallet.name.clone())
            .execute(pool)
            .await?;
    
    Ok(())
}

pub async fn insert_backup_txs(pool: &Pool<Sqlite>, wallet_name: &str, statechain_id: &str, backup_txs: &Vec<BackupTx>) -> Result<()> {

    let backup_txs_json = json!(backup_txs).to_string();

    let query = "INSERT INTO backup_txs (wallet_name, statechain_id, txs) VALUES ($1, $2, $3)";

    let _ = sqlx::query(query)
            .bind(wallet_name)
            .bind(statechain_id)
            .bind(backup_txs_json)
            .execute(pool)
            .await?;
    
    Ok(())
}

pub async fn update_backup_txs(pool: &Pool<Sqlite>, wallet_name: &str, statechain_id: &str, backup_txs: &Vec<BackupTx>) -> Result<()> {

    let backup_txs_json = json!(backup_txs).to_string();

    let query = "UPDATE backup_txs SET txs = $1 WHERE statechain_id = $2 AND wallet_name = $3";

    let _ = sqlx::query(query)
            .bind(backup_txs_json)
            .bind(statechain_id)
            .bind(wallet_name)
            .execute(pool)
            .await?;
    
    Ok(())
}

pub async fn get_backup_txs(pool: &Pool<Sqlite>, wallet_name: &str, statechain_id: &str,) -> Result<Vec<BackupTx>> {

    let query = "SELECT txs FROM backup_txs WHERE statechain_id = $1 AND wallet_name = $2";

    // `fetch_one`, so an ABSENT row is an `Err` — and callers DISTINGUISH a genuine absence from a
    // failed read by DOWNCASTING to `sqlx::Error::RowNotFound` (`tokens::read_backup_rows`). Wrapping
    // this error in an `anyhow!` to add context therefore breaks absence detection wallet-wide: every
    // missing row starts reading as "the database could not be read", which fails closed so hard that
    // `claim()` stops laddering anything. The bare sqlx error is load-bearing. Do not decorate it.
    let row = sqlx::query(query)
        .bind(statechain_id)
        .bind(wallet_name)
        .fetch_one(pool)
        .await?;

    if row.is_empty() {
        return Err(anyhow!("Statechain id not found"));
    }

    let backup_txs_json: String = row.get(0);

    let backup_txs: Vec<BackupTx> = serde_json::from_str(&backup_txs_json)?;

    Ok(backup_txs)
}

/// **`Ok(None)` = the row genuinely does not exist; `Err` = the database could not be read.**
///
/// [`get_backup_txs`] reports BOTH through `Err`, and the bare `sqlx` error is deliberately left
/// undecorated so this distinction survives (see the note there). But "undecorated" also means a
/// caller that bare-`?`s it hands the user a **database implementation detail** — literally
/// *"no rows returned by a query that expected to return at least one row"* — in place of a refusal.
///
/// That is not cosmetic. A coin with no flat backup rows is an ORDINARY, EXPECTED shape on this
/// protocol: a split child's funding output was never deposited (`CHILD_V2_BASELINE == 0`), and a
/// spine tip's is `SP.out[K]`, un-broadcast. Routing one of those to the flat sender is a real
/// condition that deserves a named refusal — and until it had one, `chaos22`'s oracle correctly
/// classified nine of them as UNCLASSIFIED breaches, because an error it cannot recognise is exactly
/// what a routing regression looks like.
///
/// Callers that want "absent means empty" must say so by matching `Ok(None)`; they may never reach
/// that conclusion from an `Err`, which carries no information about whether the row exists.
pub async fn try_get_backup_txs(
    pool: &Pool<Sqlite>,
    wallet_name: &str,
    statechain_id: &str,
) -> Result<Option<Vec<BackupTx>>> {
    match get_backup_txs(pool, wallet_name, statechain_id).await {
        Ok(rows) => Ok(Some(rows)),
        Err(e) => {
            let missing = matches!(e.downcast_ref::<sqlx::Error>(), Some(sqlx::Error::RowNotFound))
                || e.to_string().contains("Statechain id not found");
            if missing {
                Ok(None)
            } else {
                Err(anyhow!("backup rows for '{statechain_id}' could not be read: {e}"))
            }
        }
    }
}

/// Every backup row for a wallet as `(statechain_id_key, raw_txs_json)`, including the pseudo-keys
/// `branch-<id>` (off-chain exit branch) and `parents-<id>` (terminal-ancestor list). Used to
/// export a full recovery bundle: this exit material lives ONLY on the owner's disk and the SE
/// cannot re-serve it after a claim.
pub async fn get_all_backup_txs(pool: &Pool<Sqlite>, wallet_name: &str) -> Result<Vec<(String, String)>> {
    let query = "SELECT statechain_id, txs FROM backup_txs WHERE wallet_name = $1";
    let rows = sqlx::query(query).bind(wallet_name).fetch_all(pool).await?;
    Ok(rows.iter().map(|r| (r.get::<String, _>(0), r.get::<String, _>(1))).collect())
}

/// Insert a raw backup row (statechain_id key + txs JSON string) verbatim, replacing any existing
/// row for that key. Used by recovery-bundle import to restore branch-*/parents-*/backup rows, and
/// by the in-ladder split's write-ahead journal (`tesr::journal_write`).
///
/// **[B4] ATOMIC — DELETE + INSERT in ONE transaction, and it must stay that way.** This used to
/// execute the two statements against the pool directly, i.e. as two independent auto-committed
/// transactions with a real window between them in which the row does NOT EXIST. Every caller of
/// this function treats the write as a replace, but the crash-durability argument the split journal
/// rests on is stronger than that: `journal_write` advances one record through its stages, so a
/// crash inside that window destroys the PREVIOUS stage's record as well as failing to write the new
/// one — the write-ahead log evaporates at exactly the moment it is being relied on, and the split
/// it was protecting (a terminalized parent, unregenerable co-signatures) becomes invisible to the
/// recovery reader. Narrower than having no journal at all, and the same failure.
///
/// Wrapping both statements in one transaction makes the row transition old→new with no observable
/// intermediate state: a crash before `commit` leaves the PREVIOUS record intact (recovery replays
/// from the older stage, which is always safe — every stage is idempotent by construction), and a
/// crash after it leaves the new one. There is no ordering in which the record is absent.
pub async fn insert_raw_backup_txs(pool: &Pool<Sqlite>, wallet_name: &str, statechain_id: &str, txs_json: &str) -> Result<()> {
    let mut transaction = pool.begin().await?;

    let _ = sqlx::query("DELETE FROM backup_txs WHERE statechain_id = $1 AND wallet_name = $2")
        .bind(statechain_id)
        .bind(wallet_name)
        .execute(&mut *transaction)
        .await?;
    sqlx::query("INSERT INTO backup_txs (statechain_id, wallet_name, txs) VALUES ($1, $2, $3)")
        .bind(statechain_id)
        .bind(wallet_name)
        .bind(txs_json)
        .execute(&mut *transaction)
        .await?;

    transaction.commit().await?;
    Ok(())
}

pub async fn insert_or_update_backup_txs(pool: &Pool<Sqlite>, wallet_name: &str, statechain_id: &str, backup_txs: &Vec<BackupTx>) -> Result<()> {

    let mut transaction = pool.begin().await?;

    let backup_txs_json = json!(backup_txs).to_string();

    let query = "DELETE FROM backup_txs WHERE statechain_id = $1 AND wallet_name = $2";

    let _ = sqlx::query(query)
            .bind(statechain_id)
            .bind(wallet_name)
            .execute(&mut *transaction)
            .await?;

    let query = "INSERT INTO backup_txs (statechain_id, wallet_name, txs) VALUES ($1, $2, $3)";

    let _ = sqlx::query(query)
            .bind(statechain_id)
            .bind(wallet_name)
            .bind(backup_txs_json)
            .execute(&mut *transaction)
            .await?;

    transaction.commit().await?;
    
    Ok(())
}