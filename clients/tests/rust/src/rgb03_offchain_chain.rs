//! E2E: **2-deep off-chain chain** — off-chain DAG *depth*. A split and a combine are chained with
//! NEITHER broadcast, and the leaf is validated against BOTH un-broadcast witnesses via the
//! `validate_offchain_chain` resolver enabler.
//!
//! ROOT (a confirmed statechain coin) is split into A(600)+B(400) WITHOUT broadcasting; A and B are
//! then spent (still un-broadcast) by an un-broadcast combine -> recipient 700 + change 300. The SE
//! co-signs the combine's inputs even though A/B's funding tx (the split) is not on chain — the
//! single-use guarantee is the SE's, not Bitcoin's. The receiver validates the combine consignment
//! off-chain against the chain [split, combine] (two un-broadcast witnesses). Exit broadcasts the
//! branch (split then combine).
//!
//! Run with RGB_E2E=3. Requires the regtest + Mercury (lockbox) stack.

use std::{env, fs, process::Command, str::FromStr, thread, time::Duration};

use anyhow::{anyhow, Result};
use electrum_client::ElectrumApi;
use mercury_rgb::RgbWallet;
use mercuryrustlib::{client_config::ClientConfig, Coin, CoinStatus};

use crate::{bitcoin_core, electrs};

const ELECTRUM_URL: &str = "127.0.0.1:50001";
const RGB_PROXY: &str = "rpc://127.0.0.1:3000/json-rpc";
const NETWORK: &str = "regtest";
const BLINDING: u64 = 61;
const ISSUED: u64 = 1000;
const A_AMT: u64 = 600;
const B_AMT: u64 = 400;
const RECV_AMT: u64 = 700;
const CHANGE_AMT: u64 = 300;
const COIN_SAT_ROOT: u32 = 90_000;
const SAT_A: u64 = 35_000;
const SAT_B: u64 = 30_000;
const RECV_SAT: u64 = 35_000;
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

fn tx_exists(cc: &ClientConfig, txid: &str) -> bool {
    use electrum_client::bitcoin::Txid;
    cc.electrum_client.transaction_get_raw(&Txid::from_str(txid).unwrap()).is_ok()
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
    let sc_address =
        mercuryrustlib::deposit::get_deposit_bitcoin_address(cc, &wallet.name, &token_id, size_sat).await?;
    let _txid = fund(&sc_address)?;
    wait_for_address(cc, &sc_address, size_sat).await?;
    let core = bitcoin_core::getnewaddress()?;
    let _ = bitcoin_core::generatetoaddress(cc.confirmation_target, &core)?;
    mercuryrustlib::coin_status::update_coins(cc, &wallet.name).await?;
    Ok(sc_address)
}

async fn open_deposit_address(cc: &ClientConfig, wallet_name: &str, size_sat: u32) -> Result<String> {
    let wallet = mercuryrustlib::wallet::create_wallet(wallet_name, cc).await?;
    mercuryrustlib::sqlite_manager::insert_wallet(&cc.pool, &wallet).await?;
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    let token_id = crate::utils::handle_token_response(cc, &token).await?;
    Ok(mercuryrustlib::deposit::get_deposit_bitcoin_address(cc, &wallet.name, &token_id, size_sat).await?)
}

async fn coin_outpoint(cc: &ClientConfig, wallet_name: &str, sc_address: &str) -> Result<(String, u32, Coin)> {
    let coin = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, wallet_name)
        .await?
        .coins.iter()
        .find(|c| c.aggregated_address.as_deref() == Some(sc_address))
        .ok_or(anyhow!("coin not found for {sc_address}"))?.clone();
    assert!(coin.status == CoinStatus::CONFIRMED, "statechain coin must confirm");
    let txid = coin.utxo_txid.clone().ok_or(anyhow!("no utxo_txid"))?;
    let vout = coin.utxo_vout.ok_or(anyhow!("no utxo_vout"))?;
    Ok((txid, vout, coin))
}

