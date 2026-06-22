//! E2E: **RGB anchor refresh via statechain self-transfer** (see `docs/rgb_anchor_refresh.md`).
//!
//! The asset is assigned to a statechain UTXO X that stays unspent on Bitcoin. We update the RGB
//! commitment *without broadcasting* by transferring the coin from the owner to the owner: each
//! self-transfer produces a new server-co-signed colored backup/exit tx that spends the same X,
//! carries a new RGB commitment, has a lower nLockTime, and rotates the owner key-share. Only on
//! `exit` (broadcasting the latest backup tx) does the RGB transition become Bitcoin-confirmed.
//!
//! Detailed flow (each step is asserted below):
//!   1. Issue 1000, deposit onto statechain UTXO X (color deposit), register X (asset assigned to X).
//!   2. REFRESH #1 (self-transfer): new colored backup tx spends the same X, new commitment, lower
//!      nLockTime, tx_n incremented, owner key-share rotated, and X is still UNSPENT on-chain
//!      (statechain-accepted, NOT Bitcoin-confirmed).
//!   3. REFRESH #2: refresh again -> nLockTime drops further, tx_n increments again, X still unspent.
//!   4. EXIT: mine to the latest backup tx's nLockTime, broadcast it -> X is finally spent on-chain
//!      (the refreshed RGB anchor becomes Bitcoin-confirmed).
//!
//! Run with RGB_E2E=7. Requires the regtest + Mercury (lockbox) stack.

use std::{env, fs, process::Command, str::FromStr, thread, time::Duration};

use anyhow::{anyhow, Result};
use electrum_client::ElectrumApi;
use mercury_rgb::RgbWallet;
use mercuryrustlib::{client_config::ClientConfig, rgb::RgbStatechainStatus, CoinStatus};

use crate::{bitcoin_core, electrs};

const ELECTRUM_URL: &str = "127.0.0.1:50001";
const RGB_PROXY: &str = "rpc://127.0.0.1:3000/json-rpc";
const NETWORK: &str = "regtest";
const BLINDING: u64 = 31;
const ISSUED: u64 = 1000;
const COIN_SAT_X: u32 = 50_000;

async fn wait_for_address(cc: &ClientConfig, address: &str, amount: u32) -> Result<()> {
    for _ in 0..60 {
        if electrs::check_address(cc, address, amount).await? {
            return Ok(());
        }
        thread::sleep(Duration::from_secs(1));
    }
    Err(anyhow!("address {address} not indexed in time"))
}

fn is_outpoint_spent(cc: &ClientConfig, txid: &str, vout: u32) -> bool {
    use electrum_client::bitcoin::Txid;
    let raw = match cc.electrum_client.transaction_get_raw(&Txid::from_str(txid).unwrap()) {
        std::result::Result::Ok(r) => r,
        _ => return false,
    };
    let tx: electrum_client::bitcoin::Transaction =
        electrum_client::bitcoin::consensus::deserialize(&raw).unwrap();
    let spk = &tx.output[vout as usize].script_pubkey;
    let listed = cc.electrum_client.script_list_unspent(spk).unwrap_or_default();
    !listed.iter().any(|u| u.tx_hash.to_string() == txid && u.tx_pos as u32 == vout)
}

fn setup(data_dir: &str) -> Result<(RgbWallet, String)> {
    let _ = fs::create_dir_all(data_dir);
    let mnemonic = RgbWallet::generate_mnemonic(NETWORK)?;
    let mut rgb = RgbWallet::open(data_dir, &mnemonic, NETWORK, ELECTRUM_URL, RGB_PROXY)?;
    let address = rgb.get_address()?;
    let _ = bitcoin_core::sendtoaddress(500_000, &address)?;
    let core = bitcoin_core::getnewaddress()?;
    let _ = bitcoin_core::generatetoaddress(6, &core)?;
    rgb.refresh(None)?;
    rgb.create_utxos(1, 200_000, 2)?;
    let contract = rgb.issue_nia("RGBSC", "RGB Statechain Asset", 0, vec![ISSUED])?;
    Ok((rgb, contract))
}

/// Find the spendable (duplicate_index 0) coin for a statechain_id and return its auth_pubkey/status.
async fn owner_auth_and_status(cc: &ClientConfig, wallet_name: &str, statechain_id: &str, status: CoinStatus) -> Result<String> {
    let w = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, wallet_name).await?;
    w.coins
        .iter()
        .find(|c| c.statechain_id.as_deref() == Some(statechain_id) && c.duplicate_index == 0 && c.status == status)
        .map(|c| c.auth_pubkey.clone())
        .ok_or(anyhow!("no {status:?} coin for {statechain_id}"))
}

