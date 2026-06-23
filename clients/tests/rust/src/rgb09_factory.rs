//! E2E (Stage 5 — multi-owner factory): **one on-chain root amortized across many distinct owners**.
//! An operator (issuer) deposits the whole supply onto ONE root statechain coin, then splits it
//! off-chain into N sub-coins, each belonging to a DIFFERENT owner wallet (its own keys). Every owner
//! independently validates its own sub-coin off-chain (it holds only its own consignment), and any
//! owner can unilaterally exit — broadcasting the one split tx materializes every owner's allocation
//! on-chain at once. N sovereign owners, ONE on-chain UTXO until exit.
//!
//! This is the realistic issuer/custodian factory: the root is single-use + epoch-bounded, so the
//! distribution is double-spend-protected and time-bounded. (The advanced variant where the owners
//! JOINTLY control the root via n-of-n MuSig2 and co-sign in-place updates needs multi-party keygen
//! in the lockbox; this demonstrates the operator-distributes shape, which covers the core value.)
//!
//! Run with RGB_E2E=9. Requires the regtest + Mercury (lockbox) stack.

use std::{env, fs, process::Command, str::FromStr, thread, time::{Duration, SystemTime, UNIX_EPOCH}};

use anyhow::{anyhow, Result};
use electrum_client::ElectrumApi;
use mercury_rgb::RgbWallet;
use mercuryrustlib::{client_config::ClientConfig, CoinStatus};

use crate::{bitcoin_core, electrs};

const ELECTRUM_URL: &str = "127.0.0.1:50001";
const RGB_PROXY: &str = "rpc://127.0.0.1:3000/json-rpc";
const NETWORK: &str = "regtest";
const BLINDING: u64 = 99;
const ISSUED: u64 = 1000;
const COIN_SAT_ROOT: u32 = 80_000;
const EPOCH_FAR: u64 = 3600;
// Per-owner (asset amount, sat for the sub-coin output). 500+300+200 = 1000; 25k+20k+15k = 60k; fee 20k.
const OWNERS: [(u64, u64); 3] = [(500, 25_000), (300, 20_000), (200, 15_000)];

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

async fn deposit_root<F>(cc: &ClientConfig, wallet_name: &str, size_sat: u32, fund: F) -> Result<String>
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

async fn coin_outpoint(cc: &ClientConfig, wallet_name: &str, sc_address: &str) -> Result<(String, u32, mercuryrustlib::Coin)> {
    let coin = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, wallet_name)
        .await?
        .coins.iter()
        .find(|c| c.aggregated_address.as_deref() == Some(sc_address))
        .ok_or(anyhow!("coin not found for {sc_address}"))?.clone();
    assert!(coin.status == CoinStatus::CONFIRMED, "root statechain coin must confirm");
    let txid = coin.utxo_txid.clone().ok_or(anyhow!("no utxo_txid"))?;
    let vout = coin.utxo_vout.ok_or(anyhow!("no utxo_vout"))?;
    Ok((txid, vout, coin))
}

