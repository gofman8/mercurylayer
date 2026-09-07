//! E2E (SDK_E2E=47) — **full R′ transfer: the DEPOSIT's TES-R ladder carried across a transfer, with
//! the census `se_num_sigs == tiers + superseded` (flat term 0) balanced on BOTH sides**.
//!
//! A coin's only exit material is the ladder `coin_status::check_deposit` co-signs at the first
//! mempool sighting of its funding: T, X_0, S_0 — the enclave count is exactly 3, and no flat `tx1`
//! exists beside it. A whole-coin transfer conveys that ladder in a protocol_version=2 message with
//! `backup_transactions: []`: the sender co-signs EXACTLY ONE more tier — the receiver-paying state
//! S′, one δ below S_0 over the same outpoint — and never a per-hop flat backup. The receiver binds
//! the ladder to the coin (`coin_authority_from_tx0`), runs the census with the flat term 0, and books
//! the coin with `locktime: None` and no flat backup row.
//!
//! What this test measures against the live stack:
//!   1. Before the transfer: `num_sigs == 3`; the deposit ladder verifies at `(3, 0)`.
//!   2. Opening the transfer costs exactly ONE co-sign (`num_sigs == 4`): S′, nothing flat.
//!   3. Bob's receive completes. His wallet holds the SAME ladder (same trigger, same F), re-pointed
//!      at HIS backup address, with Alice's S_0 disclosed as superseded at a strictly HIGHER CSV than
//!      S′; it passes the bound census at `(4, 0)` — exactly the receiver's own check — and is refused
//!      at `(4, 1)`, so a receiver that still budgeted a deposit `tx1` would have refused this
//!      transfer. Bob holds NO flat backup row and his coin has `locktime: None`.
//!
//! Run with SDK_E2E=47 (needs the regtest + Mercury lockbox stack, Core 28+).

use std::{env, fs, process::Command, str::FromStr};

use anyhow::{anyhow, Result};
use electrum_client::ElectrumApi;
use mercuryrustlib::{
    client_config::ClientConfig,
    tesr::{verify_bundle, verify_bundle_bound},
    CoinStatus,
};

use crate::sdk40_tesr_consensus::{deposit_coin, se_num_sigs};

/// The funding transaction as the RECEIVER reads it — from the chain, never from the message.
fn tx0_hex_from_chain(cc: &ClientConfig, txid: &str) -> Result<String> {
    let id = electrum_client::bitcoin::Txid::from_str(txid).map_err(|_| anyhow!("bad txid"))?;
    let raw = cc.electrum_client.transaction_get_raw(&id).map_err(|_| anyhow!("F not on chain"))?;
    Ok(hex::encode(raw))
}

