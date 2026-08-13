//! E2E (SDK_E2E=42) — **TES-R wallet-level lifecycle + persistence** on the live SE + real bitcoind.
//!
//! Proves a wallet can HOLD a V2 coin across sessions and manage its whole off-chain life through
//! library calls (not test glue): establish the ladder, renew it off-chain, persist it to the wallet
//! DB, RELOAD it as a fresh session would, and unilaterally exit from the reloaded bundle.
//!
//! Flow:
//!   1. Deposit F; `tesr::establish` builds + blind-co-signs T → X_0 → S_0; `tesr::persist` writes the
//!      bundle to the wallet DB (`tesr-<id>` key).
//!   2. `tesr::renew` co-signs a lower-CSV X_1 + fresh state OFF-CHAIN (zero bytes); persist again.
//!   3. `tesr::load` re-reads the bundle from disk (simulating a fresh wallet load) — asserts it
//!      round-trips and reflects the renewal (m=1, lower extension CSV).
//!   4. Exit purely from the RELOADED bundle: broadcast trigger → extension → state honoring each
//!      tier's CSV; funds land at the owner. Nothing aged while the coin sat un-broadcast.
//!
//! Run with SDK_E2E=42 (needs the regtest + Mercury lockbox stack, Core 28+).

use std::{env, fs, process::Command};

use anyhow::{anyhow, Result};

use crate::bitcoin_core;
use crate::sdk40_tesr_consensus::{broadcast, deposit_coin, is_outpoint_spent, mine, tx_exists, wait_for_address};

const NETWORK: &str = "regtest";
/// **THIS FIXTURE'S rate, not the protocol's.** The shipped committed rate is 3.0 since [D44]
/// (`TesrParams::for_network`). This test builds its own tiers at 2.0 and checks them against 2.0,
/// which is self-consistent and fine — but anything that must agree with a ladder the SDK built
/// must read the rate from the preset instead (see `sdk58`/`sdk70`, which broke by mixing the two).
const FEE_RATE: f64 = 2.0;
const CSV_E: u16 = 6; // initial extension CSV
const CSV_E_RENEWED: u16 = 3; // renewed extension CSV (strictly lower)
const CSV_D: u16 = 4; // state CSV

pub async fn execute() -> Result<()> {
    let _ = Command::new("rm").arg("wallet.db").arg("wallet.db-shm").arg("wallet.db-wal").output();
    let _ = fs::remove_dir_all("./rgb-data-sdk42");
    env::set_var("ML_NETWORK", "regtest");
    let cc = mercuryrustlib::client_config::load().await;
    let wallet = "sdk42_alice";

    // ---- 1. Deposit F; establish + persist the TES-R ladder. ----
    let mut coin = deposit_coin(&cc, wallet).await?;
    let sid = coin.statechain_id.clone().ok_or(anyhow!("no statechain_id"))?;
    let f_txid = coin.utxo_txid.clone().ok_or(anyhow!("no F txid"))?;
    let f_vout = coin.utxo_vout.ok_or(anyhow!("no F vout"))?;
    let owner_exit = bitcoin_core::getnewaddress()?;

    let mut bundle =
        mercuryrustlib::tesr::establish(&cc, &mut coin, &owner_exit, CSV_E, CSV_D, FEE_RATE, NETWORK).await?;
    mercuryrustlib::tesr::persist(&cc, wallet, &bundle).await?;
    println!("SDK42 - established + persisted ladder for {sid}: T={} X={}(csv {CSV_E})", bundle.trigger.txid, bundle.current().extension.txid);

    // ---- 2. Off-chain renewal (lower-CSV extension), persist the updated bundle. ----
    mercuryrustlib::tesr::renew(&cc, &mut coin, &mut bundle, CSV_E_RENEWED, CSV_D).await?;
    mercuryrustlib::tesr::persist(&cc, wallet, &bundle).await?;
    let renewed_ext_txid = bundle.current().extension.txid.clone();
    println!("SDK42 - renewed off-chain (0 on-chain bytes): m={}, new X={}(csv {CSV_E_RENEWED})", bundle.m, renewed_ext_txid);

    // ---- 3. RELOAD from disk (as a fresh wallet session) and verify it round-trips. ----
    drop(bundle);
    let reloaded = mercuryrustlib::tesr::load(&cc, wallet, &sid)
        .await?
        .ok_or(anyhow!("bundle did not persist"))?;
    assert_eq!(reloaded.m, 1, "reloaded bundle reflects the renewal");
    assert_eq!(reloaded.current().extension.csv, Some(CSV_E_RENEWED), "reloaded extension has the renewed CSV");
    assert_eq!(reloaded.current().extension.txid, renewed_ext_txid, "reloaded extension is the renewed one");
    assert_eq!(reloaded.statechain_id, sid, "bundle keyed to the coin");
    println!("SDK42 - ✓ bundle reloaded from DB intact (m={}, extension csv {:?})", reloaded.m, reloaded.current().extension.csv);

    // Un-broadcast immunity: the coin sat with a full renewed ladder, F never touched.
    assert!(!is_outpoint_spent(&cc, &f_txid, f_vout), "F still UNSPENT before exit — nothing aged");

    // ---- 4. Unilateral exit purely from the RELOADED bundle (trigger → extension → state). ----
    let exit_state = reloaded.current().state.clone();
    let ext_csv = reloaded.current().extension.csv.unwrap();
    let state_csv = reloaded.current().state.csv.unwrap();
    let _ = broadcast(&cc, &reloaded.trigger.signed_tx)?;
    let _ = mine(ext_csv as u32); // trigger confirmed + ext_csv confs
    let _ = broadcast(&cc, &reloaded.current().extension.signed_tx)?;
    let _ = mine(state_csv as u32); // extension confirmed + state_csv confs
    let _ = broadcast(&cc, &exit_state.signed_tx)?;
    let _ = mine(1)?;
    assert!(tx_exists(&cc, &exit_state.txid), "state confirms");
    assert!(is_outpoint_spent(&cc, &f_txid, f_vout), "F consumed by the exit");
    assert!(wait_for_address(&cc, &owner_exit, exit_state.out_value as u32).await.is_ok(), "owner funded on exit");
    println!("SDK42 - ✓ PASS: persisted V2 coin renewed off-chain, reloaded from disk, and exited ({} sat to owner)", exit_state.out_value);
    Ok(())
}
