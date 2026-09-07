//! E2E (RGB_E2E=7 — the coordinator's `epoch_deadline` gate): the SE refuses to co-sign any NEW
//! spend of a coin once its own clock passes the coin's `epoch_deadline`. This is a SERVER-side
//! gate on `sign/first` (`sign.rs`: `now >= epoch_deadline`), independent of how the coin exits.
//!
//! Re-derived for the ladder rule. The coins here are `single_use` deposits with an epoch
//! (`get_deposit_bitcoin_address_single_use_epoch`) — the one deposit shape that gets NO TES-R
//! ladder at first sight (the SE refuses a second co-sign on a single_use coin) and, since
//! `create_tx1` is gone, no flat backup either. So the shape is pinned first, then the gate:
//!
//!   A (active):  deposit with a FAR epoch; assert no ladder / no flat backup / `locktime == None`;
//!                co-sign one coloured WITHDRAWAL of the whole allocation -> the SE co-signs.
//!   B (expired): deposit with a NEAR epoch, wait past it, attempt the FIRST co-sign -> the SE
//!                REFUSES and the refusal names the epoch. B is fresh (0 finalized signatures), so
//!                single-use (>=1) cannot be the cause — the refusal uniquely proves epoch
//!                enforcement.
//!   exit:        broadcast A's already-co-signed withdrawal AFTER B's deadline -> it confirms with
//!                no further SE call. A pre-signed spend needs no SE cooperation to reach the chain;
//!                the deadline bounds NEW co-signatures, never the broadcast of old ones.
//!
//! Run with RGB_E2E=7. Requires the regtest + Mercury (lockbox) stack.

use std::{env, fs, process::Command, thread, time::{Duration, SystemTime, UNIX_EPOCH}};

use anyhow::{anyhow, Result};
use electrum_client::ElectrumApi;
use mercury_rgb::RgbWallet;
use mercuryrustlib::{client_config::ClientConfig, Coin, CoinStatus};

use crate::{bitcoin_core, electrs};

const ELECTRUM_URL: &str = "127.0.0.1:50001";
const RGB_PROXY: &str = "rpc://127.0.0.1:3000/json-rpc";
const NETWORK: &str = "regtest";
const BLINDING: u64 = 71;
const ISSUED: u64 = 1000;
const COIN_SAT: u32 = 60_000;
const EPOCH_FAR: u64 = 3600; // A: comfortably inside the active period when co-signed
const EPOCH_NEAR: u64 = 5;   // B: short window so it expires during the test

fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

async fn wait_for_address(cc: &ClientConfig, address: &str, amount: u32) -> Result<()> {
    for _ in 0..60 {
        if electrs::check_address(cc, address, amount).await? {
            return Ok(());
        }
        thread::sleep(Duration::from_secs(1));
    }
    Err(anyhow!("address {address} not indexed in time"))
}

fn setup(data_dir: &str, issue: bool, make_utxos: bool) -> Result<(RgbWallet, Option<String>)> {
    let _ = fs::create_dir_all(data_dir);
    let mnemonic = RgbWallet::generate_mnemonic(NETWORK)?;
    let mut rgb = RgbWallet::open(data_dir, &mnemonic, NETWORK, ELECTRUM_URL, RGB_PROXY)?;
    let address = rgb.get_address()?;
    let _ = bitcoin_core::sendtoaddress(500_000, &address)?;
    let core = bitcoin_core::getnewaddress()?;
    let _ = bitcoin_core::generatetoaddress(6, &core)?;
    rgb.refresh(None)?;
    if make_utxos {
        rgb.create_utxos(1, 200_000, 2)?;
    }
    let contract = if issue {
        Some(rgb.issue_nia("RGBSC", "RGB Statechain Asset", 0, vec![ISSUED])?)
    } else {
        None
    };
    Ok((rgb, contract))
}

