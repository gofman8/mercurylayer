//! E2E (Stage 1 of `docs/rgb_offchain_split_spilman.md`): **off-chain RGB split into sub-coins that
//! are outputs of an un-broadcast transaction**.
//!
//! Splitting an allocation across seals that are each a *separately-funded* statechain coin makes N
//! pieces cost N on-chain UTXOs up front. Here the two sub-coins are instead **witness outputs of one
//! un-broadcast split tx** off a single root coin — zero on-chain cost per piece until exit. This is
//! the pseudo-Spilman "carve sub-coins as the spend's own outputs" property.
//!
//! Flow:
//!   1. Issuer issues 1000 and deposits it onto a single root statechain coin R; registers R.
//!   2. Splitter (issuer) witness-invoices its 700 "reserve"; receiver witness-invoices its 300.
//!   3. `create_colored_split_tx` spends R into TWO colored witness outputs (700 + 300) committing one
//!      RGB transition (one OP_RETURN), blind-MuSig2 co-signed with the SE, **NOT broadcast**.
//!      (Asserts the tx shape: exactly 2 spendable outputs + 1 OP_RETURN.)
//!   4. Both sub-coin owners VALIDATE the split OFF-CHAIN against the unbroadcast tx (no Bitcoin tx).
//!   5. Exit: broadcast the split tx; both sub-allocations settle on-chain (receiver 300, splitter
//!      700) and R is consumed. Proves the un-broadcast split is a valid unilateral exit.
//!
//! Run with RGB_E2E=1. Requires the regtest + Mercury (lockbox) stack.

use std::{env, fs, process::Command, str::FromStr, thread, time::Duration};

use anyhow::{anyhow, Result};
use electrum_client::ElectrumApi;
use mercury_rgb::RgbWallet;
use mercuryrustlib::{client_config::ClientConfig, CoinStatus};

use crate::{bitcoin_core, electrs};

const ELECTRUM_URL: &str = "127.0.0.1:50001";
const RGB_PROXY: &str = "rpc://127.0.0.1:3000/json-rpc";
const NETWORK: &str = "regtest";
const BLINDING: u64 = 41;
const ISSUED: u64 = 1000;
const KEEP_AMT: u64 = 700; // splitter keeps (reserve)
const RECV_AMT: u64 = 300; // goes to the receiver
const COIN_SAT_ROOT: u32 = 50_000;
const KEEP_SAT: u64 = 25_000;
const RECV_SAT: u64 = 20_000; // remainder (5_000) is the on-exit miner fee

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

