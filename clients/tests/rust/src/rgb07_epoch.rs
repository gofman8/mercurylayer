//! E2E (Stage 4 — epoch deadline / bounded exit window): the SE refuses to co-sign any NEW spend of
//! a coin once its own clock passes the coin's `epoch_deadline`. This makes the design's trust model
//! real and enforced ("trust = SE honest + exit before the epoch deadline"). The crucial property:
//! **unilateral exit needs no SE co-signature** — the owner just broadcasts an already-co-signed
//! branch — so the deadline only bounds when the owner must transact/exit by; funds are never stuck.
//!
//! Probe:
//!   A (active):  deposit with a FAR epoch, co-sign a spend  -> SE co-signs OK (active period).
//!   B (expired): deposit with a NEAR epoch, wait past it, attempt the FIRST co-sign -> SE REFUSES.
//!                B is fresh (0 finalized signatures), so single-use (>=1) cannot be the cause —
//!                the refusal uniquely proves epoch enforcement.
//!   exit:        broadcast A's pre-co-signed branch AFTER B's deadline -> confirms on-chain with no
//!                further SE call (unilateral exit needs no co-signature).
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
const OUT_SAT: u64 = 40_000;
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
    println!("RGB07 - A = {txid_a}:{vout_a} holds {ISSUED}, epoch={epoch_a} (now={})", now_unix());

    // Co-sign A -> out_a within the active period. The SE must co-sign.
    let si = mercuryrustlib::utils::info_config(&cc).await?;
    let out_a = tokio::task::block_in_place(|| issuer.witness_receive(ISSUED))?;
    let recv_a = tokio::task::block_in_place(|| issuer.address_from_recipient_id(&out_a))?;
    let spend_a = mercuryrustlib::rgb::create_colored_combine_tx(
        &cc, &issuer, std::slice::from_mut(&mut coin_a), &contract,
        &[(recv_a, OUT_SAT, ISSUED)], 1, true, None, NETWORK, si.initlock, si.interval, BLINDING,
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
    println!("RGB07 - B deposited, epoch={epoch_b}");

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

    // ===== Exit: broadcast A's pre-co-signed branch (no SE call) AFTER B's deadline. =====
    let exit_txid = cc.electrum_client.transaction_broadcast_raw(&hex::decode(&spend_a.signed_tx)?)?;
    let core = bitcoin_core::getnewaddress()?;
    let _ = bitcoin_core::generatetoaddress(cc.confirmation_target, &core)?;
    println!("RGB07 - exited A unilaterally by broadcasting its branch (txid {exit_txid}), no SE co-signature needed \u{2713}");

    println!("RGB07 - SUCCESS: SE enforces the epoch deadline - it co-signs inside the active period, REFUSES a new co-signature past the deadline, and unilateral exit (broadcasting a pre-co-signed branch) needs no SE involvement.");
    Ok(())
}
