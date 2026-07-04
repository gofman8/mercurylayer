//! E2E: **off-chain resolver safety — reject a consignment with an omitted ancestor witness txid.**
//!
//! The whole off-chain model rests on the receiver being able to resolve EVERY un-broadcast witness
//! in a consignment's branch. If a receiver would book a coin while an ancestor witness txid is
//! unaccounted for, the single-use / value-conservation guarantee is void. This test proves the
//! receiver's `validate_offchain_chain` REJECTS a leaf when the caller omits an ancestor's txid.
//!
//! A 2-deep off-chain chain is built (mirrors rgb03/rgb06 chaining mechanics), NEITHER tx broadcast:
//!   ROOT (confirmed statechain coin, holds 600) --S1 split--> [recv 100 @ wlt_2, change 500 @ issuer]
//!   wlt_2's un-broadcast 100 sub-coin       --S2 split--> [wlt_3 50, change 50 @ issuer]
//!
//!   POSITIVE: wlt_3.validate_offchain_chain(S2.consignment, [S1.txid, S2.txid]) => valid == true
//!             (both un-broadcast ancestor witnesses supplied — the resolver can walk the branch).
//!   NEGATIVE: wlt_3.validate_offchain_chain(S2.consignment, [S2.txid] ONLY) => valid == false AND
//!             detail == Some(...) (the S1 ancestor witness is missing / cannot be resolved).
//!
//! This is the mirror image of rgb03's positive chain proof: same branch, but omitting the ancestor
//! txid must make the receiver refuse to book the coin.
//!
//! Run with RGB_E2E=12. Requires the regtest + Mercury (lockbox) stack.

use std::{env, fs, process::Command, str::FromStr, thread, time::Duration};

use anyhow::{anyhow, Result};
use electrum_client::ElectrumApi;
use mercury_rgb::RgbWallet;
use mercuryrustlib::{client_config::ClientConfig, Coin, CoinStatus};

use crate::{bitcoin_core, electrs};

const ELECTRUM_URL: &str = "127.0.0.1:50001";
const RGB_PROXY: &str = "rpc://127.0.0.1:3000/json-rpc";
const NETWORK: &str = "regtest";
const BLINDING: u64 = 121;
const ISSUED: u64 = 600;
const RECV_AMT: u64 = 100; // wlt_2 sub-coin out of S1
const CHANGE1_AMT: u64 = 500; // issuer change out of S1
const LEAF_AMT: u64 = 50; // wlt_3 sub-coin out of S2
const CHANGE2_AMT: u64 = 50; // issuer change out of S2
const COIN_SAT_ROOT: u32 = 90_000;
const RECV_SAT: u64 = 35_000;
const CHANGE1_SAT: u64 = 30_000; // S1 fee = 90_000 - 65_000 = 25_000
const LEAF_SAT: u64 = 18_000;
const CHANGE2_SAT: u64 = 12_000; // S2 fee = 35_000 - 30_000 = 5_000

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
        mercuryrustlib::deposit::get_deposit_bitcoin_address_single_use(cc, &wallet.name, &token_id, size_sat).await?;
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
    Ok(mercuryrustlib::deposit::get_deposit_bitcoin_address_single_use(cc, &wallet.name, &token_id, size_sat).await?)
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

