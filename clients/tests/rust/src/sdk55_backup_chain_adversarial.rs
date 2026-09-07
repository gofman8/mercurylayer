//! E2E (SDK_E2E=55) — **ADVERSARIAL: the flat term of a laddered coin's census is IDENTICALLY ZERO
//! and cannot be padded; a disclosed rival cannot be inverted [S2]**.
//!
//! This file used to attack `validate_backup_chain_v2` — the INV-5 decrement rule over a conveyed
//! FLAT backup chain — because a laddered coin still conveyed its signed-once backups and fed their
//! COUNT into the census (`flat_backups = backup_transactions.len()`), a term the sender supplied.
//! That chain no longer exists: `deposit::create_tx1` is gone, a transfer conveys
//! `backup_transactions: []`, and the census every receiver runs is
//! `se_num_sigs == tiers + superseded` with the flat term pinned to ZERO. So the two attacks this
//! file exists for are re-derived onto the ladder, against the REAL conveyed bundle of a real hop:
//!
//!   (a) **padding** — the sender conveys a flat backup beside the ladder, hoping the receiver counts
//!       it and thereby absorbs one hidden co-signed state. The receiver never counts it: ANY
//!       non-empty vector is refused BY NAME before the census runs (`verify_flat_backup_lane` on
//!       the root lane, `refuse_conveyed_flat_backups` on the child/tail/stub lanes), the census
//!       does not balance with a flat term of one, and — the property that closes the attack — a
//!       count one higher than the disclosed tiers is refused with the flat term at zero, because
//!       there is no term left for a hidden co-sign to hide behind;
//!   (b) **inversion** — the sender arranges for THEIR stale state to mature first (the old attack
//!       built the receiver's backup at `L + interval` and kept its own at `L`). On the ladder the
//!       receiver's state must sit at a STRICTLY LOWER CSV than every disclosed rival over the same
//!       outpoint; a bundle whose live state is the sender's higher-CSV `S_0` and whose disclosed
//!       rival is the receiver's lower-CSV `S'` is refused by the superseded battery, by name.
//!
//! Both attacks use the coin's REAL, validly co-signed tiers — so a rejection can only come from
//! the census rules themselves, never from an incidental signature failure. The honest bundle
//! validates before and after.
//!
//! Run with SDK_E2E=55 (needs the regtest + Mercury lockbox stack, Core 28+).

use std::{env, fs};

use anyhow::{anyhow, Result};
use mercurylib::wallet::BackupTx;

use crate::sdk40_tesr_consensus::{deposit_coin, se_num_sigs};

