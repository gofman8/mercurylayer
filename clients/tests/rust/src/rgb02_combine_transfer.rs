//! E2E: **multi-input ("combine") off-chain RGB transition** — the realistic forest/DAG shape where a
//! transition combines several deposit coins and separates into recipient + change.
//!
//! Mercury is single-input by construction; this exercises the lifted restriction
//! (`get_unsigned_combine_psbt` / `get_partial_sig_request_for_colored_tx_multi` /
//! `new_backup_transaction_multi` via `create_colored_combine_tx`) with **two** inputs the SE co-signs
//! per input.
//!
//! Funding the two inputs sidesteps an rgb-lib stale-UTXO quirk on multi-coin deposits by carving them
//! from a CONFIRMED root statechain coin: deposit ROOT (full, proven), split ROOT -> two Mercury
//! deposit addresses (SE-aggregate, so SE-co-signable) = coins A (600) + B (400), then COMBINE A+B.
//!
//! Flow:
//!   1. Issue 1000; deposit ROOT (full 1000); register ROOT.
//!   2. Open two Mercury deposit addresses depA/depB; split ROOT -> [600@depA, 400@depB]; broadcast.
//!      update_coins -> A, B are confirmed statechain coins; register both; ROOT consumed.
//!   3. Combine A+B in one un-broadcast tx the SE co-signs PER input -> receiver 700 + change 300.
//!      (Asserts 2 inputs, 2 spendable outputs + 1 OP_RETURN.)
//!   4. Receiver validates the combine OFF-CHAIN; broadcast to exit; settle (recv 700, change 300).
//!
//! Run with RGB_E2E=2. Requires the regtest + Mercury (lockbox) stack.

use std::{env, fs, process::Command, str::FromStr, thread, time::Duration};

use anyhow::{anyhow, Result};
use electrum_client::ElectrumApi;
use mercury_rgb::RgbWallet;
use mercuryrustlib::{client_config::ClientConfig, CoinStatus};

use crate::{bitcoin_core, electrs};

const ELECTRUM_URL: &str = "127.0.0.1:50001";
const RGB_PROXY: &str = "rpc://127.0.0.1:3000/json-rpc";
const NETWORK: &str = "regtest";
const BLINDING: u64 = 51;
const ISSUED: u64 = 1000;
const A_AMT: u64 = 600;
const B_AMT: u64 = 400;
const RECV_AMT: u64 = 700;
const CHANGE_AMT: u64 = 300;
const COIN_SAT_ROOT: u32 = 90_000;
const SAT_A: u64 = 35_000;
const SAT_B: u64 = 30_000; // split fee = 90_000 - 65_000 = 25_000
const RECV_SAT: u64 = 35_000;
const CHANGE_SAT: u64 = 20_000; // combine fee = 65_000 - 55_000 = 10_000

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

/// Deposit (full) onto a statechain coin via the proven rgb01 path; returns the deposit address.
async fn deposit_coin<F>(cc: &ClientConfig, wallet_name: &str, size_sat: u32, fund: F) -> Result<String>
where
    F: FnOnce(&str) -> Result<String>,
{
    let wallet = mercuryrustlib::wallet::create_wallet(wallet_name, cc).await?;
    mercuryrustlib::sqlite_manager::insert_wallet(&cc.pool, &wallet).await?;
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    let token_id = crate::utils::handle_token_response(cc, &token).await?;
    let sc_address =
        mercuryrustlib::deposit::get_deposit_bitcoin_address(cc, &wallet.name, &token_id, size_sat).await?;
    let _txid = fund(&sc_address)?;
    wait_for_address(cc, &sc_address, size_sat).await?;
    let core = bitcoin_core::getnewaddress()?;
    let _ = bitcoin_core::generatetoaddress(cc.confirmation_target, &core)?;
    mercuryrustlib::coin_status::update_coins(cc, &wallet.name).await?;
    Ok(sc_address)
}

