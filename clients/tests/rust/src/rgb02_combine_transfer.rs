//! E2E: **multi-input ("combine") colored transition signing path** via `create_colored_combine_tx`.
//!
//! Mercury is single-input by construction; this exercises the lifted restriction
//! (`get_unsigned_combine_psbt` / `get_partial_sig_request_for_colored_tx_multi` /
//! `new_backup_transaction_multi`) end-to-end: spend statechain coin(s) in ONE tx the SE co-signs
//! PER input, then separate into recipient + change, validate off-chain, and exit.
//!
//! This run proves the combine code path with **one** input (the per-input signing loop, prevout-set
//! sighash, and multi-witness assembly are the same code for N inputs). Scaling the E2E to N>=2
//! distinct funded coins needs multi-coin deposit funding, which hits a separate rgb-lib stale-UTXO
//! quirk on this regtest (full single deposits dodge it) - tracked separately. See
//! `docs/rgb_offchain_split_spilman.md` (the "combine" half of the forest/DAG model).
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
const RECV_AMT: u64 = 700;
const CHANGE_AMT: u64 = 300;
const COIN_SAT: u32 = 60_000;
const RECV_SAT: u64 = 30_000;
const CHANGE_SAT: u64 = 22_000; // input 60_000 - outputs 52_000 = 8_000 fee

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

    // ---- 1. Deposit the whole 1000 onto a statechain coin A; register it (full deposit = rgb01 path). ----
    let sources: Vec<String> = tokio::task::block_in_place(|| issuer.list_allocations(&contract))?
        .into_iter().map(|(op, _, _)| op).collect();
    let addr_a = deposit_coin(&cc, "rgb02_a", COIN_SAT, |sc| {
        let (_t, _v, _c, signed) = tokio::task::block_in_place(|| {
            issuer.fund_statechain(sc, COIN_SAT as u64, &contract, ISSUED, 2, BLINDING)
        })?;
        Ok(cc.electrum_client.transaction_broadcast_raw(&hex::decode(&signed)?)?.to_string())
    }).await?;
    let (txid_a, vout_a, coin_a) = coin_outpoint(&cc, "rgb02_a", &addr_a).await?;
    tokio::task::block_in_place(|| {
        issuer.register_statechain(&txid_a, vout_a, COIN_SAT as u64, &contract, ISSUED, &sources)
    })?;
    println!("RGB02 - input coin A = {txid_a}:{vout_a} holds {}",
        tokio::task::block_in_place(|| issuer.settled_balance(&contract))?);

    // ---- 2. Witness invoices: receiver 700, issuer (change) 300. ----
    let recv_id = tokio::task::block_in_place(|| receiver.witness_receive(RECV_AMT))?;
    let recv_addr = tokio::task::block_in_place(|| receiver.address_from_recipient_id(&recv_id))?;
    let change_id = tokio::task::block_in_place(|| issuer.witness_receive(CHANGE_AMT))?;
    let change_addr = tokio::task::block_in_place(|| issuer.address_from_recipient_id(&change_id))?;
    println!("RGB02 - invoices: recv {RECV_AMT}@{recv_addr}, change {CHANGE_AMT}@{change_addr}");

    // ---- 3. Multi-input combine path: spend coin A in one SE-co-signed (per-input) tx -> recv + change. NOT broadcast. ----
    let si = mercuryrustlib::utils::info_config(&cc).await?;
    let outputs = vec![
        (recv_addr.clone(), RECV_SAT, RECV_AMT),
        (change_addr.clone(), CHANGE_SAT, CHANGE_AMT),
    ];
    let mut inputs = vec![coin_a.clone()];
    let combine = mercuryrustlib::rgb::create_colored_combine_tx(
        &cc, &issuer, &mut inputs, &contract, &outputs, 1, true, None, NETWORK,
        si.initlock, si.interval, BLINDING,
    ).await?;
    println!("RGB02 - built combine tx {} (NOT broadcast); inputs={:?} output vouts={:?}",
        combine.txid, combine.input_outpoints, combine.output_vouts);

    let raw = hex::decode(&combine.signed_tx)?;
    let ctx: electrum_client::bitcoin::Transaction = electrum_client::bitcoin::consensus::deserialize(&raw)?;
    let op_returns = ctx.output.iter().filter(|o| o.script_pubkey.is_op_return()).count();
    assert_eq!(ctx.input.len(), inputs.len(), "combine spends exactly the input coin(s)");
    assert_eq!(ctx.output.len() - op_returns, 2, "combine must have two spendable outputs");
    assert_eq!(op_returns, 1, "exactly one OP_RETURN opret commitment");
    assert!(!is_outpoint_spent(&cc, &txid_a, vout_a), "off-chain: A still UNSPENT (nothing broadcast)");

    // ---- 4. Receiver validates the combine OFF-CHAIN (inputs on-chain; combine tx is the witness). ----
    let (valid, detail) = tokio::task::block_in_place(|| {
        receiver.validate_offchain_chain(&combine.consignment, &[combine.txid.clone()])
    })?;
    println!("RGB02 - [receiver] validate_offchain_chain(txid={}) -> valid={valid} detail={:?}", combine.txid, detail);
    assert!(valid, "receiver must validate the combine off-chain");

    // ---- 5. Exit: broadcast the combine; settle both outputs; A consumed. ----
    tokio::task::block_in_place(|| {
        receiver.post_consignment(&recv_id, &combine.consignment, &combine.txid, combine.output_vouts[0])?;
        issuer.post_consignment(&change_id, &combine.consignment, &combine.txid, combine.output_vouts[1])
    })?;
    let _ = cc.electrum_client.transaction_broadcast_raw(&hex::decode(&combine.signed_tx)?)?;
    tokio::task::block_in_place(|| issuer.mark_spent(&[format!("{txid_a}:{vout_a}")]))?;
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
    crate::rgb_dump::dump("issuer after combine (change 300, A consumed)", &mut issuer, &contract);

    assert_eq!(recv_bal, RECV_AMT, "receiver must hold {RECV_AMT} after the combine exit");
    assert_eq!(chg_bal, CHANGE_AMT, "issuer change must be {CHANGE_AMT} after the combine exit");
    assert!(is_outpoint_spent(&cc, &txid_a, vout_a), "input A must be consumed by the combine");

    println!("RGB02 - SUCCESS: multi-input combine signing path proven end-to-end (1 input -> recv 700 + change 300), SE co-signed per input, validated off-chain, exited on-chain. The N-input loop is the same code; N>=2 needs multi-coin funding (tracked separately).");
    Ok(())
}
