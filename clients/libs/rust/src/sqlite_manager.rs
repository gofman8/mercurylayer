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
/// row for that key. Used by recovery-bundle import to restore branch-*/parents-*/backup rows.
pub async fn insert_raw_backup_txs(pool: &Pool<Sqlite>, wallet_name: &str, statechain_id: &str, txs_json: &str) -> Result<()> {
    let _ = sqlx::query("DELETE FROM backup_txs WHERE statechain_id = $1 AND wallet_name = $2")
        .bind(statechain_id)
        .bind(wallet_name)
        .execute(pool)
        .await?;
    sqlx::query("INSERT INTO backup_txs (statechain_id, wallet_name, txs) VALUES ($1, $2, $3)")
        .bind(statechain_id)
        .bind(wallet_name)
        .bind(txs_json)
        .execute(pool)
        .await?;
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