//! E2E: **standard NIA transfer parity over statechain — BLINDED then WITNESS**, single recipient.
//!
//! This exercises the two ways a receiver can be paid an RGB allocation held on a Mercury statechain
//! coin, back-to-back over the same asset:
//!
//!   LEG A (BLINDED): the receiver publishes a `blind_receive` invoice whose seal is one of its OWN
//!   free colorable statechain UTXOs (a statechain coin registered with rgb_amount==0). The issuer
//!   spends its funded statechain coin R via the blinded coloring path (`create_colored_backup_tx`
//!   with a `blinded` beneficiary list): the RGB asset is re-assigned to the receiver's blinded seal
//!   (400) and the issuer's own blinded change seal (600); the single bitcoin output pays the issuer's
//!   own witness address. Validated off-chain against the un-broadcast backup tx, then exited on-chain.
//!
//!   LEG B (WITNESS): from the issuer's 600 change coin the issuer does a standard *witness* transfer
//!   of 200 to the receiver (`witness_receive` + `address_from_recipient_id`, exactly like rgb01/02),
//!   carved as a two-output split (recipient 200 + change 400). Validated off-chain, exited on-chain.
//!
//! End state: issuer settles at 400, receiver settles at 600 (400 blinded + 200 witness).
//!
//! Run with RGB_E2E=9. Requires the regtest + Mercury (lockbox) stack.

use std::{env, fs, process::Command, str::FromStr, thread, time::Duration};

use anyhow::{anyhow, Result};
use electrum_client::ElectrumApi;
use mercury_rgb::RgbWallet;
use mercuryrustlib::{client_config::ClientConfig, Coin, CoinStatus};

use crate::{bitcoin_core, electrs};

const ELECTRUM_URL: &str = "127.0.0.1:50001";
const RGB_PROXY: &str = "rpc://127.0.0.1:3000/json-rpc";
const NETWORK: &str = "regtest";
const BLINDING: u64 = 91;
const ISSUED: u64 = 1000;

// LEG A (blinded): R (1000) -> receiver 400 (blinded) + issuer change 600 (blinded).
const A_RECV_AMT: u64 = 400;
const A_CHANGE_AMT: u64 = 600;
// LEG B (witness): issuer change coin C (600) -> receiver 200 (witness) + issuer change 400 (witness).
const B_RECV_AMT: u64 = 200;
const B_CHANGE_AMT: u64 = 400;

const COIN_SAT: u32 = 90_000; // funded root coin R
const FREE_SAT: u32 = 40_000; // free colorable statechain coins (receiver seal + issuer change seal)

// LEG A self-output (sats stay with the sender; RGB moves to blinded seals) + on-exit fee.
const A_SELF_SAT: u64 = 65_000; // 90_000 - 65_000 = 25_000 on-exit fee
// LEG B witness split sat sizes. C is the issuer's blinded CHANGE SEAL from LEG A — a free colorable
// statechain coin of FREE_SAT (40_000) sats — so the two split outputs must fit inside it with a fee.
const B_RECV_SAT: u64 = 18_000;
const B_CHANGE_SAT: u64 = 12_000; // 40_000 - 30_000 = 10_000 split fee

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
        mercuryrustlib::deposit::get_deposit_bitcoin_address_single_use(cc, &wallet.name, &token_id, size_sat).await?;
    let _txid = fund(&sc_address)?;
    wait_for_address(cc, &sc_address, size_sat).await?;
    let core = bitcoin_core::getnewaddress()?;
    let _ = bitcoin_core::generatetoaddress(cc.confirmation_target, &core)?;
    mercuryrustlib::coin_status::update_coins(cc, &wallet.name).await?;
    Ok(sc_address)
}

