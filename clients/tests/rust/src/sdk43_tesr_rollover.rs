//! E2E (SDK_E2E=43) — **TES-R off-chain rollover ("off-chain forever")** on the live SE + real
//! bitcoind.
//!
//! Renewal replaces an extension horizontally but the extension-CSV budget is finite. At exhaustion,
//! TES-R does NOT touch the chain: it ROLLS OVER off-chain — the current level's state becomes a
//! self-split paying the aggregate A, and a fresh level (extension + owner state) hangs off it. This
//! gives unbounded off-chain state transitions with zero on-chain bytes (PROTOCOL.md §5.6), the
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
// [S3] These MUST sit on the regtest TesrParams grid (d0=24, delta=6, d_floor=6, e0=12, delta_e=3,
// e_floor=3), because rollover/presign decrement the STATE CSV by `params.delta` and floor at
// `params.d_floor` — an off-grid state CSV either underflows the floor or produces an equal-CSV twin.
// The state CSV entering the rollover must be >= d_floor + delta = 12 so the self-split can drop one
// rung (to 6) and still clear the floor.
const CSV_E: u16 = 6; // on the e-grid, > CSV_E_RENEWED
const CSV_E_RENEWED: u16 = 3; // e_floor
const CSV_D: u16 = 12; // d_floor + delta, so rollover's self-split lands exactly on d_floor (6)

/// The SE's cumulative co-sign counter for this node — the authoritative `num_sigs` verify_bundle checks.
async fn num_sigs(cc: &mercuryrustlib::client_config::ClientConfig, sid: &str) -> Result<u32> {
    Ok(mercuryrustlib::utils::get_statechain_info(sid, cc)
        .await?
        .ok_or(anyhow!("no statechain_info"))?
        .num_sigs)
}

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
    // Baseline: the deposit's co-sign count, before any tier is established — the signed-once backup
    // transactions conveyed with the coin. This is flat_backups.
    let baseline = num_sigs(&cc, &sid).await?;
    let mut b = mercuryrustlib::tesr::establish(&cc, &mut coin, &owner_exit, CSV_E, CSV_D, FEE_RATE, NETWORK).await?;

    // ---- 2. Renew (level 0), then ROLL OVER off-chain to level 1. ----
    mercuryrustlib::tesr::renew(&cc, &mut coin, &mut b, CSV_E_RENEWED, CSV_D).await?;
    assert_eq!(b.level(), 0, "still level 0 after renewal");
    mercuryrustlib::tesr::rollover(&cc, &mut coin, &mut b, CSV_E, CSV_D).await?;
    assert_eq!(b.level(), 1, "rollover added a depth level");
    assert_eq!(b.levels.len(), 2, "two levels now");
    println!("SDK43 - rolled over off-chain to level {} (0 on-chain bytes); {} tiers in exit chain", b.level(), b.exit_tiers().len());

    // [S3] The rollover self-split must SUPERSEDE the old owner state at a strictly LOWER CSV, or it is
    // an equal-CSV twin that verify_bundle's per-prevout race check rejects. Before the builder fix this
    // assertion FAILED (every rolled-over coin was unverifiable — and rollover is mandatory at m_max, so
    // this was the terminal state of every long-lived coin). It is the S3 regression guard.
    let ns_roll = num_sigs(&cc, &sid).await?;
    mercuryrustlib::tesr::verify_bundle(&b, ns_roll, baseline)
        .map_err(|e| anyhow!("verify_bundle REJECTED a rolled-over bundle (S3 regression): {e}"))?;
    println!("SDK43 - ✓ verify_bundle ACCEPTS the rolled-over bundle at SE count {ns_roll} (S3 fixed)");

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

    // The reloaded, twice-renewed, rolled-over bundle must STILL verify against the live SE count.
    let ns_final = num_sigs(&cc, &sid).await?;
    mercuryrustlib::tesr::verify_bundle(&r, ns_final, baseline)
        .map_err(|e| anyhow!("verify_bundle REJECTED the reloaded deep bundle (S3 regression): {e}"))?;
    println!("SDK43 - ✓ verify_bundle ACCEPTS the reloaded renew→rollover→renew bundle at SE count {ns_final}");

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
