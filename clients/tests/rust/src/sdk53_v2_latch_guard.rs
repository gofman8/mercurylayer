//! E2E (SDK_E2E=53) — **V2DEF-4: the LN-latch atomicity guard refuses a latched V2 transfer**.
//!
//! A Lightning-latched transfer would co-sign the Model A state S' WITHOUT gating it on the swap
//! preimage (presign_receiver_state co-signs unconditionally and records nothing locally). If the swap
//! rolled back, that orphan SE co-sign would silently unbalance a later verify_bundle and brick the
//! coin. Until the co-sign is preimage-gated server-side (V2-DESIGN TES-fixable #5), transfer_sender
//! refuses a latched transfer of a V2 coin BEFORE any co-sign. This proves the guard fires — and that
//! a plain (un-latched) transfer of the same coin still succeeds via Model A.
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

    // --- A LATCHED transfer (batch_id set) of a V2 coin must be refused before any co-sign. ---------
    let batch_id = format!("sdk53-batch-{}", uuid::Uuid::new_v4());
    let latched = mercuryrustlib::transfer_sender::execute(
        &cc, &bob_addr, "sdk53_alice", &sid, None, false, Some(batch_id),
    )
    .await;
    let err = latched.err().ok_or(anyhow!("a latched V2 transfer must be refused, but it succeeded"))?;
    let msg = err.to_string();
    assert!(
        msg.contains("Lightning-latched transfer of a V2"),
        "refusal must be the specific atomicity guard, got: {msg}"
    );
    println!("SDK53 - latched V2 transfer correctly refused: {msg}");

    // --- The SAME coin still transfers fine un-latched (Model A path, batch_id None). ---------------
    mercuryrustlib::transfer_sender::execute(&cc, &bob_addr, "sdk53_alice", &sid, None, false, None).await?;
    mercuryrustlib::transfer_receiver::execute(&cc, "sdk53_bob").await?;
    mercuryrustlib::coin_status::update_coins(&cc, "sdk53_bob").await?;
    let received = mercuryrustlib::tesr::load(&cc, "sdk53_bob", &sid).await?.is_some();
    assert!(received, "un-latched Model A transfer still delivers the ladder to Bob");

    println!("SDK53 - ✓ PASS: latched V2 transfer refused (no orphan co-sign); un-latched Model A transfer still succeeds");
    Ok(())
}
