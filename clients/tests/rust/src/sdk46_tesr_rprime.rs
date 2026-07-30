//! E2E (SDK_E2E=46) — **R′ verifier validated against the REAL SE sig count** on the live stack.
//!
//! The R′ soundness linchpin is `se_num_sigs == flat_backups + <tier count>`. This test proves the
//! count formula matches what the actual SE reports (not just a unit-test mock): it measures the
//! SE's `num_sigs` before and after establishing the ladder, asserts the SE increments by exactly
//! the number of tier co-signs, then confirms `verify_bundle` ACCEPTS the true count and REJECTS a
//! hidden extra signature. This retires the last fund-safety unknown before wiring R′ into the
//! transfer receiver.
//!
//! Run with SDK_E2E=46 (needs the regtest + Mercury lockbox stack, Core 28+).

use std::{env, fs, process::Command};

use anyhow::{anyhow, Result};

use crate::bitcoin_core;
use crate::sdk40_tesr_consensus::deposit_coin;

const NETWORK: &str = "regtest";

async fn num_sigs(cc: &mercuryrustlib::client_config::ClientConfig, sid: &str) -> Result<u32> {
    Ok(mercuryrustlib::utils::get_statechain_info(sid, cc)
        .await?
        .ok_or(anyhow!("no statechain_info"))?
        .num_sigs)
}

pub async fn execute() -> Result<()> {
    let _ = Command::new("rm").arg("wallet.db").arg("wallet.db-shm").arg("wallet.db-wal").output();
    let _ = fs::remove_dir_all("./rgb-data-sdk46");
    env::set_var("ML_NETWORK", "regtest");
    let cc = mercuryrustlib::client_config::load().await;

    let mut coin = deposit_coin(&cc, "sdk46_owner").await?;
    let sid = coin.statechain_id.clone().ok_or(anyhow!("no statechain_id"))?;
    let owner_exit = bitcoin_core::getnewaddress()?;

    // Measure the SE's finalized-signature count BEFORE establishing any tiers — this is the
    // `flat_backups` term: the signed-once backup transactions the deposit already co-signed.
    let baseline = num_sigs(&cc, &sid).await?;
    println!("SDK46 - SE num_sigs after deposit (flat_backups baseline): {baseline}");

    // Establish the ladder at the schedule (T + extension + state = 3 SE co-signs).
    let bundle = mercuryrustlib::tesr::establish_auto(&cc, &mut coin, &owner_exit, NETWORK).await?;
    let tier_count = bundle.exit_tiers().len() as u32;
    assert_eq!(tier_count, 3, "single-level ladder = trigger + extension + state");

    let after = num_sigs(&cc, &sid).await?;
    println!("SDK46 - SE num_sigs after establishing {tier_count} tiers: {after}");
    assert_eq!(after, baseline + tier_count, "SE incremented by EXACTLY the tier count — the R′ count formula holds against the real SE");

    // R′ verifier ACCEPTS the real ladder with the real count (flat_backups = the pre-tier baseline).
    mercuryrustlib::tesr::verify_bundle(&bundle, after, baseline)
        .map_err(|e| anyhow!("verify_bundle rejected a valid ladder: {e}"))?;
    println!("SDK46 - ✓ verify_bundle ACCEPTS the real ladder at the true SE count {after}");

    // And REJECTS a hidden extra co-signed state (num_sigs one higher than the ladder accounts for).
    assert!(
        mercuryrustlib::tesr::verify_bundle(&bundle, after + 1, baseline).is_err(),
        "verify_bundle MUST reject a hidden extra sig (double-spend defence)"
    );
    // ...and an undercount.
    assert!(mercuryrustlib::tesr::verify_bundle(&bundle, after - 1, baseline).is_err(), "must reject undercount");
    println!("SDK46 - ✓ verify_bundle REJECTS a hidden extra sig and an undercount");

    println!("SDK46 - ✓ PASS: R′ count formula validated against the live SE — hidden-state defence is real");
    Ok(())
}
