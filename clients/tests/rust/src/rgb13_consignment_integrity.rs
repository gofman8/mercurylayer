//! E2E (rgb-tests parity, negative): **consignment integrity over statechain**. The off-chain
//! receiver must ACCEPT a well-formed consignment for its coin but REJECT one that has been tampered
//! with (payload corrupted) or is presented against the wrong witness. This is the statechain
//! analogue of the upstream `validate_consignment_*_fail` family — the receiver's
//! `validate_offchain_chain` is the single gate that protects it before it books an off-chain coin.
//!
//! Run with RGB_E2E=13. Requires the regtest + Mercury (lockbox) stack.

use std::{env, fs, process::Command, str::FromStr, thread, time::Duration};

use anyhow::{anyhow, Result};
use electrum_client::ElectrumApi;
use mercury_rgb::RgbWallet;
use mercuryrustlib::{client_config::ClientConfig, CoinStatus};

use crate::{bitcoin_core, electrs};

const ELECTRUM_URL: &str = "127.0.0.1:50001";
const RGB_PROXY: &str = "rpc://127.0.0.1:3000/json-rpc";
const NETWORK: &str = "regtest";
const BLINDING: u64 = 131;
const ISSUED: u64 = 600;
const RECV_AMT: u64 = 200;
const CHANGE_AMT: u64 = 400;
const COIN_SAT: u32 = 90_000;
const RECV_SAT: u64 = 30_000;
const CHANGE_SAT: u64 = 20_000;

async fn wait_for_address(cc: &ClientConfig, address: &str, amount: u32) -> Result<()> {
    for _ in 0..60 {
        if electrs::check_address(cc, address, amount).await? {
            return Ok(());
        }
        thread::sleep(Duration::from_secs(1));
    }
    Err(anyhow!("address {address} not indexed in time"))
}

fn setup(data_dir: &str, issue: bool) -> Result<(RgbWallet, Option<String>)> {
    let _ = fs::create_dir_all(data_dir);
    let mnemonic = RgbWallet::generate_mnemonic(NETWORK)?;
    let mut rgb = RgbWallet::open(data_dir, &mnemonic, NETWORK, ELECTRUM_URL, RGB_PROXY)?;
    let address = rgb.get_address()?;
    let _ = bitcoin_core::sendtoaddress(500_000, &address)?;
    let core = bitcoin_core::getnewaddress()?;
    let _ = bitcoin_core::generatetoaddress(6, &core)?;
    rgb.refresh(None)?;
    let contract = if issue {
        rgb.create_utxos(1, 200_000, 2)?;
        Some(rgb.issue_nia("INTGR", "Integrity asset", 0, vec![ISSUED])?)
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
    let sc_address =
        mercuryrustlib::deposit::get_deposit_bitcoin_address_single_use(cc, &wallet.name, &token_id, size_sat).await?;
    let _txid = fund(&sc_address)?;
    wait_for_address(cc, &sc_address, size_sat).await?;
    let core = bitcoin_core::getnewaddress()?;
    let _ = bitcoin_core::generatetoaddress(cc.confirmation_target, &core)?;
    mercuryrustlib::coin_status::update_coins(cc, &wallet.name).await?;
    Ok(sc_address)
}

async fn coin_outpoint(cc: &ClientConfig, wallet_name: &str, sc_address: &str) -> Result<(String, u32, mercuryrustlib::Coin)> {
    let coin = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, wallet_name)
        .await?
        .coins
        .iter()
        .find(|c| c.aggregated_address.as_deref() == Some(sc_address))
        .ok_or(anyhow!("coin not found for {sc_address}"))?
        .clone();
    assert!(coin.status == CoinStatus::CONFIRMED, "statechain coin must confirm");
    let txid = coin.utxo_txid.clone().ok_or(anyhow!("no utxo_txid"))?;
    let vout = coin.utxo_vout.ok_or(anyhow!("no utxo_vout"))?;
    Ok((txid, vout, coin))
}

/// Corrupt a run of base64 chars in the middle of a consignment (keeps it valid base64 but mangles
/// the decoded payload, so the RGB `Transfer` fails to load/validate).
fn tamper_consignment(consignment_b64: &str) -> String {
    let mut t = consignment_b64.to_string();
    let len = t.len();
    if len < 64 {
        return t; // too short to meaningfully tamper (shouldn't happen for a real consignment)
    }
    let start = len / 2;
    // 16 valid-base64 chars, guaranteed different from the original run at this position.
    let run: String = (0..16)
        .map(|i| {
            let orig = t.as_bytes()[start + i];
            let c = if orig == b'A' { b'B' } else { b'A' };
            c as char
        })
        .collect();
    t.replace_range(start..start + 16, &run);
    t
}

