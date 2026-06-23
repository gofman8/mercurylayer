//! E2E (scale): **wide combine** — the "hundreds of deposits amortized into one on-chain tx" case.
//! ROOT is split into N=6 SE-co-signable sub-coins (each 200 units, single-use + epoch-bounded), then
//! ALL SIX are combined in ONE SE-co-signed (per-input) tx -> recipient 1000 + change 200, validated
//! off-chain, then exited in a SINGLE on-chain transaction. N deposits, one exit footprint: the
//! on-chain cost is constant regardless of how many coins are combined.
//!
//! Run with RGB_E2E=8. Requires the regtest + Mercury (lockbox) stack.

use std::{env, fs, process::Command, str::FromStr, thread, time::{Duration, SystemTime, UNIX_EPOCH}};

use anyhow::{anyhow, Result};
use electrum_client::ElectrumApi;
use mercury_rgb::RgbWallet;
use mercuryrustlib::{client_config::ClientConfig, Coin, CoinStatus};

use crate::{bitcoin_core, electrs};

const ELECTRUM_URL: &str = "127.0.0.1:50001";
const RGB_PROXY: &str = "rpc://127.0.0.1:3000/json-rpc";
const NETWORK: &str = "regtest";
const BLINDING: u64 = 88;
const N: usize = 6;
const PIECE_AMT: u64 = 200;        // asset per sub-coin
const ISSUED: u64 = 1200;          // N * PIECE_AMT
const RECV_AMT: u64 = 1000;
const CHANGE_AMT: u64 = 200;
const COIN_SAT_ROOT: u32 = 150_000;
const SAT_PIECE: u64 = 20_000;     // N * SAT_PIECE = 120_000; split fee = 30_000
const RECV_SAT: u64 = 70_000;
const CHANGE_SAT: u64 = 30_000;    // combine fee = 120_000 - 100_000 = 20_000
const EPOCH_FAR: u64 = 3600;       // far enough that the whole flow completes inside the active period

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

/// Deposit a single-use + epoch-bounded coin funded by `fund`, and confirm it.
async fn deposit_coin<F>(cc: &ClientConfig, wallet_name: &str, size_sat: u32, fund: F) -> Result<String>
where
    F: FnOnce(&str) -> Result<String>,
{
    let wallet = mercuryrustlib::wallet::create_wallet(wallet_name, cc).await?;
    mercuryrustlib::sqlite_manager::insert_wallet(&cc.pool, &wallet).await?;
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    let token_id = crate::utils::handle_token_response(cc, &token).await?;
    let sc_address = mercuryrustlib::deposit::get_deposit_bitcoin_address_single_use_epoch(
        cc, &wallet.name, &token_id, size_sat, now_unix() + EPOCH_FAR,
    ).await?;
    let _txid = fund(&sc_address)?;
    wait_for_address(cc, &sc_address, size_sat).await?;
    let core = bitcoin_core::getnewaddress()?;
    let _ = bitcoin_core::generatetoaddress(cc.confirmation_target, &core)?;
    mercuryrustlib::coin_status::update_coins(cc, &wallet.name).await?;
    Ok(sc_address)
}