/// Mercury-deposit a statechain coin to `wallet_name` of `size_sat`; return the deposit address.
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
    let _ = fs::remove_dir_all("./rgb-data1");
    env::set_var("ML_NETWORK", "regtest");
    let cc = mercuryrustlib::client_config::load().await;

    let (mut issuer, contract) = tokio::task::block_in_place(|| setup("./rgb-data1/issuer", true, true))?;
    let contract = contract.unwrap();
    let (mut receiver, _) = tokio::task::block_in_place(|| setup("./rgb-data1/receiver", false, false))?;
    println!("RGB01 - issued {ISSUED} units of {contract}");

    // ---- 1. Deposit the whole 1000 onto a single root statechain coin R; register it. ----
    let sources: Vec<String> = tokio::task::block_in_place(|| issuer.list_allocations(&contract))?
        .into_iter().map(|(op, _, _)| op).collect();
    let addr_r = deposit_coin(&cc, "rgb01_root", COIN_SAT_ROOT, |sc| {
        let (_t, _v, _c, signed) = tokio::task::block_in_place(|| {
            issuer.fund_statechain(sc, COIN_SAT_ROOT as u64, &contract, ISSUED, 2, BLINDING)
        })?;
        Ok(cc.electrum_client.transaction_broadcast_raw(&hex::decode(&signed)?)?.to_string())
    }).await?;
    let (txid_r, vout_r, mut coin_r) = coin_outpoint(&cc, "rgb01_root", &addr_r).await?;
    tokio::task::block_in_place(|| {
        issuer.register_statechain(&txid_r, vout_r, COIN_SAT_ROOT as u64, &contract, ISSUED, &sources)
    })?;
    println!("RGB01 - root coin R = {txid_r}:{vout_r} holds {} (registered)",
        tokio::task::block_in_place(|| issuer.settled_balance(&contract))?);

    // ---- 2. Witness invoices for each sub-coin (splitter keeps 700, receiver gets 300). ----
    let keep_id = tokio::task::block_in_place(|| issuer.witness_receive(KEEP_AMT))?;
    let keep_addr = tokio::task::block_in_place(|| issuer.address_from_recipient_id(&keep_id))?;
    let recv_id = tokio::task::block_in_place(|| receiver.witness_receive(RECV_AMT))?;
    let recv_addr = tokio::task::block_in_place(|| receiver.address_from_recipient_id(&recv_id))?;
    println!("RGB01 - sub-coin invoices: keep {KEEP_AMT}@{keep_addr}, recv {RECV_AMT}@{recv_addr}");

    // ---- 3. Build the colored SPLIT tx: R -> [700 @ keep_addr, 300 @ recv_addr] + OP_RETURN. NOT broadcast. ----
    let si = mercuryrustlib::utils::info_config(&cc).await?;
    let splits = vec![
        (keep_addr.clone(), KEEP_SAT, KEEP_AMT),
        (recv_addr.clone(), RECV_SAT, RECV_AMT),
    ];
    let split = mercuryrustlib::rgb::create_colored_split_tx(
        &cc, &issuer, &mut coin_r, &contract, &splits, 1, true, None, NETWORK,
        si.initlock, si.interval, BLINDING,
    ).await?;
    println!("RGB01 - built split tx {} (NOT broadcast); sub-coin vouts = {:?}", split.txid, split.output_vouts);

    // Structural proof: the un-broadcast split tx is one input -> two spendable outputs + one OP_RETURN.
    let raw = hex::decode(&split.signed_tx)?;
    let stx: electrum_client::bitcoin::Transaction = electrum_client::bitcoin::consensus::deserialize(&raw)?;
    let op_returns = stx.output.iter().filter(|o| o.script_pubkey.is_op_return()).count();
    let spendable = stx.output.len() - op_returns;
    assert_eq!(stx.input.len(), 1, "split spends exactly the root coin");
    assert_eq!(spendable, 2, "split must carve exactly two sub-coin outputs");
    assert_eq!(op_returns, 1, "exactly one OP_RETURN opret commitment");
    assert_eq!(split.output_vouts.len(), 2, "two sub-coin vouts reported");
    assert!(!is_outpoint_spent(&cc, &txid_r, vout_r), "off-chain: R is still UNSPENT (nothing broadcast)");

    // ---- 4. Validate the split OFF-CHAIN against the un-broadcast tx (both sub-coin owners). ----
    let (recv_valid, recv_detail) = tokio::task::block_in_place(|| {
        receiver.validate_offchain(&split.consignment, &split.txid)
    })?;
    println!("RGB01 - [receiver] validate_offchain(txid={}) -> valid={recv_valid} detail={:?}", split.txid, recv_detail);
    assert!(recv_valid, "receiver must validate the off-chain split (un-broadcast tx commits 300 to its witness seal)");
    assert!(!is_outpoint_spent(&cc, &txid_r, vout_r), "after off-chain acceptance, R is still UNSPENT");
    println!("RGB01 - off-chain split accepted with NO Bitcoin transaction (R unspent)");

    // ---- 5. Exit: broadcast the split -> both sub-allocations settle on-chain, R consumed. ----
    tokio::task::block_in_place(|| {
        issuer.post_consignment(&keep_id, &split.consignment, &split.txid, split.output_vouts[0])?;
        receiver.post_consignment(&recv_id, &split.consignment, &split.txid, split.output_vouts[1])
    })?;
    let _ = cc.electrum_client.transaction_broadcast_raw(&hex::decode(&split.signed_tx)?)?;
    // R is consumed on-chain by the broadcast split, but it is BDK-invisible (a statechain UTXO), so
    // rgb-lib keeps counting its 1000 allocation until told. Mark it spent BEFORE measuring, so the
    // splitter's balance reflects only the new 700 sub-coin (a statechain UTXO is BDK-invisible).
    tokio::task::block_in_place(|| issuer.mark_spent(&[format!("{txid_r}:{vout_r}")]))?;
    let mut keep_bal = 0;
    let mut recv_bal = 0;
    for _ in 0..12 {
        let core = bitcoin_core::getnewaddress()?;
        let _ = bitcoin_core::generatetoaddress(cc.confirmation_target.max(1), &core)?;
        tokio::task::block_in_place(|| { let _ = issuer.refresh(None); let _ = receiver.refresh(None); });
        keep_bal = tokio::task::block_in_place(|| issuer.settled_balance(&contract)).unwrap_or(0);
        recv_bal = tokio::task::block_in_place(|| receiver.settled_balance(&contract)).unwrap_or(0);
        if keep_bal == KEEP_AMT && recv_bal == RECV_AMT { break; }
        thread::sleep(Duration::from_secs(1));
    }

    crate::rgb_dump::dump("splitter after exit (keeps 700)", &mut issuer, &contract);
    crate::rgb_dump::dump("receiver after exit (got 300)", &mut receiver, &contract);

    assert_eq!(recv_bal, RECV_AMT, "receiver must hold {RECV_AMT} on its sub-coin after the exit");
    assert_eq!(keep_bal, KEEP_AMT, "splitter must hold {KEEP_AMT} on its sub-coin after the exit");
    assert!(is_outpoint_spent(&cc, &txid_r, vout_r), "root coin R must be consumed by the broadcast split");

    println!("RGB01 - SUCCESS: off-chain split of 1000 -> 700 + 300 as witness sub-coins of one un-broadcast tx, validated off-chain, then exited on-chain. One root UTXO, two sub-coins, no per-piece on-chain cost until exit.");
    Ok(())
}
