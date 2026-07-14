//! E2E (SDK_E2E=49) — **Model A transfer: receiver adopts a self-paying ladder and EXITS it**.
//!
//! The fund-safety core of V2DEF-3 (V2-MIGRATION). Alice transfers a V2 coin to Bob; the sender
//! pre-signs the RECEIVER-paying state S' (pays Bob, one δ lower CSV); the receiver verifies it
//! exits to Bob's OWN key and adopts (persists) the ladder; then Bob unilaterally exits it and the
//! funds land at Bob. Bob could only spend S' if it truly pays his key — so this end-to-end exit is
//! the direct proof that Model A hands the receiver a complete, self-custodial exit chain.
//!
//! Run with SDK_E2E=49 (needs the regtest + Mercury lockbox stack, Core 28+).

use std::{env, fs, process::Command};

use anyhow::{anyhow, Result};

use crate::bitcoin_core;
use crate::sdk40_tesr_consensus::{broadcast, deposit_coin, is_outpoint_spent, mine, tx_exists, wait_for_address};

const NETWORK: &str = "regtest";

pub async fn execute() -> Result<()> {
    let _ = Command::new("rm").arg("wallet.db").arg("wallet.db-shm").arg("wallet.db-wal").output();
    let _ = fs::remove_dir_all("./rgb-data-sdk49");
    env::set_var("ML_NETWORK", "regtest");
    let cc = mercuryrustlib::client_config::load().await;

    // ---- Alice: V2 coin (establish a ladder), then transfer to Bob. ----
    let mut alice = deposit_coin(&cc, "sdk49_alice").await?;
    let sid = alice.statechain_id.clone().ok_or(anyhow!("no statechain_id"))?;
    let f_txid = alice.utxo_txid.clone().ok_or(anyhow!("no F txid"))?;
    let f_vout = alice.utxo_vout.ok_or(anyhow!("no F vout"))?;
    let alice_exit = bitcoin_core::getnewaddress()?;
    let ab = mercuryrustlib::tesr::establish_auto(&cc, &mut alice, &alice_exit, NETWORK).await?;
    mercuryrustlib::tesr::persist(&cc, "sdk49_alice", &ab).await?;
    println!("SDK49 - Alice established a ladder; transferring to Bob (Model A pre-signs the Bob-paying state)");

    let bob_wallet = mercuryrustlib::wallet::create_wallet("sdk49_bob", &cc).await?;
    mercuryrustlib::sqlite_manager::insert_wallet(&cc.pool, &bob_wallet).await?;
    let bob_addr = mercuryrustlib::transfer_receiver::new_transfer_address(&cc, "sdk49_bob").await?;
    mercuryrustlib::transfer_sender::execute(&cc, &bob_addr, "sdk49_alice", &sid, None, false, None).await?;
    mercuryrustlib::transfer_receiver::execute(&cc, "sdk49_bob").await?;
    mercuryrustlib::coin_status::update_coins(&cc, "sdk49_bob").await?;

    // ---- Bob ADOPTED the ladder (persisted at receive). It exits to Bob's own key. ----
    let bob_coin = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk49_bob")
        .await?
        .coins
        .iter()
        .find(|c| c.statechain_id.as_deref() == Some(&sid))
        .cloned()
        .ok_or(anyhow!("Bob did not receive the coin"))?;
    let bob_backup = mercurylib::transaction::get_user_backup_address(&bob_coin, NETWORK.to_string())?;
    let bob_bundle = mercuryrustlib::tesr::load(&cc, "sdk49_bob", &sid)
        .await?
        .ok_or(anyhow!("Bob did not adopt the ladder"))?;
    assert_eq!(bob_bundle.owner_exit_address, bob_backup, "adopted ladder exits to BOB's own key");
    // The receiver-paying state must sit one δ below Alice's (matures first).
    assert!(bob_bundle.current().state.csv.unwrap() < ab.current().state.csv.unwrap(), "S' CSV is lower than Alice's");
    println!("SDK49 - Bob adopted the ladder; its state pays Bob (csv {:?}) and is lower than Alice's (csv {:?})",
        bob_bundle.current().state.csv, ab.current().state.csv);

    // ---- Bob unilaterally EXITS the adopted ladder — funds must land at Bob. ----
    let tiers: Vec<(String, Option<u16>)> =
        bob_bundle.exit_tiers().iter().map(|t| (t.signed_tx.clone(), t.csv)).collect();
    let final_state = bob_bundle.current().state.clone();
    assert!(!is_outpoint_spent(&cc, &f_txid, f_vout), "F still unspent before Bob's exit");
    let _ = broadcast(&cc, &tiers[0].0)?; // trigger
    for (signed, csv) in &tiers[1..] {
        let _ = mine(csv.unwrap() as u32);
        let _ = broadcast(&cc, signed)?;
    }
    let _ = mine(1)?;
    assert!(tx_exists(&cc, &final_state.txid), "Bob's exit state confirms");
    assert!(is_outpoint_spent(&cc, &f_txid, f_vout), "F consumed by Bob's exit");
    assert!(wait_for_address(&cc, &bob_backup, final_state.out_value as u32).await.is_ok(), "funds landed at Bob's own key");
    println!("SDK49 - ✓ PASS: Model A — Bob adopted a self-paying ladder and unilaterally exited it ({} sat to Bob's key)", final_state.out_value);
    Ok(())
}