pub async fn execute() -> Result<()> {
    let _ = Command::new("rm").arg("wallet.db").arg("wallet.db-shm").arg("wallet.db-wal").output();
    let _ = fs::remove_dir_all("./rgb-data-sdk47");
    env::set_var("ML_NETWORK", "regtest");
    let cc = mercuryrustlib::client_config::load().await;

    // ---- 1. Alice's deposit: laddered at first sight — exactly 3 co-signs, census flat term 0. ----
    // LOADED, never re-established: a second ladder over F would be three more irreversible co-signs
    // the census could never account for.
    let alice = deposit_coin(&cc, "sdk47_alice").await?;
    let sid = alice.statechain_id.clone().ok_or(anyhow!("no statechain_id"))?;
    let alice_ladder = mercuryrustlib::tesr::load(&cc, "sdk47_alice", &sid)
        .await?
        .ok_or(anyhow!("the deposit must have been laddered at first sight — it has no other exit material"))?;
    let before = se_num_sigs(&cc, &sid).await?;
    assert_eq!(before, 3, "a deposited coin's enclave count is exactly its three tiers (T, X_0, S_0) — no tx1");
    verify_bundle(&alice_ladder, before, 0)
        .map_err(|e| anyhow!("the deposit ladder must pass the census with flat term 0: {e}"))?;
    let alice_state_csv = alice_ladder.current().state.csv.ok_or(anyhow!("S_0 has no CSV"))?;
    println!("SDK47 - Alice's deposit ladder loaded ({} tiers, num_sigs={before}, S_0 csv {alice_state_csv})", alice_ladder.exit_tiers().len());

    // ---- 2. Transfer: exactly ONE co-sign (the receiver state S′), no flat backup. ---------------
    let bob_wallet = mercuryrustlib::wallet::create_wallet("sdk47_bob", &cc).await?;
    mercuryrustlib::sqlite_manager::insert_wallet(&cc.pool, &bob_wallet).await?;
    let bob_addr = mercuryrustlib::transfer_receiver::new_transfer_address(&cc, "sdk47_bob").await?;
    mercuryrustlib::transfer_sender::execute(&cc, &bob_addr, "sdk47_alice", &sid, None, false, None).await?;
    let after_send = se_num_sigs(&cc, &sid).await?;
    assert_eq!(
        after_send,
        before + 1,
        "opening a transfer co-signs EXACTLY ONE tier — the receiver-paying state S′ — and no per-hop flat backup"
    );
    println!("SDK47 - Alice sent the transfer (ladder conveyed in a protocol_version=2 message; num_sigs {before} -> {after_send})");

    // ---- 3. Bob receives: the same ladder, bound to the coin, census (4, 0). --------------------
    let received = mercuryrustlib::transfer_receiver::execute(&cc, "sdk47_bob").await?;
    println!("SDK47 - Bob receive result: {:?}", received.received_statechain_ids);
    assert!(
        received.received_statechain_ids.contains(&sid),
        "Bob must have received the coin — R′ accepted the deposit-established ladder"
    );
    mercuryrustlib::coin_status::update_coins(&cc, "sdk47_bob").await?;

    let bob = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk47_bob")
        .await?
        .coins
        .iter()
        .find(|c| c.statechain_id.as_deref() == Some(&sid))
        .cloned()
        .ok_or(anyhow!("Bob's wallet does not hold the received coin"))?;
    assert_eq!(bob.status, CoinStatus::CONFIRMED, "the received coin rests on the confirmed funding output F");
    assert_eq!(bob.utxo_txid, alice.utxo_txid, "F is invariant across the transfer (no on-chain tx)");
    assert_eq!(bob.utxo_vout, alice.utxo_vout, "F vout invariant");
    assert_eq!(bob.locktime, None, "a laddered coin carries NO absolute calendar: the receiver books locktime None");
    assert!(
        mercuryrustlib::sqlite_manager::try_get_backup_txs(&cc.pool, "sdk47_bob", &sid).await?.is_none(),
        "the receiver books NO flat backup row — none was conveyed and none may be"
    );

    let bob_ladder = mercuryrustlib::tesr::load(&cc, "sdk47_bob", &sid)
        .await?
        .ok_or(anyhow!("Bob's wallet has no `tesr-{sid}` row — the conveyed ladder is the coin's only exit material"))?;
    assert_eq!(bob_ladder.trigger.txid, alice_ladder.trigger.txid, "the SAME ladder crossed the transfer: T over F is invariant");
    assert_eq!(bob_ladder.f_txid, alice_ladder.f_txid, "rooted at the same funding outpoint");
    assert_eq!(bob_ladder.f_vout, alice_ladder.f_vout, "rooted at the same funding vout");
    assert_eq!(bob_ladder.owner_exit_address, bob.backup_address, "the conveyed ladder exits to BOB's own key ([D2])");
    assert_eq!(bob_ladder.exit_tiers().len(), 3, "still T -> X_0 -> S′");
    assert_eq!(bob_ladder.superseded_states.len(), 1, "Alice's S_0 is disclosed as superseded — full-disclosure counting");
    assert!(bob_ladder.superseded_extensions.is_empty(), "a transfer supersedes no extension");
    assert!(bob_ladder.conveyed_states.is_empty(), "the sender's conveyance record never travels");
    let bob_state_csv = bob_ladder.current().state.csv.ok_or(anyhow!("S′ has no CSV"))?;
    assert!(
        bob_state_csv < alice_state_csv,
        "S′ (csv {bob_state_csv}) must mature strictly before Alice's stale S_0 (csv {alice_state_csv}) over the same outpoint"
    );

    // The census, exactly as the receiver runs it: bound to the coin (funding tx read from the chain,
    // aggregate from the coordinator), flat term 0.
    let se = se_num_sigs(&cc, &sid).await?;
    assert_eq!(se, 4, "after one transfer the enclave count is 3 tiers + 1 receiver state");
    verify_bundle(&bob_ladder, se, 0)
        .map_err(|e| anyhow!("Bob's ladder must pass the census at num_sigs={se} with flat term 0: {e}"))?;
    match verify_bundle(&bob_ladder, se, 1) {
        Ok(()) => return Err(anyhow!("SECURITY: a flat term of 1 (a deposit tx1 that was never co-signed) balanced the census — one hidden state could ride on it")),
        Err(e) => {
            let msg = e.to_string();
            if !msg.contains("num_sigs mismatch") {
                return Err(anyhow!("flat term 1 was refused for the WRONG reason — expected the census mismatch, got: {msg}"));
            }
            println!("SDK47 - a flat term of 1 is correctly REFUSED against the received ladder: {msg}");
        }
    }
    let info = mercuryrustlib::utils::get_statechain_info(&sid, &cc)
        .await?
        .ok_or(anyhow!("no statechain_info"))?;
    let tx0 = tx0_hex_from_chain(&cc, &bob_ladder.f_txid)?;
    let authority = mercuryrustlib::tesr::coin_authority_from_tx0(
        &sid,
        &bob_ladder.f_txid,
        bob_ladder.f_vout,
        &tx0,
        info.aggregate_pubkey.clone(),
    )?;
    verify_bundle_bound(&bob_ladder, se, 0, &authority)
        .map_err(|e| anyhow!("Bob's ladder must pass the BOUND census against its own coin: {e}"))?;
    // The empty `backup_transactions` vector every laddered conveyance carries is admitted by the
    // receiver's lane check (its refusal of a non-empty one is attacked in sdk54, ATTACK H).
    mercuryrustlib::tesr::verify_flat_backup_lane(&bob_ladder, &[])
        .map_err(|e| anyhow!("the empty backup vector a laddered conveyance carries must be admitted: {e}"))?;

    println!("SDK47 - ✓ PASS: the deposit-established TES-R ladder crossed a transfer via R′ — one co-sign (S′), no flat backup, census {se} == 3 tiers + 1 superseded with flat term 0, bound to the coin");
    Ok(())
}