/// Load the deposit coin record (keys + statechain_id + signed_statechain_id are already set at
/// deposit-init) and PATCH its outpoint/amount to a still-UN-BROADCAST output, so it can be spent
/// off-chain. The SE co-signs by statechain_id auth (no on-chain check), so an un-broadcast funding
/// tx is fine — this is the off-chain single-use model.
async fn unbroadcast_subcoin(cc: &ClientConfig, wallet_name: &str, dep_addr: &str, txid: &str, vout: u32, sat: u64) -> Result<Coin> {
    let mut coin = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, wallet_name)
        .await?
        .coins.iter()
        .find(|c| c.aggregated_address.as_deref() == Some(dep_addr))
        .ok_or(anyhow!("deposit coin not found for {dep_addr}"))?.clone();
    coin.utxo_txid = Some(txid.to_string());
    coin.utxo_vout = Some(vout);
    coin.amount = Some(sat as u32);
    coin.status = CoinStatus::CONFIRMED;
    Ok(coin)
}

pub async fn execute() -> Result<()> {
    let _ = Command::new("rm").arg("wallet.db").arg("wallet.db-shm").arg("wallet.db-wal").output();
    let _ = fs::remove_dir_all("./rgb-data3");
    env::set_var("ML_NETWORK", "regtest");
    let cc = mercuryrustlib::client_config::load().await;

    let (mut issuer, contract) = tokio::task::block_in_place(|| setup("./rgb-data3/issuer", true, true))?;
    let contract = contract.unwrap();
    let (mut receiver, _) = tokio::task::block_in_place(|| setup("./rgb-data3/receiver", false, false))?;
    println!("RGB03 - issued {ISSUED} units of {contract}");

    // ---- 1. Deposit ROOT (full 1000); register it. ----
    let sources: Vec<String> = tokio::task::block_in_place(|| issuer.list_allocations(&contract))?
        .into_iter().map(|(op, _, _)| op).collect();
    let addr_root = deposit_coin(&cc, "rgb03_root", COIN_SAT_ROOT, |sc| {
        let (_t, _v, _c, signed) = tokio::task::block_in_place(|| {
            issuer.fund_statechain(sc, COIN_SAT_ROOT as u64, &contract, ISSUED, 2, BLINDING)
        })?;
        Ok(cc.electrum_client.transaction_broadcast_raw(&hex::decode(&signed)?)?.to_string())
    }).await?;
    let (txid_root, vout_root, mut coin_root) = coin_outpoint(&cc, "rgb03_root", &addr_root).await?;
    tokio::task::block_in_place(|| {
        issuer.register_statechain(&txid_root, vout_root, COIN_SAT_ROOT as u64, &contract, ISSUED, &sources)
    })?;
    println!("RGB03 - ROOT = {txid_root}:{vout_root} holds {}",
        tokio::task::block_in_place(|| issuer.settled_balance(&contract))?);

    // ---- 2. Split ROOT -> A(600)+B(400) at two deposit addresses. NOT broadcast. ----
    let dep_a = open_deposit_address(&cc, "rgb03_a", SAT_A as u32).await?;
    let dep_b = open_deposit_address(&cc, "rgb03_b", SAT_B as u32).await?;
    let si = mercuryrustlib::utils::info_config(&cc).await?;
    let splits = vec![(dep_a.clone(), SAT_A, A_AMT), (dep_b.clone(), SAT_B, B_AMT)];
    let split = mercuryrustlib::rgb::create_colored_split_tx(
        &cc, &issuer, &mut coin_root, &contract, &splits, 1, true, None, NETWORK,
        si.initlock, si.interval, BLINDING,
    ).await?;
    let a_vout = split.output_vouts[0];
    let b_vout = split.output_vouts[1];
    println!("RGB03 - split (UN-broadcast) tx {} -> A@{}:{a_vout} B@{}:{b_vout}", split.txid, split.txid, split.txid);
    assert!(!is_outpoint_spent(&cc, &txid_root, vout_root), "ROOT still UNSPENT (split not broadcast)");
    assert!(!tx_exists(&cc, &split.txid), "split tx must NOT be on chain yet");

    // Register A, B at their UN-broadcast outpoints (allocations are in the stash from the split's color).
    tokio::task::block_in_place(|| {
        issuer.register_statechain(&split.txid, a_vout, SAT_A, &contract, A_AMT, &[format!("{txid_root}:{vout_root}")])
    })?;
    tokio::task::block_in_place(|| {
        issuer.register_statechain(&split.txid, b_vout, SAT_B, &contract, B_AMT, &[])
    })?;

    // ---- 3. Combine the UN-broadcast A,B -> recv 700 + change 300. NOT broadcast. ----
    let coin_a = unbroadcast_subcoin(&cc, "rgb03_a", &dep_a, &split.txid, a_vout, SAT_A).await?;
    let coin_b = unbroadcast_subcoin(&cc, "rgb03_b", &dep_b, &split.txid, b_vout, SAT_B).await?;
    let recv_id = tokio::task::block_in_place(|| receiver.witness_receive(RECV_AMT))?;
    let recv_addr = tokio::task::block_in_place(|| receiver.address_from_recipient_id(&recv_id))?;
    let change_id = tokio::task::block_in_place(|| issuer.witness_receive(CHANGE_AMT))?;
    let change_addr = tokio::task::block_in_place(|| issuer.address_from_recipient_id(&change_id))?;
    let outputs = vec![(recv_addr.clone(), RECV_SAT, RECV_AMT), (change_addr.clone(), CHANGE_SAT, CHANGE_AMT)];
    let mut inputs = vec![coin_a, coin_b];
    let combine = mercuryrustlib::rgb::create_colored_combine_tx(
        &cc, &issuer, &mut inputs, &contract, &outputs, 1, true, None, NETWORK,
        si.initlock, si.interval, BLINDING,
    ).await?;
    println!("RGB03 - combine (UN-broadcast) tx {} spends [{}:{a_vout}, {}:{b_vout}]", combine.txid, split.txid, split.txid);
    assert!(!tx_exists(&cc, &split.txid) && !tx_exists(&cc, &combine.txid),
        "BOTH split and combine must be off-chain (un-broadcast) at validation time");

    // ---- 4. KEY PROOF: validate the leaf against the CHAIN of two un-broadcast witnesses. ----
    let (valid, detail) = tokio::task::block_in_place(|| {
        receiver.validate_offchain_chain(&combine.consignment, &[split.txid.clone(), combine.txid.clone()])
    })?;
    println!("RGB03 - [receiver] validate_offchain_chain([split, combine]) -> valid={valid} detail={:?}", detail);
    assert!(valid, "receiver must validate the 2-deep off-chain chain (split + combine both un-broadcast)");

    // ---- 5. Exit: broadcast the branch (split, then combine); settle. ----
    let _ = cc.electrum_client.transaction_broadcast_raw(&hex::decode(&split.signed_tx)?)?;
    let core = bitcoin_core::getnewaddress()?;
    let _ = bitcoin_core::generatetoaddress(cc.confirmation_target, &core)?;
    tokio::task::block_in_place(|| {
        receiver.post_consignment(&recv_id, &combine.consignment, &combine.txid, combine.output_vouts[0])?;
        issuer.post_consignment(&change_id, &combine.consignment, &combine.txid, combine.output_vouts[1])
    })?;
    let _ = cc.electrum_client.transaction_broadcast_raw(&hex::decode(&combine.signed_tx)?)?;
    tokio::task::block_in_place(|| issuer.mark_spent(&[
        format!("{txid_root}:{vout_root}"), format!("{}:{a_vout}", split.txid), format!("{}:{b_vout}", split.txid),
    ]))?;
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

    crate::rgb_dump::dump("receiver after chain exit (got 700)", &mut receiver, &contract);
    assert_eq!(recv_bal, RECV_AMT, "receiver must hold {RECV_AMT} after the chain exit");
    assert_eq!(chg_bal, CHANGE_AMT, "issuer change must be {CHANGE_AMT} after the chain exit");
    assert!(is_outpoint_spent(&cc, &txid_root, vout_root), "ROOT consumed by the branch exit");

    println!("RGB03 - SUCCESS: 2-deep OFF-CHAIN chain (split -> combine, both un-broadcast) validated against [split, combine] via validate_offchain_chain, then exited by broadcasting the branch. Off-chain DAG depth works.");
    Ok(())
}