fn rejected(res: Result<(bool, Option<String>)>) -> (bool, String) {
    match res {
        Ok((valid, detail)) => (!valid, format!("valid={valid} detail={:?}", detail)),
        Err(e) => (true, format!("Err({e})")),
    }
}

pub async fn execute() -> Result<()> {
    let _ = Command::new("rm").arg("wallet.db").arg("wallet.db-shm").arg("wallet.db-wal").output();
    let _ = fs::remove_dir_all("./rgb-data13");
    env::set_var("ML_NETWORK", "regtest");
    let cc = mercuryrustlib::client_config::load().await;

    let (mut issuer, contract) = tokio::task::block_in_place(|| setup("./rgb-data13/issuer", true))?;
    let contract = contract.unwrap();
    let (mut receiver, _) = tokio::task::block_in_place(|| setup("./rgb-data13/receiver", false))?;
    println!("RGB13 - issued {ISSUED} units of {contract}");

    // Fund + register a colored statechain coin R.
    let sources: Vec<String> = tokio::task::block_in_place(|| issuer.list_allocations(&contract))?
        .into_iter().map(|(op, _, _)| op).collect();
    let addr_r = deposit_coin(&cc, "rgb13_root", COIN_SAT, |sc| {
        let (_t, _v, _c, signed) = tokio::task::block_in_place(|| {
            issuer.fund_statechain(sc, COIN_SAT as u64, &contract, ISSUED, 2, BLINDING)
        })?;
        Ok(cc.electrum_client.transaction_broadcast_raw(&hex::decode(&signed)?)?.to_string())
    }).await?;
    let (txid_r, vout_r, mut coin_r) = coin_outpoint(&cc, "rgb13_root", &addr_r).await?;
    tokio::task::block_in_place(|| {
        issuer.register_statechain(&txid_r, vout_r, COIN_SAT as u64, &contract, ISSUED, &sources)
    })?;

    // Build a real (un-broadcast) split -> receiver 200 + change 400.
    let si = mercuryrustlib::utils::info_config(&cc).await?;
    let recv_id = tokio::task::block_in_place(|| receiver.witness_receive(RECV_AMT))?;
    let recv_addr = tokio::task::block_in_place(|| receiver.address_from_recipient_id(&recv_id))?;
    let change_id = tokio::task::block_in_place(|| issuer.witness_receive(CHANGE_AMT))?;
    let change_addr = tokio::task::block_in_place(|| issuer.address_from_recipient_id(&change_id))?;
    let splits = vec![(recv_addr, RECV_SAT, RECV_AMT), (change_addr, CHANGE_SAT, CHANGE_AMT)];
    let split = mercuryrustlib::rgb::create_colored_split_tx(
        &cc, &issuer, &mut coin_r, &contract, &splits, 1, true, None, NETWORK, si.initlock, si.interval, BLINDING,
    ).await?;
    println!("RGB13 - built split tx {} (NOT broadcast)", split.txid);

    // POSITIVE: the untouched consignment validates against its witness txid.
    let (valid, detail) = tokio::task::block_in_place(|| {
        receiver.validate_offchain_chain(&split.consignment, &[split.txid.clone()])
    })?;
    println!("RGB13 - [POSITIVE] untouched consignment -> valid={valid} detail={:?}", detail);
    assert!(valid, "the well-formed consignment MUST validate off-chain");

    // NEGATIVE 1: a payload-corrupted consignment is rejected.
    let tampered = tamper_consignment(&split.consignment);
    assert_ne!(tampered, split.consignment, "tamper helper must actually change the consignment");
    let (rej1, why1) = rejected(tokio::task::block_in_place(|| {
        receiver.validate_offchain_chain(&tampered, &[split.txid.clone()])
    }));
    println!("RGB13 - [NEGATIVE 1] tampered consignment -> rejected={rej1} ({why1})");
    assert!(rej1, "a payload-corrupted consignment MUST be rejected (got {why1})");

    // NEGATIVE 2: the genuine consignment presented against a bogus witness txid is rejected.
    let bogus = "0000000000000000000000000000000000000000000000000000000000000000".to_string();
    let (rej2, why2) = rejected(tokio::task::block_in_place(|| {
        receiver.validate_offchain_chain(&split.consignment, &[bogus])
    }));
    println!("RGB13 - [NEGATIVE 2] consignment vs bogus witness txid -> rejected={rej2} ({why2})");
    assert!(rej2, "a consignment validated against a witness txid that is not its own MUST be rejected (got {why2})");

    println!("RGB13 - SUCCESS: the off-chain receiver ACCEPTS a well-formed consignment for its coin but REJECTS a payload-tampered consignment and one presented against the wrong witness — consignment integrity holds over statechain.");
    Ok(())
}
