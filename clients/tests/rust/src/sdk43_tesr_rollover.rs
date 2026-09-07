//! E2E (SDK_E2E=43) — **TES-R off-chain rollover ("off-chain forever")** on the live SE + real
//! bitcoind.
//!
//! Renewal replaces an extension horizontally but the extension-CSV budget is finite. At exhaustion,
//! TES-R does NOT touch the chain: it ROLLS OVER off-chain — the current level's state becomes a
//! self-split paying the aggregate A, and a fresh level (extension + owner state) hangs off it. This
//! gives unbounded off-chain state transitions with zero on-chain bytes (PROTOCOL.md §5.6), the
//! "users can be off-chain forever" property.
//!
//! The ladder under test is the one the DEPOSIT co-signed at first mempool sight of F (T, X_0, S_0
//! on the regtest schedule) — LOADED, never re-established. There is no flat `tx1`: the enclave count
//! after deposit is exactly 3 and the census `se_num_sigs == tiers + superseded` runs with the flat
//! term 0. Every step below is checked against the live SE count with that flat term, and the count
//! must grow by EXACTLY the tiers the step discloses — nothing flat is ever co-signed:
//!
//!   1. Deposit F; load the level-0 ladder. `num_sigs == 3`; verifies at `(3, 0)`, refused at `(3, 1)`.
//!   2. Renew off-chain (level 0) on the production cadence (`renew_auto`): +2 (X_1, S). Then ROLL
//!      OVER off-chain (`rollover_auto`) → level 1: +3 (self-split, X, S). The self-split sits one
//!      rung BELOW the state it supersedes ([S3]); the rolled-over bundle verifies.
//!   3. Renew again AT level 1 (+2) — proving renewal keeps working after a rollover.
//!   4. Persist, RELOAD from disk (2 levels), then unilaterally exit through the WHOLE deep chain
//!      T → X0 → S0(self-split) → X1 → S1(owner). Funds land at the coin's backup address; F
//!      untouched until exit.
//!
//! Run with SDK_E2E=43 (needs the regtest + Mercury lockbox stack, Core 28+).

use std::{env, fs, process::Command};

use anyhow::{anyhow, Result};
use mercuryrustlib::tesr::verify_bundle;

use crate::sdk40_tesr_consensus::{
    broadcast, deposit_coin, is_outpoint_spent, mine, se_num_sigs, tx_exists, wait_for_address,
};

