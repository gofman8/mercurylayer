//! E2E: a **partial** statechain->statechain RGB transfer where the change goes back to a free
//! statechain UTXO - "each transfer consumes a UTXO, change returns to a free statechain UTXO, and
//! the sats move to the receiver via the statechain instead of to a miner".
//!
//! Sender holds 1000 on statechain UTXO A. Receiver blind-invoices 600 on its statechain UTXO B;
//! the sender also blind-invoices its own 400 change onto a free statechain UTXO C. The sender spends
//! A "to itself" with an OP_RETURN committing 600 -> B and 400 -> C (one transition, two blinded
//! beneficiaries via the new `blinded_map`). The receiver settles 600 onto B via standard `refresh`;
//! the sender settles its 400 change onto C; A is consumed. End state matches on-chain RGB exactly.
//!
//! Run with RGB_E2E=6. Requires the regtest + Mercury (lockbox) stack.

use std::{env, fs, process::Command, str::FromStr, thread, time::Duration};

use anyhow::{anyhow, Result};
use electrum_client::ElectrumApi;
use mercury_rgb::RgbWallet;
use mercuryrustlib::{client_config::ClientConfig, CoinStatus};

use crate::{bitcoin_core, electrs};

const ELECTRUM_URL: &str = "127.0.0.1:50001";
const RGB_PROXY: &str = "rpc://127.0.0.1:3000/json-rpc";
const NETWORK: &str = "regtest";
const BLINDING: u64 = 31;
const ISSUED: u64 = 1000;
const RECEIVE_AMT: u64 = 600;
const CHANGE_AMT: u64 = 400;
const COIN_SAT_A: u32 = 40_000;
const COIN_SAT_B: u32 = 30_000;
const COIN_SAT_C: u32 = 25_000;

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

