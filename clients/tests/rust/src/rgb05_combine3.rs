//! E2E: **3-input combine** — the multi-input combine scales beyond two (the "many deposits combine
//! into one payment" case). ROOT is split into A(500)+B(300)+C(200) at three SE-co-signable deposit
//! addresses, then all THREE are combined in one SE-co-signed (per-input) tx -> recipient 800 +
//! change 200, validated off-chain, then exited.
//!
//! Run with RGB_E2E=5. Requires the regtest + Mercury (lockbox) stack.

use std::{env, fs, process::Command, str::FromStr, thread, time::Duration};

use anyhow::{anyhow, Result};
use electrum_client::ElectrumApi;
use mercury_rgb::RgbWallet;
use mercuryrustlib::{client_config::ClientConfig, CoinStatus};

use crate::{bitcoin_core, electrs};

const ELECTRUM_URL: &str = "127.0.0.1:50001";
const RGB_PROXY: &str = "rpc://127.0.0.1:3000/json-rpc";
const NETWORK: &str = "regtest";
const BLINDING: u64 = 81;
const ISSUED: u64 = 1000;
const A_AMT: u64 = 500;
const B_AMT: u64 = 300;
const C_AMT: u64 = 200;
const RECV_AMT: u64 = 800;
const CHANGE_AMT: u64 = 200;
const COIN_SAT_ROOT: u32 = 130_000;
const SAT_A: u64 = 35_000;
const SAT_B: u64 = 30_000;
const SAT_C: u64 = 25_000; // split fee = 130_000 - 90_000 = 40_000
const RECV_SAT: u64 = 45_000;
const CHANGE_SAT: u64 = 30_000; // combine fee = 90_000 - 75_000 = 15_000

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