pub async fn execute() -> Result<()> {
    let _ = Command::new("rm").arg("wallet.db").arg("wallet.db-shm").arg("wallet.db-wal").output();
    let _ = fs::remove_dir_all("./rgb-data-sdk43");
    env::set_var("ML_NETWORK", "regtest");
    let cc = mercuryrustlib::client_config::load().await;
    let wallet = "sdk43_alice";

    // ---- 1. Deposit; LOAD the ladder the deposit established (3 co-signs, flat term 0). ----
    let mut coin = deposit_coin(&cc, wallet).await?;
    let sid = coin.statechain_id.clone().ok_or(anyhow!("no statechain_id"))?;
    let f_txid = coin.utxo_txid.clone().ok_or(anyhow!("no F txid"))?;
    let f_vout = coin.utxo_vout.ok_or(anyhow!("no F vout"))?;
    let mut b = mercuryrustlib::tesr::load(&cc, wallet, &sid)
        .await?
        .ok_or(anyhow!("the deposit must have been laddered at first sight — it has no other exit material"))?;
    let p = b.params;
    // Baseline: the deposit's co-sign count. Under the rule it is the THREE TIERS and nothing else —
    // the flat term of the census is zero, and a verifier that still budgeted a deposit tx1 would
    // refuse every honest coin (or launder one hidden state per coin).
    let baseline = se_num_sigs(&cc, &sid).await?;
    assert_eq!(baseline, 3, "a deposited coin's enclave count is exactly its three tiers (T, X_0, S_0) — no tx1");
    assert_eq!(b.exit_tiers().len(), 3, "level 0 is T -> X_0 -> S_0");
    assert_eq!(b.current().state.csv, Some(p.state_csv(0)), "S_0 is at the schedule's D0");
    verify_bundle(&b, baseline, 0).map_err(|e| anyhow!("the deposit ladder must pass the census with flat term 0: {e}"))?;
    assert!(verify_bundle(&b, baseline, 1).is_err(), "the retired flat term (one deposit tx1) must NOT balance: 3 != 1 + 3");
    assert_eq!(b.owner_exit_address, coin.backup_address, "the deposit ladder exits to the coin's own backup address");

    // ---- 2. Renew (level 0), then ROLL OVER off-chain to level 1 — production cadence. ----
    let _ = mercuryrustlib::tesr::renew_auto(&cc, &mut coin, &mut b).await?;
    assert_eq!(b.level(), 0, "still level 0 after renewal");
    assert_eq!(b.m, 1, "renewal counter advanced");
    let ns_renew = se_num_sigs(&cc, &sid).await?;
    assert_eq!(ns_renew, baseline + 2, "a renewal co-signs exactly X_1 and S — the count grows by the tiers disclosed, nothing flat");
    verify_bundle(&b, ns_renew, 0).map_err(|e| anyhow!("verify_bundle REJECTED the renewed bundle: {e}"))?;
    let state_csv_before_rollover = b.current().state.csv.ok_or(anyhow!("state has no CSV"))?;

    mercuryrustlib::tesr::rollover_auto(&cc, &mut coin, &mut b).await?;
    assert_eq!(b.level(), 1, "rollover added a depth level");
    assert_eq!(b.levels.len(), 2, "two levels now");
    assert_eq!(b.m, 0, "fresh renewal budget at the new level");
    let ns_roll = se_num_sigs(&cc, &sid).await?;
    assert_eq!(ns_roll, ns_renew + 3, "a rollover co-signs exactly the self-split, the new X and the new S");
    println!("SDK43 - rolled over off-chain to level {} (0 on-chain bytes); {} tiers in exit chain; num_sigs {baseline} -> {ns_renew} -> {ns_roll}", b.level(), b.exit_tiers().len());

    // [S3] The rollover self-split must SUPERSEDE the old owner state at a strictly LOWER CSV, or it is
    // an equal-CSV twin that verify_bundle's per-prevout race check rejects. Before the builder fix this
    // FAILED (every rolled-over coin was unverifiable — and rollover is mandatory at m_max, so this was
    // the terminal state of every long-lived coin). It is the S3 regression guard.
    assert_eq!(
        b.levels[0].state.csv,
        Some(state_csv_before_rollover - p.delta),
        "[S3] the self-split sits exactly one rung (δ) BELOW the owner state it supersedes"
    );
    verify_bundle(&b, ns_roll, 0)
        .map_err(|e| anyhow!("verify_bundle REJECTED a rolled-over bundle (S3 regression): {e}"))?;
    println!("SDK43 - ✓ verify_bundle ACCEPTS the rolled-over bundle at SE count {ns_roll}, flat term 0 (S3 fixed)");

    // ---- 3. Renew AGAIN at the new level (proves renewal survives rollover). ----
    let _ = mercuryrustlib::tesr::renew_auto(&cc, &mut coin, &mut b).await?;
    assert_eq!(b.m, 1, "renewal counter advanced at the new level");
    let ns_final = se_num_sigs(&cc, &sid).await?;
    assert_eq!(ns_final, ns_roll + 2, "the level-1 renewal co-signs exactly X and S");
    mercuryrustlib::tesr::persist(&cc, wallet, &b).await?;
    println!("SDK43 - renewed again at level 1 (m={}); persisted; num_sigs={ns_final}", b.m);

    // ---- 4. RELOAD from disk and exit through the whole deep chain. ----
    drop(b);
    let r = mercuryrustlib::tesr::load(&cc, wallet, &sid).await?.ok_or(anyhow!("bundle did not persist"))?;
    assert_eq!(r.levels.len(), 2, "reloaded bundle has both levels");
    assert!(!is_outpoint_spent(&cc, &f_txid, f_vout), "F still UNSPENT — nothing aged across renew+rollover+renew");
    assert!(coin.locktime.is_none(), "no absolute calendar at any point of the coin's life");

    // The reloaded, twice-renewed, rolled-over bundle must STILL verify against the live SE count,
    // and the census must be EXACTLY tiers + superseded: every co-sign the SE ever issued for this
    // coin is a disclosed tier.
    assert_eq!(
        ns_final,
        r.exit_tiers().len() as u32 + r.superseded_states.len() as u32 + r.superseded_extensions.len() as u32,
        "every SE co-sign is a disclosed tier — the census has no flat term"
    );
    verify_bundle(&r, ns_final, 0)
        .map_err(|e| anyhow!("verify_bundle REJECTED the reloaded deep bundle (S3 regression): {e}"))?;
    assert!(verify_bundle(&r, ns_final, 1).is_err(), "the retired flat term must not balance the deep bundle either");
    println!("SDK43 - ✓ verify_bundle ACCEPTS the reloaded renew→rollover→renew bundle at SE count {ns_final}, flat term 0");

    // Owned copies of the exit chain (trigger + each level's extension/state, in order).
    let chain: Vec<(String, String, Option<u16>)> =
        r.exit_tiers().iter().map(|t| (t.txid.clone(), t.signed_tx.clone(), t.csv)).collect();
    let final_state = r.current().state.clone();

    // Broadcast the trigger, then each tier after mining its CSV blocks (which confirms its parent).
    let _ = broadcast(&cc, &chain[0].1)?;
    for (_txid, signed, csv) in &chain[1..] {
        let _ = mine(csv.ok_or(anyhow!("a non-trigger tier has no CSV"))? as u32);
        let _ = broadcast(&cc, signed)?;
    }
    let _ = mine(1)?;
    assert!(tx_exists(&cc, &final_state.txid), "final owner state confirms");
    assert!(is_outpoint_spent(&cc, &f_txid, f_vout), "F consumed by the deep exit");
    assert!(
        wait_for_address(&cc, &r.owner_exit_address, final_state.out_value as u32).await.is_ok(),
        "owner funded through the deep chain at the coin's backup address"
    );
    println!("SDK43 - ✓ PASS: unbounded off-chain life — renew→rollover→renew, then a {}-tier unilateral exit reached the owner ({} sat)", chain.len(), final_state.out_value);
    Ok(())
}