/// Mercury-deposit a statechain coin to `wallet_name` of `size_sat`; return (address, wallet_name).
async fn deposit_coin<F>(cc: &ClientConfig, wallet_name: &str, size_sat: u32, fund: F) -> Result<String>
where
    F: FnOnce(&str) -> Result<String>,
{
    let wallet = mercuryrustlib::wallet::create_wallet(wallet_name, cc).await?;
    mercuryrustlib::sqlite_manager::insert_wallet(&cc.pool, &wallet).await?;
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    let token_id = crate::utils::handle_token_response(cc, &token).await?;
    let sc_address =
        mercuryrustlib::deposit::get_deposit_bitcoin_address(cc, &wallet.name, &token_id, size_sat)
            .await?;
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
    let _ = fs::remove_dir_all("./rgb-data6");
    env::set_var("ML_NETWORK", "regtest");
    let cc = mercuryrustlib::client_config::load().await;

    let (mut issuer, contract) = tokio::task::block_in_place(|| setup("./rgb-data6/issuer", true, true))?;
    let contract = contract.unwrap();
    let (mut receiver, _) = tokio::task::block_in_place(|| setup("./rgb-data6/receiver", false, false))?;
    println!("RGB06 - issued {ISSUED} units of {contract}");

    // Sender: deposit asset onto statechain UTXO A, register it.
    let sources: Vec<String> = tokio::task::block_in_place(|| issuer.list_allocations(&contract))?
        .into_iter().map(|(op, _, _)| op).collect();
    let addr_a = deposit_coin(&cc, "rgb06_sender", COIN_SAT_A, |sc| {
        let (_t, _v, _c, signed) = tokio::task::block_in_place(|| {
            issuer.fund_statechain(sc, COIN_SAT_A as u64, &contract, ISSUED, 2, BLINDING)
        })?;
        Ok(cc.electrum_client.transaction_broadcast_raw(&hex::decode(&signed)?)?.to_string())
    }).await?;
    let (txid_a, vout_a, mut coin_a) = coin_outpoint(&cc, "rgb06_sender", &addr_a).await?;
    tokio::task::block_in_place(|| {
        issuer.register_statechain(&txid_a, vout_a, COIN_SAT_A as u64, &contract, ISSUED, &sources)
    })?;
    println!("RGB06 - sender holds {} on statechain UTXO A = {txid_a}:{vout_a}",
        tokio::task::block_in_place(|| issuer.settled_balance(&contract))?);

    // Receiver: onboard free statechain UTXO B, register, blind-invoice RECEIVE_AMT on it.
    let addr_b = deposit_coin(&cc, "rgb06_receiver", COIN_SAT_B, |sc| {
        Ok(bitcoin_core::sendtoaddress(COIN_SAT_B, sc)?)
    }).await?;
    let (txid_b, vout_b, _cb) = coin_outpoint(&cc, "rgb06_receiver", &addr_b).await?;
    tokio::task::block_in_place(|| receiver.register_statechain(&txid_b, vout_b, COIN_SAT_B as u64, &contract, 0, &[]))?;
    let receiver_id = tokio::task::block_in_place(|| receiver.blind_receive(None, RECEIVE_AMT))?;

    // Sender: onboard free statechain UTXO C for change, register, blind-invoice CHANGE_AMT on it.
    let addr_c = deposit_coin(&cc, "rgb06_change", COIN_SAT_C, |sc| {
        Ok(bitcoin_core::sendtoaddress(COIN_SAT_C, sc)?)
    }).await?;
    let (txid_c, vout_c, _cc2) = coin_outpoint(&cc, "rgb06_change", &addr_c).await?;
    tokio::task::block_in_place(|| issuer.register_statechain(&txid_c, vout_c, COIN_SAT_C as u64, &contract, 0, &[]))?;
    let change_id = tokio::task::block_in_place(|| issuer.blind_receive(None, CHANGE_AMT))?;
    println!("RGB06 - receiver invoice {RECEIVE_AMT}@B={txid_b}:{vout_b}; change invoice {CHANGE_AMT}@C={txid_c}:{vout_c}");

    // Sender spends A "to itself" + OP_RETURN committing RECEIVE_AMT -> B and CHANGE_AMT -> C.
    let exit_address = tokio::task::block_in_place(|| issuer.get_address())?;
    let si = mercuryrustlib::utils::info_config(&cc).await?;
    let blinded = vec![(receiver_id.clone(), RECEIVE_AMT), (change_id.clone(), CHANGE_AMT)];
    let transfer = mercuryrustlib::rgb::create_colored_backup_tx(
        &cc, &issuer, &mut coin_a, &contract, ISSUED, &exit_address, 1, true, None, NETWORK,
        si.fee_rate_sats_per_byte, si.initlock, si.interval, BLINDING, Some(&blinded),
    ).await?;
    println!("RGB06 - sender built unilateral-exit tx {} (consumes A, change -> C)", transfer.txid);

    // Both parties fetch the (single) consignment under their own recipient_id and settle via refresh.
    tokio::task::block_in_place(|| {
        receiver.post_consignment(&receiver_id, &transfer.consignment, &transfer.txid, transfer.recipient_vout)?;
        issuer.post_consignment(&change_id, &transfer.consignment, &transfer.txid, transfer.recipient_vout)
    })?;
    let _ = cc.electrum_client.transaction_broadcast_raw(&hex::decode(&transfer.signed_tx)?)?;

    let mut recv_bal = 0;
    let mut chg_bal = 0;
    for _ in 0..12 {
        let core = bitcoin_core::getnewaddress()?;
        let _ = bitcoin_core::generatetoaddress(cc.confirmation_target.max(1), &core)?;
        tokio::task::block_in_place(|| { let _ = receiver.refresh(None); let _ = issuer.refresh(None); });
        recv_bal = tokio::task::block_in_place(|| receiver.settled_balance(&contract)).unwrap_or(0);
        chg_bal = tokio::task::block_in_place(|| issuer.list_allocations(&contract))
            .unwrap_or_default().into_iter()
            .find(|(op, _, settled)| op == &format!("{txid_c}:{vout_c}") && *settled)
            .map(|(_, amt, _)| amt).unwrap_or(0);
        if recv_bal == RECEIVE_AMT && chg_bal == CHANGE_AMT { break; }
        thread::sleep(Duration::from_secs(1));
    }

    // A is consumed; mark it spent so the sender's balance drops to just the change.
    tokio::task::block_in_place(|| issuer.mark_spent(&[format!("{txid_a}:{vout_a}")]))?;

    // -------- Proofs (standard rgb-lib methods). --------
    let recv_allocs = tokio::task::block_in_place(|| receiver.list_allocations(&contract))?;
    let send_allocs = tokio::task::block_in_place(|| issuer.list_allocations(&contract))?;
    for (op, amt, settled) in &recv_allocs { println!("RGB06 - [standard] receiver alloc: {op} amount={amt} settled={settled}"); }
    for (op, amt, settled) in &send_allocs { println!("RGB06 - [standard] sender   alloc: {op} amount={amt} settled={settled}"); }
    let send_bal = tokio::task::block_in_place(|| issuer.settled_balance(&contract))?;
    println!("RGB06 - receiver settled balance = {recv_bal} (on B); sender settled balance = {send_bal} (change on C)");

    assert_eq!(recv_bal, RECEIVE_AMT, "receiver must hold {RECEIVE_AMT} on its statechain UTXO B");
    assert!(recv_allocs.iter().any(|(op, amt, s)| op == &format!("{txid_b}:{vout_b}") && *amt == RECEIVE_AMT && *s),
        "received {RECEIVE_AMT} must sit on the receiver's statechain UTXO B");
    assert_eq!(chg_bal, CHANGE_AMT, "change {CHANGE_AMT} must settle on the sender's free statechain UTXO C");
    assert_eq!(send_bal, CHANGE_AMT, "sender balance must drop to the change {CHANGE_AMT} (A consumed)");
    assert!(tokio::task::block_in_place(|| is_outpoint_spent(&cc, &txid_a, vout_a)),
        "the sender's statechain UTXO A must be consumed on-chain");

    println!("RGB06 - SUCCESS: partial statechain->statechain transfer - {RECEIVE_AMT} to the receiver's statechain UTXO, {CHANGE_AMT} change back to a free statechain UTXO, A consumed (RGB the same way it works on-chain).");
    Ok(())
}
