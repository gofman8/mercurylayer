//! E2E (SDK_E2E=43) — **TES-R off-chain rollover ("off-chain forever")** on the live SE + real
//! bitcoind.
//!
//! Renewal replaces an extension horizontally but the extension-CSV budget is finite. At exhaustion,
//! TES-R does NOT touch the chain: it ROLLS OVER off-chain — the current level's state becomes a
//! self-split paying the aggregate A, and a fresh level (extension + owner state) hangs off it. This
//! gives unbounded off-chain state transitions with zero on-chain bytes (V2-DESIGN §5.6), the
//! "users can be off-chain forever" property.
//!
//! Flow:
//!   1. Deposit F; establish the ladder (level 0).
//!   2. Renew off-chain (level 0). Then ROLL OVER off-chain → level 1 (deeper ladder, fresh budget).
//!   3. Renew again AT level 1 — proving renewal keeps working after a rollover.
//!   4. Persist, RELOAD from disk (2 levels), then unilaterally exit through the WHOLE deep chain
//!      T → X0 → S0(self-split) → X1 → S1(owner). Funds land; F untouched until exit.
//!
//! Run with SDK_E2E=43 (needs the regtest + Mercury lockbox stack, Core 28+).

use std::{env, fs, process::Command};

use anyhow::{anyhow, Result};

use crate::bitcoin_core;
use crate::sdk40_tesr_consensus::{broadcast, deposit_coin, is_outpoint_spent, mine, tx_exists, wait_for_address};

const NETWORK: &str = "regtest";
const FEE_RATE: f64 = 2.0;
const CSV_E: u16 = 4;
const CSV_E_RENEWED: u16 = 2;
const CSV_D: u16 = 3;

pub async fn execute() -> Result<()> {
    let _ = Command::new("rm").arg("wallet.db").arg("wallet.db-shm").arg("wallet.db-wal").output();
    let _ = fs::remove_dir_all("./rgb-data-sdk43");
    env::set_var("ML_NETWORK", "regtest");
    let cc = mercuryrustlib::client_config::load().await;
    let wallet = "sdk43_alice";

    // ---- 1. Deposit + establish. ----
    let mut coin = deposit_coin(&cc, wallet).await?;
    let sid = coin.statechain_id.clone().ok_or(anyhow!("no statechain_id"))?;
    let f_txid = coin.utxo_txid.clone().ok_or(anyhow!("no F txid"))?;
    let f_vout = coin.utxo_vout.ok_or(anyhow!("no F vout"))?;
    let owner_exit = bitcoin_core::getnewaddress()?;
    let mut b = mercuryrustlib::tesr::establish(&cc, &mut coin, &owner_exit, CSV_E, CSV_D, FEE_RATE, NETWORK).await?;

    // ---- 2. Renew (level 0), then ROLL OVER off-chain to level 1. ----
    mercuryrustlib::tesr::renew(&cc, &mut coin, &mut b, CSV_E_RENEWED, CSV_D).await?;
    assert_eq!(b.level(), 0, "still level 0 after renewal");
    mercuryrustlib::tesr::rollover(&cc, &mut coin, &mut b, CSV_E, CSV_D).await?;
    assert_eq!(b.level(), 1, "rollover added a depth level");
    assert_eq!(b.levels.len(), 2, "two levels now");
    println!("SDK43 - rolled over off-chain to level {} (0 on-chain bytes); {} tiers in exit chain", b.level(), b.exit_tiers().len());

    // ---- 3. Renew AGAIN at the new level (proves renewal survives rollover). ----
    mercuryrustlib::tesr::renew(&cc, &mut coin, &mut b, CSV_E_RENEWED, CSV_D).await?;
    assert_eq!(b.m, 1, "renewal counter advanced at the new level");
    mercuryrustlib::tesr::persist(&cc, wallet, &b).await?;
    println!("SDK43 - renewed again at level 1 (m={}); persisted", b.m);

    // ---- 4. RELOAD from disk and exit through the whole deep chain. ----
    drop(b);
    let r = mercuryrustlib::tesr::load(&cc, wallet, &sid).await?.ok_or(anyhow!("bundle did not persist"))?;
    assert_eq!(r.levels.len(), 2, "reloaded bundle has both levels");
    assert!(!is_outpoint_spent(&cc, &f_txid, f_vout), "F still UNSPENT — nothing aged across renew+rollover+renew");

    // Owned copies of the exit chain (trigger + each level's extension/state, in order).
    let chain: Vec<(String, String, Option<u16>)> =
        r.exit_tiers().iter().map(|t| (t.txid.clone(), t.signed_tx.clone(), t.csv)).collect();
    let final_state = r.current().state.clone();

    // Broadcast the trigger, then each tier after mining its CSV blocks (which confirms its parent).
    let _ = broadcast(&cc, &chain[0].1)?;
    for (_txid, signed, csv) in &chain[1..] {
        let _ = mine(csv.unwrap() as u32);
        let _ = broadcast(&cc, signed)?;
    }
    let _ = mine(1)?;
    assert!(tx_exists(&cc, &final_state.txid), "final owner state confirms");
    assert!(is_outpoint_spent(&cc, &f_txid, f_vout), "F consumed by the deep exit");
    assert!(wait_for_address(&cc, &owner_exit, final_state.out_value as u32).await.is_ok(), "owner funded through the deep chain");
    println!("SDK43 - ✓ PASS: unbounded off-chain life — renew→rollover→renew, then a {}-tier unilateral exit reached the owner ({} sat)", chain.len(), final_state.out_value);
    Ok(())
}
