//! E2E (SDK_E2E=47) — **full R′ transfer: a pre-established TES-R ladder carried across a transfer**.
//!
//! The exact flow sdk41 could NOT complete (it tripped the un-laddered receive path's checks). With
//! the ladder conveyed in a protocol_version=2 message, the receiver's flat
//! `num_sigs == backup_transactions.len()` equality is replaced by the R′ verifier's count equation
//! (the decrementing-ladder structural check still runs on both paths), so the transfer completes.
//!
//! Run with SDK_E2E=47 (needs the regtest + Mercury lockbox stack, Core 28+).

use std::{env, fs, process::Command};

use anyhow::{anyhow, Result};

use crate::bitcoin_core;
use crate::sdk40_tesr_consensus::deposit_coin;

const NETWORK: &str = "regtest";

pub async fn execute() -> Result<()> {
    let _ = Command::new("rm").arg("wallet.db").arg("wallet.db-shm").arg("wallet.db-wal").output();
    let _ = fs::remove_dir_all("./rgb-data-sdk47");
    env::set_var("ML_NETWORK", "regtest");
    let cc = mercuryrustlib::client_config::load().await;

    let mut alice = deposit_coin(&cc, "sdk47_alice").await?;
    let sid = alice.statechain_id.clone().ok_or(anyhow!("no statechain_id"))?;
    let owner_exit = bitcoin_core::getnewaddress()?;
    let bundle = mercuryrustlib::tesr::establish_auto(&cc, &mut alice, &owner_exit, NETWORK).await?;
    mercuryrustlib::tesr::persist(&cc, "sdk47_alice", &bundle).await?;
    println!("SDK47 - Alice established + persisted a TES-R ladder ({} tiers)", bundle.exit_tiers().len());

    let bob_wallet = mercuryrustlib::wallet::create_wallet("sdk47_bob", &cc).await?;
    mercuryrustlib::sqlite_manager::insert_wallet(&cc.pool, &bob_wallet).await?;
    let bob_addr = mercuryrustlib::transfer_receiver::new_transfer_address(&cc, "sdk47_bob").await?;
    mercuryrustlib::transfer_sender::execute(&cc, &bob_addr, "sdk47_alice", &sid, None, false, None).await?;
    println!("SDK47 - Alice sent the transfer (ladder conveyed in a protocol_version=2 message)");

    let received = mercuryrustlib::transfer_receiver::execute(&cc, "sdk47_bob").await?;
    println!("SDK47 - Bob receive result: {:?}", received.received_statechain_ids);
    mercuryrustlib::coin_status::update_coins(&cc, "sdk47_bob").await?;

    let bob_has_it = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk47_bob")
        .await?
        .coins
        .iter()
        .any(|c| c.statechain_id.as_deref() == Some(&sid));
    assert!(bob_has_it, "Bob must have received the coin — R′ accepted the pre-established ladder");
    println!("SDK47 - ✓ PASS: a pre-established TES-R ladder crossed a transfer via R′");
    Ok(())
}
