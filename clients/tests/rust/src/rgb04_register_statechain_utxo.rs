//! E2E: register statechain UTXOs as on-chain colorable UTXOs (rgb-lib parity, design-doc item 1).
//!
//! Proves - with STANDARD rgb-lib methods - that a Mercury statechain UTXO (a MuSig2 aggregate
//! output that BDK does not own and the indexer's wallet-sync cannot see) can be treated exactly
//! like an on-chain colorable UTXO:
//!
//!  SENDER side: color-deposit the asset onto statechain UTXO A, then `register_statechain(A)`:
//!    - `get_asset_balance` now reads 1000 (the color deposit alone left it at 0 - registration is
//!      what surfaces the stash allocation through the standard DB-backed accounting), and
//!    - `list_unspents` shows A as a colorable, existing UTXO carrying the 1000 allocation.
//!
//!  RECEIVER side: onboard a *free* statechain UTXO B, `register_statechain(B, 0)`, then the standard
//!    `blind_receive` picks B as the seal - i.e. the receiver's rgb invoice references its statechain
//!    UTXO (B shows `pending_blinded`). This is the user's "receiver creates an rgb invoice whose
//!    recipient_id references its statechain UTXO".
//!
//! Run with RGB_E2E=4. Requires the regtest + Mercury (lockbox) stack.

use std::{env, fs, process::Command, thread, time::Duration};

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
const COIN_SAT_A: u32 = 40_000;
const COIN_SAT_B: u32 = 30_000;

async fn wait_for_address(cc: &ClientConfig, address: &str, amount: u32) -> Result<()> {
    for _ in 0..60 {
        if electrs::check_address(cc, address, amount).await? {
            return Ok(());
        }
        thread::sleep(Duration::from_secs(1));
    }
    Err(anyhow!("address {address} not indexed in time"))
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

/// Deposit (Mercury) a statechain coin to `wallet_name` of `size_sat`, confirm it, and return its
/// (aggregated_address, utxo_txid, utxo_vout). `fund` must broadcast the funding tx itself and
/// return its txid (the issuer colors+broadcasts; the receiver pays plain BTC via bitcoind).
async fn deposit_coin<F>(
    cc: &ClientConfig,
    wallet_name: &str,
    size_sat: u32,
    fund: F,
) -> Result<(String, String, u32)>
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
    let coin = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, &wallet.name)
        .await?
        .coins
        .iter()
        .find(|c| c.aggregated_address.as_deref() == Some(sc_address.as_str()))
        .ok_or(anyhow!("deposited coin not found"))?
        .clone();
    assert!(coin.status == CoinStatus::CONFIRMED, "statechain coin must confirm");
    let txid = coin.utxo_txid.ok_or(anyhow!("coin has no utxo_txid"))?;
    let vout = coin.utxo_vout.ok_or(anyhow!("coin has no utxo_vout"))?;
    Ok((sc_address, txid, vout))
}