/// Open a single-use + epoch-bounded deposit address (no funding yet — the split funds it).
async fn open_deposit_address(cc: &ClientConfig, wallet_name: &str, size_sat: u32) -> Result<String> {
    let wallet = mercuryrustlib::wallet::create_wallet(wallet_name, cc).await?;
    mercuryrustlib::sqlite_manager::insert_wallet(&cc.pool, &wallet).await?;
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    let token_id = crate::utils::handle_token_response(cc, &token).await?;
    Ok(mercuryrustlib::deposit::get_deposit_bitcoin_address_single_use_epoch(
        cc, &wallet.name, &token_id, size_sat, now_unix() + EPOCH_FAR,
    ).await?)
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

pub async fn execute() -> Result<()> {
    let _ = Command::new("rm").arg("wallet.db").arg("wallet.db-shm").arg("wallet.db-wal").output();
    let _ = fs::remove_dir_all("./rgb-data8");
    env::set_var("ML_NETWORK", "regtest");
    let cc = mercuryrustlib::client_config::load().await;

    let (mut issuer, contract) = tokio::task::block_in_place(|| setup("./rgb-data8/issuer", true, true))?;
    let contract = contract.unwrap();
    let (mut receiver, _) = tokio::task::block_in_place(|| setup("./rgb-data8/receiver", false, false))?;
    println!("RGB08 - issued {ISSUED} units of {contract}");

    // Deposit ROOT (full ISSUED); register.
    let sources: Vec<String> = tokio::task::block_in_place(|| issuer.list_allocations(&contract))?
        .into_iter().map(|(op, _, _)| op).collect();
    let addr_root = deposit_coin(&cc, "rgb08_root", COIN_SAT_ROOT, |sc| {
        let (_t, _v, _c, signed) = tokio::task::block_in_place(|| {
            issuer.fund_statechain(sc, COIN_SAT_ROOT as u64, &contract, ISSUED, 2, BLINDING)
        })?;
        Ok(cc.electrum_client.transaction_broadcast_raw(&hex::decode(&signed)?)?.to_string())
    }).await?;
    let (txid_root, vout_root, mut coin_root) = coin_outpoint(&cc, "rgb08_root", &addr_root).await?;
    tokio::task::block_in_place(|| {
        issuer.register_statechain(&txid_root, vout_root, COIN_SAT_ROOT as u64, &contract, ISSUED, &sources)
    })?;
    println!("RGB08 - ROOT {txid_root}:{vout_root} holds {ISSUED}");

    // ---- Wide split: ROOT -> N sub-coins (PIECE_AMT each) at N deposit addresses; broadcast. ----
    let names: Vec<String> = (0..N).map(|i| format!("rgb08_p{i}")).collect();
    let mut deps: Vec<String> = Vec::with_capacity(N);
    for name in &names {
        deps.push(open_deposit_address(&cc, name, SAT_PIECE as u32).await?);
    }
    let si = mercuryrustlib::utils::info_config(&cc).await?;
    let splits: Vec<(String, u64, u64)> = deps.iter().map(|d| (d.clone(), SAT_PIECE, PIECE_AMT)).collect();
    let split = mercuryrustlib::rgb::create_colored_split_tx(
        &cc, &issuer, &mut coin_root, &contract, &splits, 1, true, None, NETWORK, si.initlock, si.interval, BLINDING,
    ).await?;
    let _ = cc.electrum_client.transaction_broadcast_raw(&hex::decode(&split.signed_tx)?)?;
    println!("RGB08 - split ROOT -> {N} x {PIECE_AMT} sub-coins (tx {})", split.txid);
    for d in &deps { wait_for_address(&cc, d, 0).await.ok(); }
    let core = bitcoin_core::getnewaddress()?;
    let _ = bitcoin_core::generatetoaddress(cc.confirmation_target, &core)?;
    for name in &names { mercuryrustlib::coin_status::update_coins(&cc, name).await?; }

    // Register each sub-coin as an input allocation.
    let mut inputs: Vec<Coin> = Vec::with_capacity(N);
    let mut outpoints: Vec<(String, u32)> = Vec::with_capacity(N);
    for (i, name) in names.iter().enumerate() {
        let (txid, vout, coin) = coin_outpoint(&cc, name, &deps[i]).await?;
        let src = if i == 0 { vec![format!("{txid_root}:{vout_root}")] } else { vec![] };
        tokio::task::block_in_place(|| issuer.register_statechain(&txid, vout, SAT_PIECE, &contract, PIECE_AMT, &src))?;
        outpoints.push((txid, vout));
        inputs.push(coin);
    }
    println!("RGB08 - {N} input sub-coins registered (each single-use + epoch-bounded)");

    // ---- Wide combine: N sub-coins -> recv RECV_AMT + change CHANGE_AMT in ONE SE-co-signed tx. ----
    let recv_id = tokio::task::block_in_place(|| receiver.witness_receive(RECV_AMT))?;
    let recv_addr = tokio::task::block_in_place(|| receiver.address_from_recipient_id(&recv_id))?;
    let change_id = tokio::task::block_in_place(|| issuer.witness_receive(CHANGE_AMT))?;
    let change_addr = tokio::task::block_in_place(|| issuer.address_from_recipient_id(&change_id))?;
    let outputs = vec![(recv_addr, RECV_SAT, RECV_AMT), (change_addr, CHANGE_SAT, CHANGE_AMT)];
    let combine = mercuryrustlib::rgb::create_colored_combine_tx(
        &cc, &issuer, &mut inputs, &contract, &outputs, 1, true, None, NETWORK, si.initlock, si.interval, BLINDING,
    ).await?;
    println!("RGB08 - built {}-input combine tx {} (one tx, one on-chain footprint)", combine.input_outpoints.len(), combine.txid);
    let raw = hex::decode(&combine.signed_tx)?;
    let ctx: electrum_client::bitcoin::Transaction = electrum_client::bitcoin::consensus::deserialize(&raw)?;
    assert_eq!(ctx.input.len(), N, "combine must spend all {N} input coins");

    let (valid, _d) = tokio::task::block_in_place(|| receiver.validate_offchain_chain(&combine.consignment, &[combine.txid.clone()]))?;
    assert!(valid, "receiver must validate the {N}-input combine off-chain");

    tokio::task::block_in_place(|| {
        receiver.post_consignment(&recv_id, &combine.consignment, &combine.txid, combine.output_vouts[0])?;
        issuer.post_consignment(&change_id, &combine.consignment, &combine.txid, combine.output_vouts[1])
    })?;
    // Exit: ONE on-chain transaction for all N deposits.
    let _ = cc.electrum_client.transaction_broadcast_raw(&hex::decode(&combine.signed_tx)?)?;
    let spent_ops: Vec<String> = outpoints.iter().map(|(t, v)| format!("{t}:{v}")).collect();
    tokio::task::block_in_place(|| issuer.mark_spent(&spent_ops))?;

    let mut recv_bal = 0; let mut chg_bal = 0;
    for _ in 0..12 {
        let core = bitcoin_core::getnewaddress()?;
        let _ = bitcoin_core::generatetoaddress(cc.confirmation_target.max(1), &core)?;
        tokio::task::block_in_place(|| { let _ = issuer.refresh(None); let _ = receiver.refresh(None); });
        recv_bal = tokio::task::block_in_place(|| receiver.settled_balance(&contract)).unwrap_or(0);
        chg_bal = tokio::task::block_in_place(|| issuer.settled_balance(&contract)).unwrap_or(0);
        if recv_bal == RECV_AMT && chg_bal == CHANGE_AMT { break; }
        thread::sleep(Duration::from_secs(1));
    }
    assert_eq!(recv_bal, RECV_AMT, "receiver must hold {RECV_AMT} after the {N}-input combine");
    assert_eq!(chg_bal, CHANGE_AMT, "issuer change must be {CHANGE_AMT}");
    for (t, v) in &outpoints {
        assert!(is_outpoint_spent(&cc, t, *v), "each input coin must be consumed by the combine");
    }

    println!("RGB08 - SUCCESS: split ROOT into {N} single-use + epoch-bounded sub-coins, then combined ALL {N} in one SE-co-signed multi-input tx -> {RECV_AMT} to the receiver + {CHANGE_AMT} change, validated off-chain, exited in ONE on-chain tx. {N} deposits, one exit footprint - the on-chain cost is constant regardless of deposit count.");
    Ok(())
}
