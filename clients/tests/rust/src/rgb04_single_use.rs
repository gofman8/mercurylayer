//! E2E (security probe, RGB_E2E=4): **SE single-use** — the coordinator refuses to co-sign two
//! CONFLICTING spends of one `single_use` node. SE-honesty substitutes for Bitcoin mining as the
//! single-use enforcer on a coin that has no other exit material.
//!
//! Re-derived for the ladder rule. A `single_use` deposit is the one deposit shape that gets NO
//! TES-R ladder at first sight (`coin_status::check_deposit`: the SE refuses any second co-sign on
//! such a coin, so a three-tier ladder cannot exist over it) — and, since `create_tx1` is gone, no
//! flat backup either. Its ONLY exit is whatever single spend the SE co-signs. So the probe first
//! pins that shape, then proves the guard:
//!
//!   0. After `update_coins` the coin is CONFIRMED with NO `tesr-` row, NO flat backup row and
//!      `locktime == None`. (If `check_deposit` ever laddered a `single_use` coin, the ladder would
//!      consume the one co-sign and spend #1 below would be the SE's refusal, not spend #2.)
//!   1. Spend #1: a coloured WITHDRAWAL of the whole allocation (`create_colored_backup_tx`,
//!      `is_withdrawal = true`) — the SE co-signs it.
//!   2. Spend #2: a second coloured withdrawal of the same coin to a different output — the SE MUST
//!      refuse it, and the refusal must be the single-use gate.
//!
//! Run with RGB_E2E=4. Requires the regtest + Mercury (lockbox) stack.

use std::{env, fs, process::Command, thread, time::Duration};

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

