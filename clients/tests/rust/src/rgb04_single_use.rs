//! E2E (security probe): **SE single-use** — the off-chain model's double-spend guard is the SE
//! refusing to co-sign two CONFLICTING spends of the same node outpoint (SE-honesty substitutes for
//! Bitcoin mining as the single-use enforcer). Without it, an owner could off-chain co-sign two txs
//! spending the same coin and broadcast whichever they like.
//!
//! Probe: deposit ROOT (a statechain coin), co-sign spend #1 (ROOT -> out_a, un-broadcast), then
//! attempt to co-sign spend #2 (ROOT -> out_b, also spending ROOT). The SE MUST refuse #2.
//!
//! Run with RGB_E2E=4. Requires the regtest + Mercury (lockbox) stack.

use std::{env, fs, process::Command, str::FromStr, thread, time::Duration};

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

pub async fn execute() -> Result<()> {
    let _ = Command::new("rm").arg("wallet.db").arg("wallet.db-shm").arg("wallet.db-wal").output();
    let _ = fs::remove_dir_all("./rgb-data4");
    env::set_var("ML_NETWORK", "regtest");
    let cc = mercuryrustlib::client_config::load().await;

    let (mut issuer, contract) = tokio::task::block_in_place(|| setup("./rgb-data4/issuer", true, true))?;
    let contract = contract.unwrap();
    println!("RGB04 - issued {ISSUED} units of {contract}");

    // Deposit ROOT (full 1000); register it.
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

    let si = mercuryrustlib::utils::info_config(&cc).await?;
    let out_a = tokio::task::block_in_place(|| issuer.witness_receive(ISSUED))?;
    let addr_a = tokio::task::block_in_place(|| issuer.address_from_recipient_id(&out_a))?;

    // ---- Spend #1: ROOT -> out_a (co-sign, un-broadcast). ----
    let mut coin1 = coin0.clone();
    let spend1 = mercuryrustlib::rgb::create_colored_combine_tx(
        &cc, &issuer, std::slice::from_mut(&mut coin1), &contract,
        &[(addr_a.clone(), OUT_SAT, ISSUED)], 1, true, None, NETWORK, si.initlock, si.interval, BLINDING,
    ).await;
    assert!(spend1.is_ok(), "spend #1 of ROOT must co-sign");
    println!("RGB04 - spend #1 of ROOT co-signed OK (tx {})", spend1.unwrap().txid);

    // ---- Spend #2: ROOT -> out_b (a CONFLICTING second spend of the same coin). ----
    let out_b = tokio::task::block_in_place(|| issuer.witness_receive(ISSUED))?;
    let addr_b = tokio::task::block_in_place(|| issuer.address_from_recipient_id(&out_b))?;
    let mut coin2 = fresh_coin(&cc, "rgb04_root", &addr_root).await?; // re-fetch (fresh nonce state)
    let spend2 = mercuryrustlib::rgb::create_colored_combine_tx(
        &cc, &issuer, std::slice::from_mut(&mut coin2), &contract,
        &[(addr_b.clone(), OUT_SAT, ISSUED)], 1, true, None, NETWORK, si.initlock, si.interval, BLINDING,
    ).await;

    match &spend2 {
        Err(e) => println!("RGB04 - spend #2 REFUSED by the SE -> single-use ENFORCED \u{2713} ({e})"),
        Ok(tx) => println!("RGB04 - spend #2 CO-SIGNED (tx {}) -> single-use NOT enforced (double-spend gap!)", tx.txid),
    }
    assert!(spend2.is_err(),
        "SE must REFUSE a second conflicting spend of a single-use node (off-chain double-spend guard)");

    println!("RGB04 - SUCCESS: SE enforces single-use - the conflicting second spend of ROOT was refused.");
    Ok(())
}