/// Open a Mercury deposit address WITHOUT funding it (a later tx pays it). Returns the address.
async fn open_deposit_address(cc: &ClientConfig, wallet_name: &str, size_sat: u32) -> Result<String> {
    let wallet = mercuryrustlib::wallet::create_wallet(wallet_name, cc).await?;
    mercuryrustlib::sqlite_manager::insert_wallet(&cc.pool, &wallet).await?;
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    let token_id = crate::utils::handle_token_response(cc, &token).await?;
    Ok(mercuryrustlib::deposit::get_deposit_bitcoin_address_single_use(cc, &wallet.name, &token_id, size_sat).await?)
}

async fn coin_outpoint(cc: &ClientConfig, wallet_name: &str, sc_address: &str) -> Result<(String, u32, Coin)> {
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

/// Deposit a FREE colorable statechain coin (no asset) onto `wallet_name`, register it with
/// rgb_amount==0 so rgb-lib treats it as a colorable UTXO the wallet can `blind_receive` onto.
/// Returns `(txid, vout)` of the free statechain UTXO.
async fn free_statechain_coin(
    cc: &ClientConfig,
    rgb: &mut RgbWallet,
    wallet_name: &str,
    contract: &str,
) -> Result<(String, u32)> {
    // A free statechain coin is funded by an ordinary Mercury deposit (plain sats, no coloring): the
    // rgb wallet's own BDK funds pay it. Fund it directly from the rgb wallet's on-chain balance by
    // sending to the statechain deposit address, then register it with rgb_amount==0.
    let addr = deposit_coin(cc, wallet_name, FREE_SAT, |sc| {
        let txid = bitcoin_core::sendtoaddress(FREE_SAT, sc)?;
        Ok(txid)
    })
    .await?;
    let (txid, vout, _coin) = coin_outpoint(cc, wallet_name, &addr).await?;
    tokio::task::block_in_place(|| {
        rgb.register_statechain(&txid, vout, FREE_SAT as u64, contract, 0, &[])
    })?;
    Ok((txid, vout))
}

pub async fn execute() -> Result<()> {
    let _ = Command::new("rm").arg("wallet.db").arg("wallet.db-shm").arg("wallet.db-wal").output();
    let _ = fs::remove_dir_all("./rgb-data9");
    env::set_var("ML_NETWORK", "regtest");
    let cc = mercuryrustlib::client_config::load().await;

    let (mut issuer, contract) = tokio::task::block_in_place(|| setup("./rgb-data9/issuer", true, true))?;
    let contract = contract.unwrap();
    let (mut receiver, _) = tokio::task::block_in_place(|| setup("./rgb-data9/receiver", false, false))?;
    let issued_bal = tokio::task::block_in_place(|| issuer.settled_balance(&contract))?;
    assert_eq!(issued_bal, ISSUED, "issuer must hold the full {ISSUED} right after issuance");
    println!("RGB09 - issued {ISSUED} units of {contract} (issuer settled_balance={issued_bal})");

    // ---- 1. Deposit the whole 1000 onto the funded root statechain coin R; register it. ----
    let sources: Vec<String> = tokio::task::block_in_place(|| issuer.list_allocations(&contract))?
        .into_iter().map(|(op, _, _)| op).collect();
    let addr_r = deposit_coin(&cc, "rgb09_root", COIN_SAT, |sc| {
        let (_t, _v, _c, signed) = tokio::task::block_in_place(|| {
            issuer.fund_statechain(sc, COIN_SAT as u64, &contract, ISSUED, 2, BLINDING)
        })?;
        Ok(cc.electrum_client.transaction_broadcast_raw(&hex::decode(&signed)?)?.to_string())
    }).await?;
    let (txid_r, vout_r, mut coin_r) = coin_outpoint(&cc, "rgb09_root", &addr_r).await?;
    tokio::task::block_in_place(|| {
        issuer.register_statechain(&txid_r, vout_r, COIN_SAT as u64, &contract, ISSUED, &sources)
    })?;
    println!("RGB09 - R = {txid_r}:{vout_r} holds {}",
        tokio::task::block_in_place(|| issuer.settled_balance(&contract))?);

    // =====================================================================================
    // LEG A (BLINDED): receiver blind-receives 400 onto its OWN free statechain UTXO; issuer's
    // 600 change goes to ITS OWN free statechain blinded seal. RGB moves to blinded seals; the
    // single bitcoin output pays the issuer's own witness address. Off-chain validate, then exit.
    // =====================================================================================
    // Receiver's free colorable statechain seal to blind-receive onto (STATECHAIN seal — preferred).
    let (recv_seal_txid, recv_seal_vout) =
        free_statechain_coin(&cc, &mut receiver, "rgb09_recv_seal", &contract).await?;
    // Issuer's free colorable statechain seal to hold the 600 blinded change.
    let (chg_seal_txid, chg_seal_vout) =
        free_statechain_coin(&cc, &mut issuer, "rgb09_chg_seal", &contract).await?;
    println!("RGB09 - [LEG A] receiver seal {recv_seal_txid}:{recv_seal_vout}, issuer change seal {chg_seal_txid}:{chg_seal_vout}");

    // Blinded invoices: recipient_id IS the blinded seal (NOT a witness address). The receiver has
    // never seen this contract, so it issues a UNIVERSAL blinded invoice (asset_id=None) — the
    // contract arrives with the consignment. The issuer already knows its own contract.
    let a_recv_id = tokio::task::block_in_place(|| receiver.blind_receive(None, A_RECV_AMT))?;
    let a_change_id = tokio::task::block_in_place(|| issuer.blind_receive(Some(&contract), A_CHANGE_AMT))?;

    // Sender self-address for the single bitcoin output (sats stay with the sender in a blinded transfer).
    let a_self_addr = open_deposit_address(&cc, "rgb09_a_self", A_SELF_SAT as u32).await?;

    let si = mercuryrustlib::utils::info_config(&cc).await?;
    // Blinded coloring: assign 400 -> receiver's blinded seal, 600 -> issuer's blinded change seal.
    let a_blinded: Vec<(String, u64)> = vec![
        (a_recv_id.clone(), A_RECV_AMT),
        (a_change_id.clone(), A_CHANGE_AMT),
    ];
    let leg_a = mercuryrustlib::rgb::create_colored_backup_tx(
        &cc,
        &issuer,
        &mut coin_r,
        &contract,
        A_RECV_AMT + A_CHANGE_AMT,
        &a_self_addr,
        1,
        true,
        None,
        NETWORK,
        si.fee_rate_sats_per_byte,
        si.initlock,
        si.interval,
        BLINDING,
        Some(a_blinded.as_slice()),
    ).await?;
    println!("RGB09 - [LEG A] built blinded backup tx {} (NOT broadcast)", leg_a.txid);
    assert!(!is_outpoint_spent(&cc, &txid_r, vout_r), "off-chain: R still UNSPENT before exit");

    // Receiver validates the un-broadcast backup tx off-chain (the blinded transition assigns 400 to
    // its seal). The witness txid is the backup tx.
    let (a_valid, a_detail) = tokio::task::block_in_place(|| {
        receiver.validate_offchain_chain(&leg_a.consignment, &[leg_a.txid.clone()])
    })?;
    println!("RGB09 - [LEG A][receiver] validate_offchain_chain -> valid={a_valid} detail={:?}", a_detail);
    assert!(a_valid, "receiver must validate the LEG A blinded transfer off-chain");

    // Post the consignment to both blinded recipients (keyed by their blinded recipient_id), broadcast
    // the exit, mark R spent, then settle both wallets.
    tokio::task::block_in_place(|| {
        receiver.post_consignment(&a_recv_id, &leg_a.consignment, &leg_a.txid, leg_a.recipient_vout)?;
        issuer.post_consignment(&a_change_id, &leg_a.consignment, &leg_a.txid, leg_a.recipient_vout)
    })?;
    let _ = cc.electrum_client.transaction_broadcast_raw(&hex::decode(&leg_a.signed_tx)?)?;
    tokio::task::block_in_place(|| issuer.mark_spent(&[format!("{txid_r}:{vout_r}")]))?;

    let mut a_recv_bal = 0;
    let mut a_chg_bal = 0;
    for _ in 0..12 {
        let core = bitcoin_core::getnewaddress()?;
        let _ = bitcoin_core::generatetoaddress(cc.confirmation_target.max(1), &core)?;
        tokio::task::block_in_place(|| { let _ = issuer.refresh(None); let _ = receiver.refresh(None); });
        a_recv_bal = tokio::task::block_in_place(|| receiver.settled_balance(&contract)).unwrap_or(0);
        a_chg_bal = tokio::task::block_in_place(|| issuer.settled_balance(&contract)).unwrap_or(0);
        if a_recv_bal == A_RECV_AMT && a_chg_bal == A_CHANGE_AMT { break; }
        thread::sleep(Duration::from_secs(1));
    }
    crate::rgb_dump::dump("issuer after LEG A (blinded, change 600)", &mut issuer, &contract);
    crate::rgb_dump::dump("receiver after LEG A (blinded, got 400)", &mut receiver, &contract);
    assert_eq!(a_recv_bal, A_RECV_AMT, "receiver must hold {A_RECV_AMT} after the blinded LEG A");
    assert_eq!(a_chg_bal, A_CHANGE_AMT, "issuer change must be {A_CHANGE_AMT} after the blinded LEG A");
    assert!(is_outpoint_spent(&cc, &txid_r, vout_r), "R consumed by the LEG A exit");
    println!("RGB09 - [LEG A] SUCCESS: blinded 400 -> receiver seal, 600 change -> issuer seal.");

    // =====================================================================================
    // LEG B (WITNESS): the issuer's 600 change now lives on the blinded change seal (an on-chain
    // colorable UTXO after the LEG A exit). Register it as a statechain change coin C and do a
    // standard WITNESS transfer of 200 to the receiver (200 recipient + 400 change), carved as a
    // two-output split, validated off-chain, exited on-chain.
    // =====================================================================================
    // The 600 landed at the issuer's blinded change seal outpoint; that seal is chg_seal_txid:vout
    // (rgb-lib assigned the blinded allocation to that pre-existing UTXO). Locate the outpoint holding
    // 600 from list_allocations, then treat it as the LEG B input coin C.
    let allocs = tokio::task::block_in_place(|| issuer.list_allocations(&contract))?;
    let (c_op, _c_amt, _c_settled) = allocs
        .iter()
        .find(|(_, amt, settled)| *amt == A_CHANGE_AMT && *settled)
        .cloned()
        .ok_or(anyhow!("issuer 600 change allocation not found for LEG B"))?;
    let (c_txid, c_vout_str) = c_op.rsplit_once(':').ok_or(anyhow!("bad outpoint {c_op}"))?;
    let c_vout: u32 = c_vout_str.parse()?;
    println!("RGB09 - [LEG B] issuer change coin C = {c_txid}:{c_vout} holds {A_CHANGE_AMT}");

    // Build a Coin record for C. C is the blinded-change seal UTXO, which is the free statechain coin
    // we opened as "rgb09_chg_seal" — reuse that deposit coin's key material, patched to C's outpoint.
    // (The seal outpoint chg_seal_txid:chg_seal_vout IS where the 600 landed; assert consistency.)
    assert_eq!(
        (c_txid.to_string(), c_vout),
        (chg_seal_txid.clone(), chg_seal_vout),
        "LEG B input C must be the issuer's blinded change seal from LEG A"
    );
    let mut coin_c = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "rgb09_chg_seal")
        .await?
        .coins
        .iter()
        .find(|c| c.aggregated_address.is_some())
        .cloned()
        .ok_or(anyhow!("issuer change-seal coin not found"))?;
    coin_c.utxo_txid = Some(c_txid.to_string());
    coin_c.utxo_vout = Some(c_vout);
    coin_c.amount = Some(FREE_SAT);
    coin_c.status = CoinStatus::CONFIRMED;

    // Witness invoices: recipient 200 (receiver) + change 400 (issuer). recipient_id -> witness address.
    let b_recv_id = tokio::task::block_in_place(|| receiver.witness_receive(B_RECV_AMT))?;
    let b_recv_addr = tokio::task::block_in_place(|| receiver.address_from_recipient_id(&b_recv_id))?;
    let b_change_id = tokio::task::block_in_place(|| issuer.witness_receive(B_CHANGE_AMT))?;
    let b_change_addr = tokio::task::block_in_place(|| issuer.address_from_recipient_id(&b_change_id))?;

    let splits = vec![
        (b_recv_addr.clone(), B_RECV_SAT, B_RECV_AMT),
        (b_change_addr.clone(), B_CHANGE_SAT, B_CHANGE_AMT),
    ];
    let leg_b = mercuryrustlib::rgb::create_colored_split_tx(
        &cc, &issuer, &mut coin_c, &contract, &splits, 1, true, None, NETWORK,
        si.initlock, si.interval, BLINDING,
    ).await?;
    println!("RGB09 - [LEG B] built witness split tx {} (NOT broadcast); vouts={:?}", leg_b.txid, leg_b.output_vouts);
    assert!(!is_outpoint_spent(&cc, c_txid, c_vout), "off-chain: C still UNSPENT before LEG B exit");

    let (b_valid, b_detail) = tokio::task::block_in_place(|| {
        receiver.validate_offchain_chain(&leg_b.consignment, &[leg_b.txid.clone()])
    })?;
    println!("RGB09 - [LEG B][receiver] validate_offchain_chain -> valid={b_valid} detail={:?}", b_detail);
    assert!(b_valid, "receiver must validate the LEG B witness transfer off-chain");

    tokio::task::block_in_place(|| {
        receiver.post_consignment(&b_recv_id, &leg_b.consignment, &leg_b.txid, leg_b.output_vouts[0])?;
        issuer.post_consignment(&b_change_id, &leg_b.consignment, &leg_b.txid, leg_b.output_vouts[1])
    })?;
    let _ = cc.electrum_client.transaction_broadcast_raw(&hex::decode(&leg_b.signed_tx)?)?;
    tokio::task::block_in_place(|| issuer.mark_spent(&[format!("{c_txid}:{c_vout}")]))?;

    let mut b_recv_total = 0;
    let mut b_chg_bal = 0;
    for _ in 0..12 {
        let core = bitcoin_core::getnewaddress()?;
        let _ = bitcoin_core::generatetoaddress(cc.confirmation_target.max(1), &core)?;
        tokio::task::block_in_place(|| { let _ = issuer.refresh(None); let _ = receiver.refresh(None); });
        b_recv_total = tokio::task::block_in_place(|| receiver.settled_balance(&contract)).unwrap_or(0);
        b_chg_bal = tokio::task::block_in_place(|| issuer.settled_balance(&contract)).unwrap_or(0);
        if b_recv_total == A_RECV_AMT + B_RECV_AMT && b_chg_bal == B_CHANGE_AMT { break; }
        thread::sleep(Duration::from_secs(1));
    }
    crate::rgb_dump::dump("issuer after LEG B (change 400)", &mut issuer, &contract);
    crate::rgb_dump::dump("receiver after LEG B (total 600)", &mut receiver, &contract);

    assert_eq!(b_chg_bal, B_CHANGE_AMT, "issuer change must be {B_CHANGE_AMT} after LEG B");
    assert_eq!(b_recv_total, A_RECV_AMT + B_RECV_AMT,
        "receiver total must be {} (400 blinded + 200 witness) after LEG B", A_RECV_AMT + B_RECV_AMT);
    assert!(is_outpoint_spent(&cc, c_txid, c_vout), "C consumed by the LEG B exit");

    println!("RGB09 - SUCCESS: NIA transfer parity over statechain — LEG A blinded (400 to receiver's STATECHAIN seal + 600 issuer change), LEG B witness (200 to receiver). Issuer settled 400, receiver settled 600 (400 blinded + 200 witness).");
    Ok(())
}