pub async fn execute() -> Result<()> {
    let _ = std::process::Command::new("rm").arg("wallet.db").arg("wallet.db-shm").arg("wallet.db-wal").output();
    let _ = fs::remove_dir_all("./rgb-data-sdk55");
    env::set_var("ML_NETWORK", "regtest");
    let cc = mercuryrustlib::client_config::load().await;

    // --- A real coin, LADDERED BY THE DEPOSIT (T, X_0, S_0 — num_sigs 3, no flat row), then --------
    // --- transferred once so the conveyed bundle carries one real hop: S' live, S_0 disclosed. ----
    let alice = deposit_coin(&cc, "sdk55_alice").await?;
    let sid = alice.statechain_id.clone().ok_or(anyhow!("no statechain_id"))?;
    let alice_exit = alice.backup_address.clone();

    let bob_wallet = mercuryrustlib::wallet::create_wallet("sdk55_bob", &cc).await?;
    mercuryrustlib::sqlite_manager::insert_wallet(&cc.pool, &bob_wallet).await?;
    let bob_addr = mercuryrustlib::transfer_receiver::new_transfer_address(&cc, "sdk55_bob").await?;
    mercuryrustlib::transfer_sender::execute(&cc, &bob_addr, "sdk55_alice", &sid, None, false, None).await?;
    mercuryrustlib::transfer_receiver::execute(&cc, "sdk55_bob").await?;

    // Bob holds NO flat backup and no calendar; what he holds is the ladder.
    let bob_flat = mercuryrustlib::sqlite_manager::try_get_backup_txs(&cc.pool, "sdk55_bob", &sid)
        .await?
        .map(|rows| rows.len())
        .unwrap_or(0);
    assert_eq!(bob_flat, 0, "SDK55 - the receiver must hold ZERO flat backup rows: none is conveyed, none is written");
    let bob_coin = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk55_bob")
        .await?
        .coins
        .into_iter()
        .find(|c| c.statechain_id.as_deref() == Some(sid.as_str()))
        .ok_or_else(|| anyhow!("bob did not book the coin"))?;
    assert!(bob_coin.locktime.is_none(), "SDK55 - a laddered coin has no absolute calendar: locktime must be None");
    let bundle = mercuryrustlib::tesr::load(&cc, "sdk55_bob", &sid)
        .await?
        .ok_or_else(|| anyhow!("bob booked the coin with no `tesr-` ladder row"))?;
    let n = se_num_sigs(&cc, &sid).await?;
    assert_eq!(n, 4, "SDK55 - one hop over a deposit ladder is exactly 3 + 1 co-signs (S'); there is no tx1 and no per-hop backup");
    assert_eq!(bundle.superseded_states.len(), 1, "SDK55 - the sender's replaced S_0 is disclosed as the one superseded state");
    println!("SDK55 - bob holds a real one-hop ladder: num_sigs {n}, {} live tiers, 1 superseded, 0 flat rows", bundle.exit_tiers().len());

    // --- Control: the honest bundle passes the census with the flat term ZERO. ---------------------
    mercuryrustlib::tesr::verify_bundle(&bundle, n, 0)
        .map_err(|e| anyhow!("the honest bundle must pass the census with flat term 0, got: {e}"))?;
    println!("SDK55 - control: the honest bundle balances the census (num_sigs {n} == tiers + superseded, flat term 0)");

    // --- ATTACK A (padding): convey ONE flat backup beside the ladder. ------------------------------
    // The "backup" is a REAL, validly co-signed transaction of this very coin (the trigger) so that
    // nothing about it is malformed; only the rule that a laddered coin conveys NO flat backup can
    // refuse it.
    let padding = BackupTx {
        tx_n: 1,
        tx: bundle.trigger.signed_tx.clone(),
        client_public_nonce: String::new(),
        server_public_nonce: String::new(),
        client_public_key: String::new(),
        server_public_key: String::new(),
        blinding_factor: String::new(),
        rgb_consignment: None,
        rgb_blinding: None,
    };
    let root_lane = mercuryrustlib::tesr::verify_flat_backup_lane(&bundle, std::slice::from_ref(&padding));
    let root_msg = format!("{:#}", root_lane.err().ok_or_else(|| anyhow!(
        "SECURITY: the ROOT lane ACCEPTED a ladder conveyed with a flat backup — the flat term is still sender-supplied"
    ))?);
    assert!(
        root_msg.contains("refusing a plain ladder conveyed with 1 flat backup transaction(s)"),
        "the root-lane refusal must be BY NAME, not incidental: {root_msg}"
    );
    println!("SDK55 - ATTACK A (root lane, 1 real tx conveyed as a flat backup) correctly REFUSED: {root_msg}");
    let child_lane = mercuryrustlib::tesr::refuse_conveyed_flat_backups("conveyed child", std::slice::from_ref(&padding));
    let child_msg = format!("{:#}", child_lane.err().ok_or_else(|| anyhow!(
        "SECURITY: the CHILD lane ACCEPTED a conveyed flat backup beside a ladder"
    ))?);
    assert!(
        child_msg.contains("conveys 1 flat backup transaction(s) beside its ladder"),
        "the child-lane refusal must be BY NAME: {child_msg}"
    );
    println!("SDK55 - ATTACK A (child lane) correctly REFUSED: {child_msg}");
    // The census itself has no slot for the padding either...
    assert!(
        mercuryrustlib::tesr::verify_bundle(&bundle, n, 1).is_err(),
        "SECURITY: the census BALANCED with a flat term of ONE — a sender-supplied term is back"
    );
    // ...and the hidden co-sign the padding was meant to absorb is caught with the term at zero: a
    // count one higher than the disclosed tiers has nowhere to hide.
    assert!(
        mercuryrustlib::tesr::verify_bundle(&bundle, n + 1, 0).is_err(),
        "SECURITY: a count one higher than the disclosed tiers was ACCEPTED with the flat term at zero — a hidden co-signed state passes the census"
    );
    println!("SDK55 - ATTACK A (census): flat term 1 does not balance; a hidden extra co-sign is refused at flat term 0");

    // --- ATTACK B (inversion): make the SENDER's stale state the live one. --------------------------
    // Swap the live receiver state S' (lower CSV) with the disclosed S_0 (higher CSV) and point the
    // exit at the sender's key, so the bundle is internally consistent everywhere EXCEPT the race:
    // the disclosed rival now matures BEFORE the live state. Every signature is real; only the
    // superseded battery's replace-by-lower-timelock rule can refuse it.
    let mut inverted = bundle.clone();
    let last = inverted.levels.len() - 1;
    let live = inverted.levels[last].state.clone();
    let rival = inverted.superseded_states[0].clone();
    assert!(
        live.csv.unwrap_or(u16::MAX) < rival.csv.unwrap_or(0),
        "precondition: the honest bundle's live S' (csv {:?}) sits below the disclosed S_0 (csv {:?})",
        live.csv, rival.csv
    );
    inverted.levels[last].state = rival;
    inverted.superseded_states[0] = live;
    inverted.owner_exit_address = alice_exit;
    let inv = mercuryrustlib::tesr::verify_bundle(&inverted, n, 0);
    let inv_msg = format!("{:#}", inv.err().ok_or_else(|| anyhow!(
        "SECURITY: an INVERTED bundle (sender's higher-CSV state live, receiver's lower-CSV state disclosed as beaten) was ACCEPTED — the sender's stale state would mature first"
    ))?);
    assert!(
        inv_msg.contains("superseded state 0 has CSV"),
        "the inversion must be refused by the superseded battery's race rule, by name: {inv_msg}"
    );
    println!("SDK55 - ATTACK B (inverted rival, all sigs valid) correctly REFUSED: {inv_msg}");

    // --- The honest bundle still validates after all that. ----------------------------------------
    mercuryrustlib::tesr::verify_bundle(&bundle, n, 0)
        .map_err(|e| anyhow!("the honest bundle must still validate, got: {e}"))?;

    println!("SDK55 - ✓ PASS: a laddered coin's flat term is identically zero — a conveyed flat backup is refused by name on both lanes, the census has no term to pad, and an inverted rival is refused by the race rule; the honest bundle validates");
    Ok(())
}