/// Deposit a single-use coin with an epoch deadline, funded by the given closure, and confirm it.
async fn deposit_epoch_coin<F>(cc: &ClientConfig, wallet_name: &str, size_sat: u32, epoch_deadline: u64, fund: F) -> Result<String>
where
    F: FnOnce(&str) -> Result<String>,
{
    let wallet = mercuryrustlib::wallet::create_wallet(wallet_name, cc).await?;
    mercuryrustlib::sqlite_manager::insert_wallet(&cc.pool, &wallet).await?;
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    let token_id = crate::utils::handle_token_response(cc, &token).await?;
    let sc_address = mercuryrustlib::deposit::get_deposit_bitcoin_address_single_use_epoch(
        cc, &wallet.name, &token_id, size_sat, epoch_deadline,
    ).await?;
    let _txid = fund(&sc_address)?;
    wait_for_address(cc, &sc_address, size_sat).await?;
    let core = bitcoin_core::getnewaddress()?;
    let _ = bitcoin_core::generatetoaddress(cc.confirmation_target, &core)?;
    mercuryrustlib::coin_status::update_coins(cc, &wallet.name).await?;
    Ok(sc_address)
}

async fn fresh_coin(cc: &ClientConfig, wallet_name: &str, sc_address: &str) -> Result<Coin> {
    let coin = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, wallet_name)
        .await?
        .coins.iter()
        .find(|c| c.aggregated_address.as_deref() == Some(sc_address))
        .ok_or(anyhow!("coin not found for {sc_address}"))?.clone();
    assert!(coin.status == CoinStatus::CONFIRMED, "statechain coin must confirm");
    Ok(coin)
}

/// The exit-material shape of a `single_use` deposit under the ladder rule: no `tesr-` row, no flat
/// backup row, no absolute calendar.
async fn assert_single_use_shape(cc: &ClientConfig, wallet_name: &str, coin: &Coin) -> Result<()> {
    let sid = coin.statechain_id.clone().ok_or(anyhow!("coin has no statechain id"))?;
    assert!(coin.single_use, "the probe coin must be a single_use deposit");
    assert!(
        mercuryrustlib::tesr::load(cc, wallet_name, &sid).await?.is_none(),
        "a single_use coin must carry NO TES-R ladder (the SE refuses its second co-sign)"
    );
    let flat = mercuryrustlib::sqlite_manager::try_get_backup_txs(&cc.pool, wallet_name, &sid).await?;
    assert!(
        flat.as_ref().map(|v| v.is_empty()).unwrap_or(true),
        "a deposit must carry NO flat backup row (create_tx1 is gone); found {:?} row(s) for {sid}",
        flat.map(|v| v.len())
    );
    assert!(coin.locktime.is_none(), "no coin carries an absolute calendar: locktime must be None");
    Ok(())
}

