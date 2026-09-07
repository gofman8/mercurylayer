//! E2E (SDK_E2E=44) — **TES-R self-driving cadence from the protocol schedule** on the live SE +
//! real bitcoind.
//!
//! Drives the whole lifecycle from the canonical `TesrParams` schedule (mainnet defaults in
//! PROTOCOL.md §5.2; regtest preset here) via the production entry points: the DEPOSIT's own
//! first-sight establishment (`coin_status::check_deposit` → `establish_auto`, at E0/D0), then
//! `renew_auto` / `rollover_auto`. The wallet renews at the schedule's decrementing extension CSV
//! until the renewal budget is spent, then rolls over — exactly the cadence a real wallet runs — and
//! still exits correctly against real consensus.
//!
//! Flow: deposit (laddered at mempool sight, on the schedule) → LOAD the ladder → assert E0/D0 →
//! renew_auto in a loop until it reports a rollover is due → rollover_auto → persist → reload →
//! census (`num_sigs == tiers + superseded`, flat term 0) → unilateral exit through the whole chain.
//! Proves the schedule math (decrement + floor + m_max) yields a working, exitable ladder, and that
//! the ladder the deposit signs IS the schedule's.
//!
//! Run with SDK_E2E=44 (needs the regtest + Mercury lockbox stack, Core 28+).

use std::{env, fs, process::Command};

use anyhow::{anyhow, Result};

use crate::sdk40_tesr_consensus::{
    broadcast, deposit_coin, is_outpoint_spent, mine, se_num_sigs, tx_exists, wait_for_address,
};

const NETWORK: &str = "regtest";

pub async fn execute() -> Result<()> {
    let _ = Command::new("rm").arg("wallet.db").arg("wallet.db-shm").arg("wallet.db-wal").output();
    let _ = fs::remove_dir_all("./rgb-data-sdk44");
    env::set_var("ML_NETWORK", "regtest");
    let cc = mercuryrustlib::client_config::load().await;
    let wallet = "sdk44_alice";
    let p = mercurylib::tesr::TesrParams::for_network(NETWORK);

    let mut coin = deposit_coin(&cc, wallet).await?;
    let sid = coin.statechain_id.clone().ok_or(anyhow!("no statechain_id"))?;
    let f_txid = coin.utxo_txid.clone().ok_or(anyhow!("no F txid"))?;
    let f_vout = coin.utxo_vout.ok_or(anyhow!("no F vout"))?;

    // The deposit established the ladder at the schedule's initial CSVs. Load it; establish nothing.
    let mut b = mercuryrustlib::tesr::load(&cc, wallet, &sid)
        .await?
        .ok_or(anyhow!("the deposit did not persist a ladder for {sid}"))?;
    let owner_exit = b.owner_exit_address.clone();
    assert_eq!(b.current().extension.csv, Some(p.ext_csv(0)), "initial extension CSV = E0");
    assert_eq!(b.current().state.csv, Some(p.state_csv(0)), "initial state CSV = D0");
    assert_eq!(b.m, 0, "fresh renewal budget");
    assert_eq!(b.level(), 0, "a fresh deposit ladder is at level 0");
    println!("SDK44 - the deposit laddered at schedule E0={} D0={} (exit -> {owner_exit})", p.ext_csv(0), p.state_csv(0));

    // Renew at the decrementing cadence until the budget is spent (m_max), then roll over.
    let mut renewals = 0u32;
    loop {
        let rollover_due = mercuryrustlib::tesr::renew_auto(&cc, &mut coin, &mut b).await?;
        renewals += 1;
        assert_eq!(b.current().extension.csv, Some(p.ext_csv(b.m as u16)), "renewed extension CSV follows the schedule");
        println!("SDK44 - renew #{renewals}: m={}, extension CSV={} (rollover_due={})", b.m, p.ext_csv(b.m as u16), rollover_due);
        if rollover_due {
            break;
        }
    }
    assert_eq!(b.m as u16, p.m_max, "renewed exactly up to m_max before rollover");

    mercuryrustlib::tesr::rollover_auto(&cc, &mut coin, &mut b).await?;
    assert_eq!(b.level(), 1, "rolled over to a fresh level");
    assert_eq!(b.m, 0, "fresh renewal budget at the new level");
    mercuryrustlib::tesr::persist(&cc, wallet, &b).await?;
    println!("SDK44 - rolled over off-chain to level {} after {renewals} scheduled renewals", b.level());

    // Reload and exit through the whole chain.
    drop(b);
    let r = mercuryrustlib::tesr::load(&cc, wallet, &sid).await?.ok_or(anyhow!("bundle did not persist"))?;
    // The census across the whole cadence: 3 at deposit + 2 per renewal + 3 for the rollover, and
    // every one of them is a tier the bundle discloses — the flat term is 0 because no tx1 exists.
    let n = se_num_sigs(&cc, &sid).await?;
    assert_eq!(n, 3 + 2 * renewals + 3, "num_sigs == 3 (deposit) + 2 per renewal + 3 (rollover), no tx1");
    mercuryrustlib::tesr::verify_bundle(&r, n, 0)
        .map_err(|e| anyhow!("the schedule-driven bundle must pass the census with flat term 0: {e}"))?;
    assert!(!is_outpoint_spent(&cc, &f_txid, f_vout), "F untouched across the entire scheduled off-chain life");
    let chain: Vec<(String, Option<u16>)> = r.exit_tiers().iter().map(|t| (t.signed_tx.clone(), t.csv)).collect();
    let final_state = r.current().state.clone();

    let _ = broadcast(&cc, &chain[0].0)?;
    for (signed, csv) in &chain[1..] {
        let _ = mine(csv.unwrap() as u32);
        let _ = broadcast(&cc, signed)?;
    }
    let _ = mine(1)?;
    assert!(tx_exists(&cc, &final_state.txid), "final state confirms");
    assert!(is_outpoint_spent(&cc, &f_txid, f_vout), "F consumed by exit");
    assert!(wait_for_address(&cc, &owner_exit, final_state.out_value as u32).await.is_ok(), "owner funded");
    println!("SDK44 - ✓ PASS: schedule-driven cadence (deposit at E0/D0, E0-m*δE renewals, m_max rollover) yields a working, exitable ladder ({} sat to owner); census {n} == tiers + superseded", final_state.out_value);
    Ok(())
}