async fn coin_outpoint(cc: &ClientConfig, wallet_name: &str, sc_address: &str) -> Result<(String, u32, mercuryrustlib::Coin)> {
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
    let _ = fs::remove_dir_all("./rgb-data5");
    env::set_var("ML_NETWORK", "regtest");
    let cc = mercuryrustlib::client_config::load().await;

    let (mut issuer, contract) = tokio::task::block_in_place(|| setup("./rgb-data5/issuer", true, true))?;
    let contract = contract.unwrap();
    let (mut receiver, _) = tokio::task::block_in_place(|| setup("./rgb-data5/receiver", false, false))?;
    println!("RGB05 - issued {ISSUED} units of {contract}");

    // Deposit ROOT (full 1000); register.
    let sources: Vec<String> = tokio::task::block_in_place(|| issuer.list_allocations(&contract))?
        .into_iter().map(|(op, _, _)| op).collect();
    let addr_root = deposit_coin(&cc, "rgb05_root", COIN_SAT_ROOT, |sc| {
        let (_t, _v, _c, signed) = tokio::task::block_in_place(|| {
            issuer.fund_statechain(sc, COIN_SAT_ROOT as u64, &contract, ISSUED, 2, BLINDING)
        })?;
        Ok(cc.electrum_client.transaction_broadcast_raw(&hex::decode(&signed)?)?.to_string())
    }).await?;
    let (txid_root, vout_root, mut coin_root) = coin_outpoint(&cc, "rgb05_root", &addr_root).await?;
    tokio::task::block_in_place(|| {
        issuer.register_statechain(&txid_root, vout_root, COIN_SAT_ROOT as u64, &contract, ISSUED, &sources)
    })?;

    // Split ROOT -> A(500)+B(300)+C(200) at three deposit addresses; broadcast.
    let dep_a = open_deposit_address(&cc, "rgb05_a", SAT_A as u32).await?;
    let dep_b = open_deposit_address(&cc, "rgb05_b", SAT_B as u32).await?;
    let dep_c = open_deposit_address(&cc, "rgb05_c", SAT_C as u32).await?;
    let si = mercuryrustlib::utils::info_config(&cc).await?;
    let splits = vec![(dep_a.clone(), SAT_A, A_AMT), (dep_b.clone(), SAT_B, B_AMT), (dep_c.clone(), SAT_C, C_AMT)];
    let split = mercuryrustlib::rgb::create_colored_split_tx(
        &cc, &issuer, &mut coin_root, &contract, &splits, 1, true, None, NETWORK, si.initlock, si.interval, BLINDING,
    ).await?;
    let _ = cc.electrum_client.transaction_broadcast_raw(&hex::decode(&split.signed_tx)?)?;
    println!("RGB05 - split ROOT -> A(500)+B(300)+C(200) (tx {})", split.txid);
    for d in [&dep_a, &dep_b, &dep_c] { wait_for_address(&cc, d, 0).await.ok(); }
    let core = bitcoin_core::getnewaddress()?;
    let _ = bitcoin_core::generatetoaddress(cc.confirmation_target, &core)?;
    for w in ["rgb05_a", "rgb05_b", "rgb05_c"] { mercuryrustlib::coin_status::update_coins(&cc, w).await?; }

    let (txid_a, vout_a, coin_a) = coin_outpoint(&cc, "rgb05_a", &dep_a).await?;
    let (txid_b, vout_b, coin_b) = coin_outpoint(&cc, "rgb05_b", &dep_b).await?;
    let (txid_c, vout_c, coin_c) = coin_outpoint(&cc, "rgb05_c", &dep_c).await?;
    tokio::task::block_in_place(|| issuer.register_statechain(&txid_a, vout_a, SAT_A, &contract, A_AMT, &[format!("{txid_root}:{vout_root}")]))?;
    tokio::task::block_in_place(|| issuer.register_statechain(&txid_b, vout_b, SAT_B, &contract, B_AMT, &[]))?;
    tokio::task::block_in_place(|| issuer.register_statechain(&txid_c, vout_c, SAT_C, &contract, C_AMT, &[]))?;
    println!("RGB05 - input coins A(500) B(300) C(200) registered");

    // Combine A+B+C -> recv 800 + change 200, in one SE-co-signed (per-input) tx.
    let recv_id = tokio::task::block_in_place(|| receiver.witness_receive(RECV_AMT))?;
    let recv_addr = tokio::task::block_in_place(|| receiver.address_from_recipient_id(&recv_id))?;
    let change_id = tokio::task::block_in_place(|| issuer.witness_receive(CHANGE_AMT))?;
    let change_addr = tokio::task::block_in_place(|| issuer.address_from_recipient_id(&change_id))?;
    let outputs = vec![(recv_addr.clone(), RECV_SAT, RECV_AMT), (change_addr.clone(), CHANGE_SAT, CHANGE_AMT)];
    let mut inputs = vec![coin_a.clone(), coin_b.clone(), coin_c.clone()];
    let combine = mercuryrustlib::rgb::create_colored_combine_tx(
        &cc, &issuer, &mut inputs, &contract, &outputs, 1, true, None, NETWORK, si.initlock, si.interval, BLINDING,
    ).await?;
    println!("RGB05 - built 3-input combine tx {} ({} inputs)", combine.txid, combine.input_outpoints.len());
    let raw = hex::decode(&combine.signed_tx)?;
    let ctx: electrum_client::bitcoin::Transaction = electrum_client::bitcoin::consensus::deserialize(&raw)?;
    assert_eq!(ctx.input.len(), 3, "combine must spend all three input coins");

    let (valid, _d) = tokio::task::block_in_place(|| receiver.validate_offchain_chain(&combine.consignment, &[combine.txid.clone()]))?;
    assert!(valid, "receiver must validate the 3-input combine off-chain");

    tokio::task::block_in_place(|| {
        receiver.post_consignment(&recv_id, &combine.consignment, &combine.txid, combine.output_vouts[0])?;
        issuer.post_consignment(&change_id, &combine.consignment, &combine.txid, combine.output_vouts[1])
    })?;
    let _ = cc.electrum_client.transaction_broadcast_raw(&hex::decode(&combine.signed_tx)?)?;
    tokio::task::block_in_place(|| issuer.mark_spent(&[format!("{txid_a}:{vout_a}"), format!("{txid_b}:{vout_b}"), format!("{txid_c}:{vout_c}")]))?;
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
    assert_eq!(recv_bal, RECV_AMT, "receiver must hold {RECV_AMT} after the 3-input combine");
    assert_eq!(chg_bal, CHANGE_AMT, "issuer change must be {CHANGE_AMT}");
    for (t, v) in [(&txid_a, vout_a), (&txid_b, vout_b), (&txid_c, vout_c)] {
        assert!(is_outpoint_spent(&cc, t, v), "each input coin must be consumed by the combine");
    }

    println!("RGB05 - SUCCESS: combined THREE statechain coins (500+300+200) in one SE-co-signed multi-input tx -> 800 to the receiver + 200 change. Multi-input combine scales (many deposits -> one payment).");
    Ok(())
}