/// Open a Mercury deposit address WITHOUT funding it (the split tx will pay it). Returns the address.
async fn open_deposit_address(cc: &ClientConfig, wallet_name: &str, size_sat: u32) -> Result<String> {
    let wallet = mercuryrustlib::wallet::create_wallet(wallet_name, cc).await?;
    mercuryrustlib::sqlite_manager::insert_wallet(&cc.pool, &wallet).await?;
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    let token_id = crate::utils::handle_token_response(cc, &token).await?;
    Ok(mercuryrustlib::deposit::get_deposit_bitcoin_address(cc, &wallet.name, &token_id, size_sat).await?)
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

pub async fn execute() -> Result<()> {
    let _ = Command::new("rm").arg("wallet.db").arg("wallet.db-shm").arg("wallet.db-wal").output();
    let _ = fs::remove_dir_all("./rgb-data2");
    env::set_var("ML_NETWORK", "regtest");
    let cc = mercuryrustlib::client_config::load().await;

    let (mut issuer, contract) = tokio::task::block_in_place(|| setup("./rgb-data2/issuer", true, true))?;
    let contract = contract.unwrap();
    let (mut receiver, _) = tokio::task::block_in_place(|| setup("./rgb-data2/receiver", false, false))?;
    println!("RGB02 - issued {ISSUED} units of {contract}");

    // ---- 1. Deposit the whole 1000 onto ROOT (full deposit = proven path); register it. ----
    let sources: Vec<String> = tokio::task::block_in_place(|| issuer.list_allocations(&contract))?
        .into_iter().map(|(op, _, _)| op).collect();
    let addr_root = deposit_coin(&cc, "rgb02_root", COIN_SAT_ROOT, |sc| {
        let (_t, _v, _c, signed) = tokio::task::block_in_place(|| {
            issuer.fund_statechain(sc, COIN_SAT_ROOT as u64, &contract, ISSUED, 2, BLINDING)
        })?;
        Ok(cc.electrum_client.transaction_broadcast_raw(&hex::decode(&signed)?)?.to_string())
    }).await?;
    let (txid_root, vout_root, mut coin_root) = coin_outpoint(&cc, "rgb02_root", &addr_root).await?;
    tokio::task::block_in_place(|| {
        issuer.register_statechain(&txid_root, vout_root, COIN_SAT_ROOT as u64, &contract, ISSUED, &sources)
    })?;
    println!("RGB02 - ROOT = {txid_root}:{vout_root} holds {}",
        tokio::task::block_in_place(|| issuer.settled_balance(&contract))?);

    // ---- 2. Split ROOT -> two Mercury deposit addresses = SE-co-signable coins A (600) + B (400). ----
    let dep_a = open_deposit_address(&cc, "rgb02_a", SAT_A as u32).await?;
    let dep_b = open_deposit_address(&cc, "rgb02_b", SAT_B as u32).await?;
    let si = mercuryrustlib::utils::info_config(&cc).await?;
    let splits = vec![(dep_a.clone(), SAT_A, A_AMT), (dep_b.clone(), SAT_B, B_AMT)];
    let split = mercuryrustlib::rgb::create_colored_split_tx(
        &cc, &issuer, &mut coin_root, &contract, &splits, 1, true, None, NETWORK,
        si.initlock, si.interval, BLINDING,
    ).await?;
    let _ = cc.electrum_client.transaction_broadcast_raw(&hex::decode(&split.signed_tx)?)?;
    println!("RGB02 - split ROOT -> A(600)@{dep_a} + B(400)@{dep_b} (tx {})", split.txid);
    wait_for_address(&cc, &dep_a, SAT_A as u32).await?;
    wait_for_address(&cc, &dep_b, SAT_B as u32).await?;
    let core = bitcoin_core::getnewaddress()?;
    let _ = bitcoin_core::generatetoaddress(cc.confirmation_target, &core)?;
    mercuryrustlib::coin_status::update_coins(&cc, "rgb02_a").await?;
    mercuryrustlib::coin_status::update_coins(&cc, "rgb02_b").await?;

    let (txid_a, vout_a, coin_a) = coin_outpoint(&cc, "rgb02_a", &dep_a).await?;
    let (txid_b, vout_b, coin_b) = coin_outpoint(&cc, "rgb02_b", &dep_b).await?;
    // ROOT is consumed by the split; register A/B (A marks ROOT spent so balance stays exact).
    tokio::task::block_in_place(|| {
        issuer.register_statechain(&txid_a, vout_a, SAT_A, &contract, A_AMT, &[format!("{txid_root}:{vout_root}")])
    })?;
    tokio::task::block_in_place(|| {
        issuer.register_statechain(&txid_b, vout_b, SAT_B, &contract, B_AMT, &[])
    })?;
    let held = |allocs: &Vec<(String, u64, bool)>, t: &str, v: u32| {
        allocs.iter().find(|(op, _, s)| op == &format!("{t}:{v}") && *s).map(|(_, a, _)| *a).unwrap_or(0)
    };
    let allocs = tokio::task::block_in_place(|| issuer.list_allocations(&contract))?;
    println!("RGB02 - input coins: A={} ({txid_a}:{vout_a}), B={} ({txid_b}:{vout_b})",
        held(&allocs, &txid_a, vout_a), held(&allocs, &txid_b, vout_b));
    assert_eq!(held(&allocs, &txid_a, vout_a), A_AMT, "coin A must hold {A_AMT}");
    assert_eq!(held(&allocs, &txid_b, vout_b), B_AMT, "coin B must hold {B_AMT}");

    // ---- 3. Combine A+B in ONE un-broadcast multi-input tx (SE co-signs each input). ----
    let recv_id = tokio::task::block_in_place(|| receiver.witness_receive(RECV_AMT))?;
    let recv_addr = tokio::task::block_in_place(|| receiver.address_from_recipient_id(&recv_id))?;
    let change_id = tokio::task::block_in_place(|| issuer.witness_receive(CHANGE_AMT))?;
    let change_addr = tokio::task::block_in_place(|| issuer.address_from_recipient_id(&change_id))?;
    let outputs = vec![(recv_addr.clone(), RECV_SAT, RECV_AMT), (change_addr.clone(), CHANGE_SAT, CHANGE_AMT)];
    let mut inputs = vec![coin_a.clone(), coin_b.clone()];
    let combine = mercuryrustlib::rgb::create_colored_combine_tx(
        &cc, &issuer, &mut inputs, &contract, &outputs, 1, true, None, NETWORK,
        si.initlock, si.interval, BLINDING,
    ).await?;
    println!("RGB02 - built combine tx {} (NOT broadcast); inputs={:?} output vouts={:?}",
        combine.txid, combine.input_outpoints, combine.output_vouts);

    let raw = hex::decode(&combine.signed_tx)?;
    let ctx: electrum_client::bitcoin::Transaction = electrum_client::bitcoin::consensus::deserialize(&raw)?;
    let op_returns = ctx.output.iter().filter(|o| o.script_pubkey.is_op_return()).count();
    assert_eq!(ctx.input.len(), 2, "combine spends exactly the two input coins");
    assert_eq!(ctx.output.len() - op_returns, 2, "combine must have two spendable outputs");
    assert_eq!(op_returns, 1, "exactly one OP_RETURN opret commitment");
    assert!(!is_outpoint_spent(&cc, &txid_a, vout_a), "off-chain: A still UNSPENT");
    assert!(!is_outpoint_spent(&cc, &txid_b, vout_b), "off-chain: B still UNSPENT");

    // ---- 4. Validate off-chain, then exit by broadcasting; settle outputs. ----
    let (valid, detail) = tokio::task::block_in_place(|| {
        receiver.validate_offchain_chain(&combine.consignment, &[combine.txid.clone()])
    })?;
    println!("RGB02 - [receiver] validate_offchain_chain -> valid={valid} detail={:?}", detail);
    assert!(valid, "receiver must validate the 2-input combine off-chain");

    tokio::task::block_in_place(|| {
        receiver.post_consignment(&recv_id, &combine.consignment, &combine.txid, combine.output_vouts[0])?;
        issuer.post_consignment(&change_id, &combine.consignment, &combine.txid, combine.output_vouts[1])
    })?;
    let _ = cc.electrum_client.transaction_broadcast_raw(&hex::decode(&combine.signed_tx)?)?;
    tokio::task::block_in_place(|| issuer.mark_spent(&[format!("{txid_a}:{vout_a}"), format!("{txid_b}:{vout_b}")]))?;
    let mut recv_bal = 0;
    let mut chg_bal = 0;
    for _ in 0..12 {
        let core = bitcoin_core::getnewaddress()?;
        let _ = bitcoin_core::generatetoaddress(cc.confirmation_target.max(1), &core)?;
        tokio::task::block_in_place(|| { let _ = issuer.refresh(None); let _ = receiver.refresh(None); });
        recv_bal = tokio::task::block_in_place(|| receiver.settled_balance(&contract)).unwrap_or(0);
        chg_bal = tokio::task::block_in_place(|| issuer.settled_balance(&contract)).unwrap_or(0);
        if recv_bal == RECV_AMT && chg_bal == CHANGE_AMT { break; }
        thread::sleep(Duration::from_secs(1));
    }

    crate::rgb_dump::dump("receiver after combine (got 700)", &mut receiver, &contract);
    crate::rgb_dump::dump("issuer after combine (change 300, A+B consumed)", &mut issuer, &contract);

    assert_eq!(recv_bal, RECV_AMT, "receiver must hold {RECV_AMT} after the combine");
    assert_eq!(chg_bal, CHANGE_AMT, "issuer change must be {CHANGE_AMT} after the combine");
    assert!(is_outpoint_spent(&cc, &txid_a, vout_a), "input A consumed by the combine");
    assert!(is_outpoint_spent(&cc, &txid_b, vout_b), "input B consumed by the combine");

    println!("RGB02 - SUCCESS: combined TWO statechain coins (600+400) in one SE-co-signed multi-input tx -> 700 to the receiver + 300 change, validated off-chain, exited on-chain. The 'combine + separate' DAG shape works.");
    Ok(())
}
