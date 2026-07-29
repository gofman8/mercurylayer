//! E2E (SDK_E2E=44) — **TES-R self-driving cadence from the protocol schedule** on the live SE +
//! real bitcoind.
//!
//! Where sdk40-43 pass hand-picked CSVs, this drives the whole lifecycle from the canonical
//! `TesrParams` schedule (mainnet defaults in PROTOCOL.md §5.2; regtest preset here) via the
//! production entry points `establish_auto` / `renew_auto` / `rollover_auto`: the wallet renews at the
//! schedule's decrementing extension CSV until the renewal budget is spent, then rolls over — exactly
//! the cadence a real wallet runs — and still exits correctly against real consensus.
//!
//! Flow: deposit → establish_auto → renew_auto in a loop until it reports a rollover is due →
//! rollover_auto → persist → reload → unilateral exit through the whole chain. Proves the schedule
//! math (decrement + floor + m_max) yields a working, exitable ladder.
//!
//! Run with SDK_E2E=44 (needs the regtest + Mercury lockbox stack, Core 28+).

use std::{env, fs, process::Command};

use anyhow::{anyhow, Result};

use crate::bitcoin_core;
use crate::sdk40_tesr_consensus::{broadcast, deposit_coin, is_outpoint_spent, mine, tx_exists, wait_for_address};

const NETWORK: &str = "regtest";

pub async fn execute() -> Result<()> {
    let _ = Command::new("rm").arg("wallet.db").arg("wallet.db-shm").arg("wallet.db-wal").output();
    let _ = fs::remove_dir_all("./rgb-data-sdk44");
    env::set_var("ML_NETWORK", "regtest");
    let cc = mercuryrustlib::client_config::load().await;
    let wallet = "sdk44_alice";
    let p = mercurylib::tesr::TesrParams::regtest();

    let mut coin = deposit_coin(&cc, wallet).await?;
    let sid = coin.statechain_id.clone().ok_or(anyhow!("no statechain_id"))?;
    let f_txid = coin.utxo_txid.clone().ok_or(anyhow!("no F txid"))?;
    let f_vout = coin.utxo_vout.ok_or(anyhow!("no F vout"))?;
    let owner_exit = bitcoin_core::getnewaddress()?;

    // Establish at the schedule's initial CSVs.
    let mut b = mercuryrustlib::tesr::establish_auto(&cc, &mut coin, &owner_exit, NETWORK).await?;
    assert_eq!(b.current().extension.csv, Some(p.ext_csv(0)), "initial extension CSV = E0");
    assert_eq!(b.current().state.csv, Some(p.state_csv(0)), "initial state CSV = D0");
    println!("SDK44 - established at schedule E0={} D0={}", p.ext_csv(0), p.state_csv(0));

    // Renew at the decrementing cadence until the budget is spent (m_max), then roll over.
    let mut renewals = 0;
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
    println!("SDK44 - ✓ PASS: schedule-driven cadence (E0-m*δE, m_max rollover) yields a working, exitable ladder ({} sat to owner)", final_state.out_value);
    Ok(())
}
