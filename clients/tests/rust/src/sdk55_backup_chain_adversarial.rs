//! E2E (SDK_E2E=55) — **ADVERSARIAL: the V2 lane's conveyed V1 backups cannot be padded or inverted [S2]**.
//!
//! A V2 coin still conveys V1 backups AND still feeds their COUNT into `verify_bundle`'s anti-theft
//! equation (`v1_backups = backup_transactions.len()`). V2DEF-1 had gated the structural check off for
//! `protocol_version >= 2`, leaving that term attacker-supplied:
//!   (a) **padding** — duplicate `tx1` inflates `expected` to absorb a hidden co-signed state (same
//!       prevout ⟹ one group; first-by-`tx_n` and `.last()` unchanged, so nothing else noticed);
//!   (b) **inversion** — the sender builds the receiver-paying backup at `L+interval` and keeps their
//!       own at `L`, so THEIR stale backup matures first and they claw the coin back.
//! `validate_backup_chain_v2`'s INV-5 (`ladder_decrements_by_interval`) is the defence for both.
//!
//! Both attacks below use the coin's REAL, validly-signed backups — so a rejection can only come from
//! INV-5 itself, not from an incidental signature failure.
//!
//! Run with SDK_E2E=55 (needs the regtest + Mercury lockbox stack, Core 28+).

use std::{env, fs};

use anyhow::{anyhow, Result};
use electrum_client::ElectrumApi;
use mercurylib::transfer::receiver::validate_backup_chain_v2;

use crate::sdk40_tesr_consensus::deposit_coin;

const NETWORK: &str = "regtest";

pub async fn execute() -> Result<()> {
    let _ = std::process::Command::new("rm").arg("wallet.db").arg("wallet.db-shm").arg("wallet.db-wal").output();
    let _ = fs::remove_dir_all("./rgb-data-sdk55");
    env::set_var("ML_NETWORK", "regtest");
    let cc = mercuryrustlib::client_config::load().await;

    // --- A real V2 coin, transferred once so the backup chain has TWO real hops (tx1 → tx2). --------
    let mut alice = deposit_coin(&cc, "sdk55_alice").await?;
    let sid = alice.statechain_id.clone().ok_or(anyhow!("no statechain_id"))?;
    let f_txid = alice.utxo_txid.clone().ok_or(anyhow!("no F txid"))?;
    let exit_addr = crate::bitcoin_core::getnewaddress()?;
    let ab = mercuryrustlib::tesr::establish_auto(&cc, &mut alice, &exit_addr, NETWORK).await?;
    mercuryrustlib::tesr::persist(&cc, "sdk55_alice", &ab).await?;

    let bob_wallet = mercuryrustlib::wallet::create_wallet("sdk55_bob", &cc).await?;
    mercuryrustlib::sqlite_manager::insert_wallet(&cc.pool, &bob_wallet).await?;
    let bob_addr = mercuryrustlib::transfer_receiver::new_transfer_address(&cc, "sdk55_bob").await?;
    mercuryrustlib::transfer_sender::execute(&cc, &bob_addr, "sdk55_alice", &sid, None, false, None).await?;
    mercuryrustlib::transfer_receiver::execute(&cc, "sdk55_bob").await?;

    // Bob's REAL conveyed backup chain (tx1 → tx2), each genuinely co-signed by the SE.
    let backups = mercuryrustlib::sqlite_manager::get_backup_txs(&cc.pool, "sdk55_bob", &sid).await?;
    if backups.len() < 2 {
        return Err(anyhow!("expected a >=2-hop backup chain, got {}", backups.len()));
    }
    println!("SDK55 - bob holds a real {}-hop V2-conveyed backup chain", backups.len());

    // --- The exact parameters the V2 receiver validates with. --------------------------------------
    let info = mercuryrustlib::utils::info_config(&cc).await?;
    let tip = cc.electrum_client.block_headers_subscribe_raw()?.height as u32;
    let tx0 = cc.electrum_client.transaction_get(&f_txid.parse()?)?;
    let tx0_hex = hex::encode(electrum_client::bitcoin::consensus::serialize(&tx0));
    let fee_rate = if info.fee_rate_sats_per_byte > cc.max_fee_rate { cc.max_fee_rate } else { info.fee_rate_sats_per_byte };
    let check = |b: &Vec<mercurylib::wallet::BackupTx>| {
        validate_backup_chain_v2(b, &tx0_hex, tip, cc.fee_rate_tolerance, fee_rate, info.initlock, info.interval)
    };

    // --- Control: the honest chain validates. ------------------------------------------------------
    check(&backups).map_err(|e| anyhow!("the honest backup chain must validate, got: {e}"))?;
    println!("SDK55 - control: the honest backup chain validates");

    // --- ATTACK A (padding): duplicate a REAL backup to inflate v1_backups by one. ------------------
    // Absorbs one hidden co-signed state in verify_bundle's count. Every tx is validly signed, so only
    // INV-5 can catch it: the duplicate decrements by 0, not `interval`.
    let mut padded = backups.clone();
    padded.insert(1, backups[0].clone());
    match check(&padded) {
        Ok(_) => return Err(anyhow!("SECURITY: duplicate-padded backup vector was ACCEPTED — v1_backups is still inflatable")),
        Err(e) => println!("SDK55 - ATTACK A (duplicate padding, all sigs valid) correctly REJECTED: {e}"),
    }

    // --- ATTACK B (inversion): order the chain so the locktime INCREASES. ---------------------------
    // Models a sender who conveys a receiver-paying backup that matures AFTER their own retained one.
    // Again all signatures are real — only INV-5's decrement rule rejects it.
    let inverted: Vec<_> = backups.iter().rev().cloned().collect();
    match check(&inverted) {
        Ok(_) => return Err(anyhow!("SECURITY: an INVERTED (increasing-locktime) ladder was ACCEPTED — the sender's stale backup would mature first")),
        Err(e) => println!("SDK55 - ATTACK B (inverted ladder, all sigs valid) correctly REJECTED: {e}"),
    }

    // --- The honest chain still validates after all that. -------------------------------------------
    check(&backups).map_err(|e| anyhow!("the honest chain must still validate, got: {e}"))?;

    println!("SDK55 - ✓ PASS: the V2 lane's v1_backups term is unforgeable — duplicate padding and ladder inversion are REJECTED by INV-5; honest chains validate");
    Ok(())
}