pub async fn execute() -> Result<()> {
    let _ = Command::new("rm").arg("wallet.db").arg("wallet.db-shm").arg("wallet.db-wal").output();
    let _ = fs::remove_dir_all("./rgb-data9");
    env::set_var("ML_NETWORK", "regtest");
    let cc = mercuryrustlib::client_config::load().await;

    // Operator (issuer) holds the supply; N distinct owner wallets each get their own keys.
    let (mut issuer, contract) = tokio::task::block_in_place(|| setup("./rgb-data9/issuer", true, true))?;
    let contract = contract.unwrap();
    let mut owners: Vec<RgbWallet> = Vec::new();
    for i in 0..OWNERS.len() {
        let (w, _) = tokio::task::block_in_place(|| setup(&format!("./rgb-data9/owner{i}"), false, false))?;
        owners.push(w);
    }
    println!("RGB09 - operator issued {ISSUED} units of {contract}; {} distinct owners", OWNERS.len());

    // ---- Operator deposits the whole supply onto ONE root statechain coin (single-use + epoch). ----
    let sources: Vec<String> = tokio::task::block_in_place(|| issuer.list_allocations(&contract))?
        .into_iter().map(|(op, _, _)| op).collect();
    let addr_r = deposit_root(&cc, "rgb09_root", COIN_SAT_ROOT, |sc| {
        let (_t, _v, _c, signed) = tokio::task::block_in_place(|| {
            issuer.fund_statechain(sc, COIN_SAT_ROOT as u64, &contract, ISSUED, 2, BLINDING)
        })?;
        Ok(cc.electrum_client.transaction_broadcast_raw(&hex::decode(&signed)?)?.to_string())
    }).await?;
    let (txid_r, vout_r, mut coin_r) = coin_outpoint(&cc, "rgb09_root", &addr_r).await?;
    tokio::task::block_in_place(|| {
        issuer.register_statechain(&txid_r, vout_r, COIN_SAT_ROOT as u64, &contract, ISSUED, &sources)
    })?;
    println!("RGB09 - ONE root UTXO {txid_r}:{vout_r} holds {ISSUED} (single-use + epoch-bounded)");

    // ---- Each owner witness-invoices its allocation; operator splits the root to all of them. ----
    let mut recv_ids: Vec<String> = Vec::new();
    let mut splits: Vec<(String, u64, u64)> = Vec::new();
    for (i, owner) in owners.iter_mut().enumerate() {
        let (amt, sat) = OWNERS[i];
        let id = tokio::task::block_in_place(|| owner.witness_receive(amt))?;
        let addr = tokio::task::block_in_place(|| owner.address_from_recipient_id(&id))?;
        splits.push((addr, sat, amt));
        recv_ids.push(id);
    }
    let si = mercuryrustlib::utils::info_config(&cc).await?;
    let split = mercuryrustlib::rgb::create_colored_split_tx(
        &cc, &issuer, &mut coin_r, &contract, &splits, 1, true, None, NETWORK, si.initlock, si.interval, BLINDING,
    ).await?;
    println!("RGB09 - operator split the root into {} owner sub-coins (tx {}, NOT broadcast)", OWNERS.len(), split.txid);
    assert!(!is_outpoint_spent(&cc, &txid_r, vout_r), "off-chain: the root is still UNSPENT (nothing broadcast)");

    // ---- Each owner INDEPENDENTLY validates only its own sub-coin off-chain. ----
    for (i, owner) in owners.iter_mut().enumerate() {
        let (valid, detail) = tokio::task::block_in_place(|| owner.validate_offchain(&split.consignment, &split.txid))?;
        println!("RGB09 - [owner{i}] validate_offchain -> valid={valid} detail={detail:?}");
        assert!(valid, "owner{i} must independently validate its allocation off-chain");
    }
    assert!(!is_outpoint_spent(&cc, &txid_r, vout_r), "after all owners accept off-chain, the root is still UNSPENT");
    println!("RGB09 - all {} owners hold a validated off-chain claim; ZERO on-chain cost so far", OWNERS.len());

    // ---- Unilateral exit: any owner broadcasting the one split tx materializes EVERY allocation. ----
    for (i, owner) in owners.iter_mut().enumerate() {
        tokio::task::block_in_place(|| owner.post_consignment(&recv_ids[i], &split.consignment, &split.txid, split.output_vouts[i]))?;
    }
    let _ = cc.electrum_client.transaction_broadcast_raw(&hex::decode(&split.signed_tx)?)?;
    tokio::task::block_in_place(|| issuer.mark_spent(&[format!("{txid_r}:{vout_r}")]))?;

    let mut balances = vec![0u64; OWNERS.len()];
    for _ in 0..12 {
        let core = bitcoin_core::getnewaddress()?;
        let _ = bitcoin_core::generatetoaddress(cc.confirmation_target.max(1), &core)?;
        tokio::task::block_in_place(|| {
            for owner in owners.iter_mut() { let _ = owner.refresh(None); }
        });
        for (i, owner) in owners.iter_mut().enumerate() {
            balances[i] = tokio::task::block_in_place(|| owner.settled_balance(&contract)).unwrap_or(0);
        }
        if balances.iter().zip(OWNERS.iter()).all(|(b, (amt, _))| b == amt) { break; }
        thread::sleep(Duration::from_secs(1));
    }
    for (i, (amt, _)) in OWNERS.iter().enumerate() {
        assert_eq!(balances[i], *amt, "owner{i} must hold {amt} on its own sub-coin after exit");
        println!("RGB09 - owner{i} settled {} on-chain (sovereign allocation)", balances[i]);
    }
    assert!(is_outpoint_spent(&cc, &txid_r, vout_r), "the one root UTXO is consumed by the broadcast split");

    let total: u64 = OWNERS.iter().map(|(a, _)| a).sum();
    println!("RGB09 - SUCCESS: one on-chain root UTXO amortized across {} distinct owners ({total} units total). Each owner independently validated and settled its own allocation; the whole distribution lived off-chain (single-use + epoch-bounded) until a single exit tx materialized every owner's coin.", OWNERS.len());
    Ok(())
}
