//! E2E (SDK_E2E=53) — **the LN-latch guard is now LIFTED (HODL-latch pivot, LIGHTNING.md)**.
//!
//! History: this test used to prove `transfer_sender` REFUSED a Lightning-latched transfer of a
//! laddered (TES-R) coin, because the Model A co-sign of S' is not preimage-gated. The HODL-latch pivot
//! lifts that refusal: rob-SSP is now blocked by the SSP's pre-pay `verify_bundle` census
//! (peek_pending_transfers → ssp.rs execute_pay), and rob-USER rests on operator trust exactly as the
//! pre-TES-R design did. So a latched transfer of a laddered coin must now OPEN successfully (no
//! refusal). The full happy path (SSP censuses + pays a real BOLT11 with a laddered coin) is proved by
//! sdk63. This test just pins that the guard is gone and the latch opens.
//!
//! Run with SDK_E2E=53 (needs the regtest + Mercury lockbox stack, Core 28+).

use std::{env, fs};

use anyhow::{anyhow, Result};

use crate::sdk40_tesr_consensus::deposit_coin;

const NETWORK: &str = "regtest";

pub async fn execute() -> Result<()> {
    let _ = std::process::Command::new("rm").arg("wallet.db").arg("wallet.db-shm").arg("wallet.db-wal").output();
    let _ = fs::remove_dir_all("./rgb-data-sdk53");
    env::set_var("ML_NETWORK", "regtest");
    let cc = mercuryrustlib::client_config::load().await;

    let mut alice = deposit_coin(&cc, "sdk53_alice").await?;
    let sid = alice.statechain_id.clone().ok_or(anyhow!("no statechain_id"))?;
    let alice_exit = crate::bitcoin_core::getnewaddress()?;
    let ab = mercuryrustlib::tesr::establish_auto(&cc, &mut alice, &alice_exit, NETWORK).await?;
    mercuryrustlib::tesr::persist(&cc, "sdk53_alice", &ab).await?;

    let bob_wallet = mercuryrustlib::wallet::create_wallet("sdk53_bob", &cc).await?;
    mercuryrustlib::sqlite_manager::insert_wallet(&cc.pool, &bob_wallet).await?;
    let bob_addr = mercuryrustlib::transfer_receiver::new_transfer_address(&cc, "sdk53_bob").await?;

    // --- A LATCHED transfer (batch_id set) of a laddered coin must now OPEN (guard lifted). --------
    // Safety is provided by the SSP's pre-pay census + operator trust (LIGHTNING.md), not a refusal.
    let batch_id = format!("sdk53-batch-{}", uuid::Uuid::new_v4());
    let latched = mercuryrustlib::transfer_sender::execute(
        &cc, &bob_addr, "sdk53_alice", &sid, None, false, Some(batch_id),
    )
    .await;
    if let Err(e) = &latched {
        let msg = e.to_string();
        assert!(
            !msg.contains("Lightning-latched transfer"),
            "the old atomicity-guard refusal must be GONE, but it fired: {msg}"
        );
        return Err(anyhow!("latched transfer of a laddered coin should open, but failed: {msg}"));
    }
    println!("SDK53 - ✓ PASS: the LN-latch guard is LIFTED — a latched (batch_id) transfer of a laddered coin now opens instead of being refused. rob-SSP is guarded by the SSP's pre-pay verify_bundle census (peek_pending_transfers → execute_pay), not a blanket refusal. The full happy path (SSP censuses + pays a real BOLT11 with a laddered coin) is proved end-to-end by sdk63.");
    Ok(())
}