pub async fn execute() -> Result<()> {
    let _ = Command::new("rm").arg("wallet.db").arg("wallet.db-shm").arg("wallet.db-wal").output();
    let _ = fs::remove_dir_all("./rgb-data7");
    env::set_var("ML_NETWORK", "regtest");
    let cc = mercuryrustlib::client_config::load().await;

    let (mut issuer, contract) = tokio::task::block_in_place(|| setup("./rgb-data7/issuer", true, true))?;
    let contract = contract.unwrap();
    println!("RGB07 - issued {ISSUED} units of {contract}");

    let sources: Vec<String> = tokio::task::block_in_place(|| issuer.list_allocations(&contract))?
        .into_iter().map(|(op, _, _)| op).collect();

    // ===== Coin A (ACTIVE): far epoch, asset-bearing, will be co-signed and later exited. =====
    let epoch_a = now_unix() + EPOCH_FAR;
    let addr_a_coin = deposit_epoch_coin(&cc, "rgb07_a", COIN_SAT, epoch_a, |sc| {
        let (_t, _v, _c, signed) = tokio::task::block_in_place(|| {
            issuer.fund_statechain(sc, COIN_SAT as u64, &contract, ISSUED, 2, BLINDING)
        })?;
        Ok(cc.electrum_client.transaction_broadcast_raw(&hex::decode(&signed)?)?.to_string())
    }).await?;
    let mut coin_a = fresh_coin(&cc, "rgb07_a", &addr_a_coin).await?;
    let (txid_a, vout_a) = (coin_a.utxo_txid.clone().unwrap(), coin_a.utxo_vout.unwrap());
    tokio::task::block_in_place(|| {
        issuer.register_statechain(&txid_a, vout_a, COIN_SAT as u64, &contract, ISSUED, &sources)
    })?;
    assert_single_use_shape(&cc, "rgb07_a", &coin_a).await?;
    println!("RGB07 - A = {txid_a}:{vout_a} holds {ISSUED}, epoch={epoch_a} (now={}); no ladder, no flat backup, locktime None", now_unix());

    // Co-sign A's withdrawal within the active period. The SE must co-sign.
    let si = mercuryrustlib::utils::info_config(&cc).await?;
    let out_a = tokio::task::block_in_place(|| issuer.witness_receive(ISSUED))?;
    let recv_a = tokio::task::block_in_place(|| issuer.address_from_recipient_id(&out_a))?;
    let spend_a = mercuryrustlib::rgb::create_colored_backup_tx(
        &cc, &issuer, &mut coin_a, &contract, ISSUED, &recv_a, 0, true, None, NETWORK,
        si.fee_rate_sats_per_byte, si.initlock, si.interval, BLINDING, None, None,
    ).await;
    assert!(spend_a.is_ok(), "A must co-sign inside the active period: {:?}", spend_a.err());
    let spend_a = spend_a.unwrap();
    println!("RGB07 - A co-signed inside active period \u{2713} (tx {})", spend_a.txid);

    // ===== Coin B (EXPIRED): near epoch, BTC-only, fresh (never co-signed) refusal probe. =====
    let epoch_b = now_unix() + EPOCH_NEAR;
    let addr_b_coin = deposit_epoch_coin(&cc, "rgb07_b", COIN_SAT, epoch_b, |sc| {
        Ok(bitcoin_core::sendtoaddress(COIN_SAT, sc)?)
    }).await?;
    let coin_b = fresh_coin(&cc, "rgb07_b", &addr_b_coin).await?;
    assert_single_use_shape(&cc, "rgb07_b", &coin_b).await?;
    println!("RGB07 - B deposited, epoch={epoch_b}; no ladder, no flat backup (0 co-signs so far)");

    // Wait until the SE's clock is past B's deadline.
    let target = epoch_b + 2;
    while now_unix() < target {
        thread::sleep(Duration::from_secs(1));
    }
    println!("RGB07 - now={} > B.epoch={epoch_b}; attempting B's FIRST co-sign", now_unix());

    // Attempt B's first co-sign (a raw sign_first round — the SE refuses before any coloring).
    let coin_nonce = mercuryrustlib::create_and_commit_nonces(&coin_b)?;
    let probe = mercuryrustlib::transaction::sign_first(&cc, &coin_nonce.sign_first_request_payload).await;
    match &probe {
        Err(e) => println!("RGB07 - B co-sign REFUSED -> epoch ENFORCED \u{2713} ({e})"),
        Ok(_) => println!("RGB07 - B co-sign ACCEPTED -> epoch NOT enforced (gap!)"),
    }
    assert!(probe.is_err(), "SE must REFUSE a new co-signature past the epoch deadline");
    let msg = probe.err().unwrap().to_string();
    assert!(msg.contains("epoch"), "refusal must be the epoch gate (fresh coin can't trip single-use): {msg}");

    // ===== Exit: broadcast A's pre-co-signed withdrawal (no SE call) AFTER B's deadline. =====
    let exit_txid = cc.electrum_client.transaction_broadcast_raw(&hex::decode(&spend_a.signed_tx)?)?;
    let core = bitcoin_core::getnewaddress()?;
    let _ = bitcoin_core::generatetoaddress(cc.confirmation_target, &core)?;
    println!("RGB07 - A's pre-co-signed withdrawal broadcast (txid {exit_txid}) with no further SE call \u{2713}");

    println!("RGB07 - SUCCESS: single_use epoch coins carry no ladder and no flat backup; the SE co-signs inside the active period, REFUSES a new co-signature past the deadline, and an already-co-signed spend reaches the chain without SE involvement.");
    Ok(())
}