pub async fn execute() -> Result<()> {
    let _ = Command::new("rm").arg("wallet.db").arg("wallet.db-shm").arg("wallet.db-wal").output();
    let _ = fs::remove_dir_all("./rgb-data4");
    env::set_var("ML_NETWORK", "regtest");
    let cc = mercuryrustlib::client_config::load().await;

    let (mut issuer, contract) = tokio::task::block_in_place(|| setup("./rgb-data4/issuer", true, true))?;
    let contract = contract.unwrap();
    // Receiver: no issuance and NO on-chain colorable UTXOs, so its statechain UTXO B is the only
    // colorable coin -> blind_receive is forced to pick B (cleanly proving the seal is statechain).
    let (mut receiver, _) = tokio::task::block_in_place(|| setup("./rgb-data4/receiver", false, false))?;
    println!("RGB04 - issued {ISSUED} units of {contract}");

    // ============================ SENDER SIDE ============================
    // Capture the on-chain UTXO(s) currently holding the issuance - the color deposit spends them
    // on-chain but bypasses DB accounting, so we hand them to register_statechain to be cleared
    // (otherwise the allocation would be double-counted: stale source + registered A).
    let sources: Vec<String> = tokio::task::block_in_place(|| issuer.list_allocations(&contract))?
        .into_iter()
        .map(|(op, _, _)| op)
        .collect();
    println!("RGB04 - issuer source allocation(s) before deposit: {sources:?}");

    // Color-deposit the full asset onto statechain UTXO A.
    let (_addr_a, txid_a, vout_a) = deposit_coin(&cc, "rgb04_sender", COIN_SAT_A, |sc| {
        let (_t, _v, _c, signed) = tokio::task::block_in_place(|| {
            issuer.fund_statechain(sc, COIN_SAT_A as u64, &contract, ISSUED, 2, BLINDING)
        })?;
        let txid = cc.electrum_client.transaction_broadcast_raw(&hex::decode(&signed)?)?;
        Ok(txid.to_string())
    })
    .await?;
    println!("RGB04 - color-deposited {ISSUED} onto statechain UTXO A = {txid_a}:{vout_a}");

    // Before registration: the color deposit spent the on-chain source UTXO and moved the asset onto
    // A in the rgb *stash*, but the DB-backed accounting still shows the stale allocation on the
    // (now spent) on-chain source - it is NOT on A yet.
    let bal_before = tokio::task::block_in_place(|| balance_or_zero(&issuer, &contract));
    let allocs_before = tokio::task::block_in_place(|| issuer.list_allocations(&contract))?;
    println!("RGB04 - [standard rgb-lib] issuer get_asset_balance BEFORE register = {bal_before}");
    for (op, amt, settled) in &allocs_before {
        println!("RGB04 - [standard rgb-lib] BEFORE register allocation: {op} amount={amt} settled={settled}");
    }
    assert!(
        !allocs_before.iter().any(|(op, ..)| op == &format!("{txid_a}:{vout_a}")),
        "before register, standard methods do not yet see the statechain UTXO A"
    );

    // Register A as a wallet-owned colorable UTXO (and clear the consumed on-chain source allocation).
    let _ = tokio::task::block_in_place(|| {
        issuer.register_statechain(&txid_a, vout_a, COIN_SAT_A as u64, &contract, ISSUED, &sources)
    })?;

    // After registration: standard methods treat A as an on-chain colorable UTXO holding the asset,
    // and the balance is correct (moved onto A, not double-counted).
    let bal_after = tokio::task::block_in_place(|| issuer.settled_balance(&contract))?;
    println!("RGB04 - [standard rgb-lib] issuer get_asset_balance AFTER register = {bal_after}");
    assert_eq!(bal_after, ISSUED, "get_asset_balance must reflect the statechain allocation after register (no double-count)");

    let allocs = tokio::task::block_in_place(|| issuer.list_allocations(&contract))?;
    for (op, amt, settled) in &allocs {
        println!("RGB04 - [standard rgb-lib] issuer list_unspents allocation: {op} amount={amt} settled={settled}");
    }
    assert!(
        allocs.iter().any(|(op, amt, settled)| op == &format!("{txid_a}:{vout_a}") && *amt == ISSUED && *settled),
        "list_unspents/list_allocations must show the {ISSUED} allocation settled on statechain UTXO A"
    );
    let dump = tokio::task::block_in_place(|| issuer.unspents_dump())?;
    let a_entry = dump.iter().find(|(op, ..)| op == &format!("{txid_a}:{vout_a}"));
    let (_, a_sat, a_color, a_exists, a_nalloc, _) = a_entry.ok_or(anyhow!("A not in list_unspents"))?;
    println!("RGB04 - [standard rgb-lib] issuer list_unspents A: sats={a_sat} colorable={a_color} exists={a_exists} n_allocations={a_nalloc}");
    assert!(*a_color && *a_exists && *a_nalloc == 1, "statechain UTXO A must be a colorable existing UTXO with 1 allocation");

    // ============================ RECEIVER SIDE ============================
    // Onboard the receiver with a *free* statechain UTXO B (plain BTC deposit, no asset).
    let (_addr_b, txid_b, vout_b) = deposit_coin(&cc, "rgb04_receiver", COIN_SAT_B, |sc| {
        Ok(bitcoin_core::sendtoaddress(COIN_SAT_B, sc)?)
    })
    .await?;
    println!("RGB04 - onboarded receiver: free statechain UTXO B = {txid_b}:{vout_b}");
    let _ = tokio::task::block_in_place(|| {
        receiver.register_statechain(&txid_b, vout_b, COIN_SAT_B as u64, &contract, 0, &[])
    })?;

    // Standard blind_receive: the only colorable coin is B, so the invoice's seal IS the statechain UTXO.
    let recipient_id = tokio::task::block_in_place(|| receiver.blind_receive(None, RECEIVE_AMT))?;
    println!("RGB04 - receiver standard blind_receive -> recipient_id (blinded seal) {recipient_id}");

    let rdump = tokio::task::block_in_place(|| receiver.unspents_dump())?;
    for (op, sat, color, exists, nalloc, pending) in &rdump {
        println!("RGB04 - [standard rgb-lib] receiver list_unspents: {op} sats={sat} colorable={color} exists={exists} n_allocations={nalloc} pending_blinded={pending}");
    }
    let b_entry = rdump.iter().find(|(op, ..)| op == &format!("{txid_b}:{vout_b}"));
    let (_, _, b_color, b_exists, _, b_pending) = b_entry.ok_or(anyhow!("B not in list_unspents"))?;
    assert!(*b_color && *b_exists, "statechain UTXO B must be a colorable existing UTXO");
    assert!(*b_pending >= 1, "blind_receive must have reserved the statechain UTXO B as the invoice seal");

    println!("RGB04 - SUCCESS: rgb-lib treats statechain UTXOs as on-chain colorable UTXOs - get_asset_balance/list_unspents surface A's allocation, and blind_receive's invoice seal is the statechain UTXO B.");
    Ok(())
}

fn balance_or_zero(rgb: &RgbWallet, contract: &str) -> u64 {
    rgb.settled_balance(contract).unwrap_or(0)
}