async fn deposit_coin<F>(cc: &ClientConfig, wallet_name: &str, size_sat: u32, fund: F) -> Result<String>
where
    F: FnOnce(&str) -> Result<String>,
{
    let wallet = mercuryrustlib::wallet::create_wallet(wallet_name, cc).await?;
    mercuryrustlib::sqlite_manager::insert_wallet(&cc.pool, &wallet).await?;
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    let token_id = crate::utils::handle_token_response(cc, &token).await?;
    // Single-use deposit: the SE must refuse a second spend of this node (the security under test).
    let sc_address =
        mercuryrustlib::deposit::get_deposit_bitcoin_address_single_use(cc, &wallet.name, &token_id, size_sat).await?;
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

/// The exit-material shape of a `single_use` deposit under the ladder rule: nothing pre-signed at
/// all — no `tesr-` row (the SE would refuse the ladder's second co-sign), no flat backup row
/// (`create_tx1` no longer exists), and no absolute calendar on the coin.
async fn assert_single_use_shape(cc: &ClientConfig, wallet_name: &str, coin: &Coin) -> Result<()> {
    let sid = coin.statechain_id.clone().ok_or(anyhow!("coin has no statechain id"))?;
    assert!(coin.single_use, "the probe coin must be a single_use deposit");
    assert!(
        mercuryrustlib::tesr::load(cc, wallet_name, &sid).await?.is_none(),
        "a single_use coin must carry NO TES-R ladder: laddering it would spend its only co-sign \
         and the probe below would measure the ladder, not the guard"
    );
    let flat = mercuryrustlib::sqlite_manager::try_get_backup_txs(&cc.pool, wallet_name, &sid).await?;
    assert!(
        flat.as_ref().map(|v| v.is_empty()).unwrap_or(true),
        "a deposit must carry NO flat backup row (create_tx1 is gone); found {:?} row(s) for {sid}",
        flat.map(|v| v.len())
    );
    assert!(
        coin.locktime.is_none(),
        "no coin carries an absolute calendar: locktime must be None, got {:?}",
        coin.locktime
    );
    Ok(())
}

pub async fn execute() -> Result<()> {
    let _ = Command::new("rm").arg("wallet.db").arg("wallet.db-shm").arg("wallet.db-wal").output();
    let _ = fs::remove_dir_all("./rgb-data4");
    env::set_var("ML_NETWORK", "regtest");
    let cc = mercuryrustlib::client_config::load().await;

    let (mut issuer, contract) = tokio::task::block_in_place(|| setup("./rgb-data4/issuer", true, true))?;
    let contract = contract.unwrap();
    println!("RGB04 - issued {ISSUED} units of {contract}");

    // Deposit ROOT (full 1000) as a single_use coin; register it.
    let sources: Vec<String> = tokio::task::block_in_place(|| issuer.list_allocations(&contract))?
        .into_iter().map(|(op, _, _)| op).collect();
    let addr_root = deposit_coin(&cc, "rgb04_root", COIN_SAT, |sc| {
        let (_t, _v, _c, signed) = tokio::task::block_in_place(|| {
            issuer.fund_statechain(sc, COIN_SAT as u64, &contract, ISSUED, 2, BLINDING)
        })?;
        Ok(cc.electrum_client.transaction_broadcast_raw(&hex::decode(&signed)?)?.to_string())
    }).await?;
    let coin0 = fresh_coin(&cc, "rgb04_root", &addr_root).await?;
    let (txid_root, vout_root) = (coin0.utxo_txid.clone().unwrap(), coin0.utxo_vout.unwrap());
    tokio::task::block_in_place(|| {
        issuer.register_statechain(&txid_root, vout_root, COIN_SAT as u64, &contract, ISSUED, &sources)
    })?;
    println!("RGB04 - ROOT = {txid_root}:{vout_root} holds {ISSUED}");

    // ---- 0. The shape: a single_use coin has no ladder, no flat backup, no calendar. ----
    assert_single_use_shape(&cc, "rgb04_root", &coin0).await?;
    println!("RGB04 - ROOT is single_use: no `tesr-` row, no flat backup row, locktime None (its only exit is the one spend the SE co-signs)");

    let si = mercuryrustlib::utils::info_config(&cc).await?;
    let out_a = tokio::task::block_in_place(|| issuer.witness_receive(ISSUED))?;
    let addr_a = tokio::task::block_in_place(|| issuer.address_from_recipient_id(&out_a))?;

    // ---- Spend #1: ROOT -> out_a, a coloured WITHDRAWAL (co-signed, not broadcast). ----
    let mut coin1 = coin0.clone();
    let spend1 = mercuryrustlib::rgb::create_colored_backup_tx(
        &cc, &issuer, &mut coin1, &contract, ISSUED, &addr_a, 0, true, None, NETWORK,
        si.fee_rate_sats_per_byte, si.initlock, si.interval, BLINDING, None, None,
    ).await;
    assert!(spend1.is_ok(), "spend #1 of ROOT must co-sign: {:?}", spend1.as_ref().err());
    println!("RGB04 - spend #1 of ROOT co-signed OK (tx {})", spend1.unwrap().txid);

    // ---- Spend #2: ROOT -> out_b (a CONFLICTING second spend of the same coin). ----
    let out_b = tokio::task::block_in_place(|| issuer.witness_receive(ISSUED))?;
    let addr_b = tokio::task::block_in_place(|| issuer.address_from_recipient_id(&out_b))?;
    let mut coin2 = fresh_coin(&cc, "rgb04_root", &addr_root).await?; // re-fetch (fresh nonce state)
    let spend2 = mercuryrustlib::rgb::create_colored_backup_tx(
        &cc, &issuer, &mut coin2, &contract, ISSUED, &addr_b, 0, true, None, NETWORK,
        si.fee_rate_sats_per_byte, si.initlock, si.interval, BLINDING, None, None,
    ).await;

    match &spend2 {
        Err(e) => println!("RGB04 - spend #2 REFUSED by the SE -> single-use ENFORCED \u{2713} ({e})"),
        Ok(tx) => println!("RGB04 - spend #2 CO-SIGNED (tx {}) -> single-use NOT enforced (double-spend gap!)", tx.txid),
    }
    assert!(spend2.is_err(),
        "SE must REFUSE a second conflicting spend of a single-use node (off-chain double-spend guard)");
    let msg = spend2.err().unwrap().to_string().to_lowercase();
    assert!(
        msg.contains("single-use") || msg.contains("single use") || msg.contains("already"),
        "the refusal must be the single-use gate, not an unrelated failure: {msg}"
    );

    // The coin is still exactly what it was: no ladder appeared, no backup row was written.
    let coin_after = fresh_coin(&cc, "rgb04_root", &addr_root).await?;
    assert_single_use_shape(&cc, "rgb04_root", &coin_after).await?;

    println!("RGB04 - SUCCESS: a single_use coin carries no ladder and no flat backup; the SE co-signed its one spend and REFUSED the conflicting second one.");
    Ok(())
}