/// Build a spendable Coin for an UN-BROADCAST output (patch utxo/amount onto the deposit-init record).
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
    let _ = fs::remove_dir_all("./rgb-data12");
    env::set_var("ML_NETWORK", "regtest");
    let cc = mercuryrustlib::client_config::load().await;

    let (mut issuer, contract) = tokio::task::block_in_place(|| setup("./rgb-data12/issuer", true, true))?;
    let contract = contract.unwrap();
    let (mut wlt_2, _) = tokio::task::block_in_place(|| setup("./rgb-data12/wlt_2", false, false))?;
    let (mut wlt_3, _) = tokio::task::block_in_place(|| setup("./rgb-data12/wlt_3", false, false))?;
    println!("RGB12 - issued {ISSUED} units of {contract}");

    // ---- Deposit ROOT (full 600); register it. ----
    let sources: Vec<String> = tokio::task::block_in_place(|| issuer.list_allocations(&contract))?
        .into_iter().map(|(op, _, _)| op).collect();
    let addr_root = deposit_coin(&cc, "rgb12_root", COIN_SAT_ROOT, |sc| {
        let (_t, _v, _c, signed) = tokio::task::block_in_place(|| {
            issuer.fund_statechain(sc, COIN_SAT_ROOT as u64, &contract, ISSUED, 2, BLINDING)
        })?;
        Ok(cc.electrum_client.transaction_broadcast_raw(&hex::decode(&signed)?)?.to_string())
    }).await?;
    let (txid_root, vout_root, mut coin_root) = coin_outpoint(&cc, "rgb12_root", &addr_root).await?;
    tokio::task::block_in_place(|| {
        issuer.register_statechain(&txid_root, vout_root, COIN_SAT_ROOT as u64, &contract, ISSUED, &sources)
    })?;
    println!("RGB12 - ROOT = {txid_root}:{vout_root} holds {}",
        tokio::task::block_in_place(|| issuer.settled_balance(&contract))?);
    let si = mercuryrustlib::utils::info_config(&cc).await?;

    // ---- S1: split ROOT -> recv(100 @ wlt_2) + change(500 @ issuer). UN-broadcast. ----
    // wlt_2 receives its 100 sub-coin on a fresh Mercury deposit address; the issuer takes 500 change.
    let dep_recv = open_deposit_address(&cc, "rgb12_recv", RECV_SAT as u32).await?;
    let dep_change1 = open_deposit_address(&cc, "rgb12_change1", CHANGE1_SAT as u32).await?;
    let s1 = mercuryrustlib::rgb::create_colored_split_tx(
        &cc, &issuer, &mut coin_root, &contract,
        &[(dep_recv.clone(), RECV_SAT, RECV_AMT), (dep_change1.clone(), CHANGE1_SAT, CHANGE1_AMT)],
        1, true, None, NETWORK, si.initlock, si.interval, BLINDING,
    ).await?;
    let (recv_vout, change1_vout) = (s1.output_vouts[0], s1.output_vouts[1]);
    println!("RGB12 - S1 split (un-broadcast) {} -> recv(100)@:{recv_vout} change(500)@:{change1_vout}", s1.txid);
    assert!(!is_outpoint_spent(&cc, &txid_root, vout_root), "ROOT still UNSPENT (S1 not broadcast)");
    assert!(!tx_exists(&cc, &s1.txid), "S1 must NOT be on chain yet");

    // Register the two un-broadcast S1 sub-coins (issuer holds the contract, co-signs the next split).
    tokio::task::block_in_place(|| {
        issuer.register_statechain(&s1.txid, recv_vout, RECV_SAT, &contract, RECV_AMT, &[format!("{txid_root}:{vout_root}")])
    })?;
    tokio::task::block_in_place(|| {
        issuer.register_statechain(&s1.txid, change1_vout, CHANGE1_SAT, &contract, CHANGE1_AMT, &[])
    })?;

    // ---- S2: split wlt_2's UN-broadcast 100 sub-coin -> leaf(50 @ wlt_3) + change(50 @ issuer). UN-broadcast. ----
    let leaf_id = tokio::task::block_in_place(|| wlt_3.witness_receive(LEAF_AMT))?;
    let leaf_addr = tokio::task::block_in_place(|| wlt_3.address_from_recipient_id(&leaf_id))?;
    let change2_id = tokio::task::block_in_place(|| issuer.witness_receive(CHANGE2_AMT))?;
    let change2_addr = tokio::task::block_in_place(|| issuer.address_from_recipient_id(&change2_id))?;
    let mut coin_recv = unbroadcast_subcoin(&cc, "rgb12_recv", &dep_recv, &s1.txid, recv_vout, RECV_SAT).await?;
    let s2 = mercuryrustlib::rgb::create_colored_split_tx(
        &cc, &issuer, &mut coin_recv, &contract,
        &[(leaf_addr.clone(), LEAF_SAT, LEAF_AMT), (change2_addr.clone(), CHANGE2_SAT, CHANGE2_AMT)],
        1, true, None, NETWORK, si.initlock, si.interval, BLINDING,
    ).await?;
    let leaf_vout = s2.output_vouts[0];
    println!("RGB12 - S2 split (un-broadcast) {} : recv(100) -> leaf(50)@:{leaf_vout} + change(50)", s2.txid);

    // Both chain levels must be off-chain at validation time.
    assert!(!tx_exists(&cc, &s1.txid) && !tx_exists(&cc, &s2.txid),
        "BOTH S1 and S2 must be un-broadcast at validation time");

    // ---- POSITIVE: validate the leaf against the FULL ancestor chain [S1, S2]. ----
    let (valid_ok, detail_ok) = tokio::task::block_in_place(|| {
        wlt_3.validate_offchain_chain(&s2.consignment, &[s1.txid.clone(), s2.txid.clone()])
    })?;
    println!("RGB12 - [POSITIVE] validate_offchain_chain([S1, S2]) -> valid={valid_ok} detail={:?}", detail_ok);
    assert!(valid_ok, "receiver MUST validate the leaf when BOTH un-broadcast ancestor txids are supplied");

    // ---- NEGATIVE (the point of this test): omit the S1 ancestor txid, supply only [S2]. ----
    // The S1 witness that funds the S2 input is now unresolvable, so the receiver must REJECT.
    let (valid_bad, detail_bad) = tokio::task::block_in_place(|| {
        wlt_3.validate_offchain_chain(&s2.consignment, &[s2.txid.clone()])
    })?;
    println!("RGB12 - [NEGATIVE] validate_offchain_chain([S2] only, S1 omitted) -> valid={valid_bad} detail={:?}", detail_bad);
    assert!(!valid_bad,
        "receiver MUST REJECT the leaf when the S1 ancestor witness txid is omitted (unresolved ancestor)");
    assert!(detail_bad.is_some(),
        "a rejection must carry a detail explaining the missing/unresolved ancestor witness");

    crate::rgb_dump::dump("wlt_3 (never booked the rejected coin)", &mut wlt_3, &contract);

    println!("RGB12 - SUCCESS: the off-chain resolver ACCEPTS the leaf against the full ancestor chain [S1, S2] \
but REJECTS it (valid=false, detail={:?}) when the S1 ancestor witness txid is omitted. A receiver will not \
book a coin whose ancestor witness is unaccounted for.", detail_bad);
    Ok(())
}