pub async fn execute() -> Result<()> {
    let _ = Command::new("rm").arg("wallet.db").arg("wallet.db-shm").arg("wallet.db-wal").output();
    let _ = fs::remove_dir_all("./rgb-data7");
    env::set_var("ML_NETWORK", "regtest");
    let cc = mercuryrustlib::client_config::load().await;

    // ---- 1. Issue + deposit onto statechain UTXO X + register (asset assigned to X). ----
    let (mut issuer, contract) = tokio::task::block_in_place(|| setup("./rgb-data7/issuer"))?;
    println!("RGB07 - issued {ISSUED} units of {contract}");

    let wallet_name = "rgb07_owner";
    let wallet = mercuryrustlib::wallet::create_wallet(wallet_name, &cc).await?;
    mercuryrustlib::sqlite_manager::insert_wallet(&cc.pool, &wallet).await?;
    let token = mercuryrustlib::deposit::get_token(&cc).await?;
    let token_id = crate::utils::handle_token_response(&cc, &token).await?;
    let sc_address =
        mercuryrustlib::deposit::get_deposit_bitcoin_address(&cc, &wallet.name, &token_id, COIN_SAT_X).await?;

    let sources: Vec<String> = tokio::task::block_in_place(|| issuer.list_allocations(&contract))?
        .into_iter().map(|(op, _, _)| op).collect();
    let (_t, _v, _c, signed) = tokio::task::block_in_place(|| {
        issuer.fund_statechain(&sc_address, COIN_SAT_X as u64, &contract, ISSUED, 2, BLINDING)
    })?;
    let _ = cc.electrum_client.transaction_broadcast_raw(&hex::decode(&signed)?)?;
    wait_for_address(&cc, &sc_address, COIN_SAT_X).await?;
    let core = bitcoin_core::getnewaddress()?;
    let _ = bitcoin_core::generatetoaddress(cc.confirmation_target, &core)?;
    mercuryrustlib::coin_status::update_coins(&cc, &wallet.name).await?;

    let coin = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, &wallet.name).await?
        .coins.iter()
        .find(|c| c.aggregated_address.as_deref() == Some(sc_address.as_str()))
        .ok_or(anyhow!("coin not found"))?.clone();
    assert!(coin.status == CoinStatus::CONFIRMED, "deposited statechain coin must confirm");
    let statechain_id = coin.statechain_id.clone().unwrap();
    let x_txid = coin.utxo_txid.clone().unwrap();
    let x_vout = coin.utxo_vout.unwrap();
    tokio::task::block_in_place(|| {
        issuer.register_statechain(&x_txid, x_vout, COIN_SAT_X as u64, &contract, ISSUED, &sources)
    })?;
    let bal = tokio::task::block_in_place(|| issuer.settled_balance(&contract))?;
    println!("RGB07 - asset assigned to statechain UTXO X = {x_txid}:{x_vout} (balance {bal}); X unspent on-chain = {}", !is_outpoint_spent(&cc, &x_txid, x_vout));
    assert_eq!(bal, ISSUED);
    assert!(!is_outpoint_spent(&cc, &x_txid, x_vout), "X must be unspent at the start");
    crate::rgb_dump::dump("owner after deposit+register (asset on statechain UTXO X)", &mut issuer, &contract);

    // ---- 2. REFRESH #1: self-transfer that re-commits the RGB anchor without broadcasting. ----
    let r1 = mercuryrustlib::rgb::refresh_rgb_anchor_self_transfer(
        &cc, &issuer, wallet_name, &statechain_id, &contract, ISSUED, BLINDING + 1, NETWORK, None,
    ).await?;
    println!("RGB07 - refresh #1: status={:?} X={} tx_n {}->{} nLockTime {}->{} new_backup_txid={}",
        r1.status, r1.funding_outpoint, r1.previous_tx_n, r1.new_tx_n, r1.previous_nlocktime, r1.new_nlocktime, r1.new_backup_txid);
    assert_eq!(r1.status, RgbStatechainStatus::RgbAnchorRefreshAccepted, "refresh #1 must be accepted");
    assert_eq!(r1.funding_outpoint, format!("{x_txid}:{x_vout}"), "INVARIANT: same funding outpoint X");
    assert!(r1.new_nlocktime < r1.previous_nlocktime, "INVARIANT: lower nLockTime");
    assert_eq!(r1.new_tx_n, r1.previous_tx_n + 1, "INVARIANT: state index (tx_n) increments");
    // INVARIANT: statechain-accepted, NOT Bitcoin-confirmed -> X is still unspent on-chain.
    assert!(!is_outpoint_spent(&cc, &x_txid, x_vout), "INVARIANT: nothing broadcast; X still unspent");
    // INVARIANT: the owner key-share was rotated (the new CONFIRMED owner has a fresh auth pubkey).
    let new_auth = owner_auth_and_status(&cc, wallet_name, &statechain_id, CoinStatus::CONFIRMED).await?;
    assert_ne!(new_auth, r1.previous_owner_auth_pubkey, "INVARIANT: key-share rotated (auth pubkey changed)");
    println!("RGB07 - refresh #1 verified: same X, lower nLockTime, tx_n+1, key-share rotated, X still UNSPENT (statechain-accepted, not Bitcoin-confirmed)");
    crate::rgb_dump::dump("owner after refresh #1 (new RGB commitment on the same X)", &mut issuer, &contract);

    // ---- 3. REFRESH #2: refresh again on the now-latest state. ----
    let r2 = mercuryrustlib::rgb::refresh_rgb_anchor_self_transfer(
        &cc, &issuer, wallet_name, &statechain_id, &contract, ISSUED, BLINDING + 2, NETWORK, None,
    ).await?;
    println!("RGB07 - refresh #2: tx_n {}->{} nLockTime {}->{}", r2.previous_tx_n, r2.new_tx_n, r2.previous_nlocktime, r2.new_nlocktime);
    assert_eq!(r2.status, RgbStatechainStatus::RgbAnchorRefreshAccepted);
    assert_eq!(r2.funding_outpoint, format!("{x_txid}:{x_vout}"), "INVARIANT: still the same X across refreshes");
    assert!(r2.new_nlocktime < r1.new_nlocktime, "INVARIANT: each refresh lowers nLockTime further");
    assert_eq!(r2.new_tx_n, r1.new_tx_n + 1, "INVARIANT: tx_n keeps incrementing");
    assert!(!is_outpoint_spent(&cc, &x_txid, x_vout), "INVARIANT: still nothing broadcast after refresh #2");
    println!("RGB07 - refresh #2 verified: nLockTime dropped further, tx_n incremented, X still UNSPENT");

    // ---- 4. EXIT: broadcast the latest colored backup tx -> the anchor becomes Bitcoin-confirmed. ----
    let backups = mercuryrustlib::sqlite_manager::get_backup_txs(&cc.pool, wallet_name, &statechain_id).await?;
    let latest = backups.iter().max_by_key(|b| b.tx_n).ok_or(anyhow!("no backup tx"))?;
    let latest_tx: electrum_client::bitcoin::Transaction =
        electrum_client::bitcoin::consensus::encode::deserialize(&hex::decode(&latest.tx)?)?;
    let latest_locktime = latest_tx.lock_time.to_consensus_u32();
    let height = cc.electrum_client.block_headers_subscribe_raw()?.height as u32;
    if latest_locktime > height {
        let core = bitcoin_core::getnewaddress()?;
        let _ = bitcoin_core::generatetoaddress(latest_locktime - height + 1, &core)?;
    }
    let _ = cc.electrum_client.transaction_broadcast_raw(&hex::decode(&latest.tx)?)?;
    let core = bitcoin_core::getnewaddress()?;
    let _ = bitcoin_core::generatetoaddress(cc.confirmation_target, &core)?;
    // Give electrs a moment to index the spend.
    for _ in 0..30 {
        if is_outpoint_spent(&cc, &x_txid, x_vout) { break; }
        thread::sleep(Duration::from_secs(1));
    }
    println!("RGB07 - exit: broadcast latest backup tx {} (nLockTime {latest_locktime}); X spent on-chain = {}", latest.tx_n, is_outpoint_spent(&cc, &x_txid, x_vout));
    assert!(is_outpoint_spent(&cc, &x_txid, x_vout), "after exit, X must be spent on-chain (Bitcoin-confirmed)");

    println!("RGB07 - SUCCESS: refreshed the RGB anchor twice via self-transfer (same X, lower nLockTime each time, key-share rotated, never broadcast), then exited on-chain.");
    Ok(())
}
