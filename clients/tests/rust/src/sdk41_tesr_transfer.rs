//! E2E (SDK_E2E=41) — **TES-R off-chain transfer: receiver gains full control, sender is locked
//! out** — on the live SE + real bitcoind.
//!
//! A payment must (a) actually move control of the coin to the receiver and (b) lock the sender out.
//! In TES-R a transfer reuses the statechain key-rotation: the aggregate key `A` and the funding
//! UTXO `F` are INVARIANT (no on-chain tx), only the secret shares rotate and the SE deletes the old
//! share. The coin's exit material — the ladder co-signed at first mempool sight of the deposit
//! (T, X_0, S_0; enclave count exactly 3, no flat `tx1`) — crosses the transfer with ONE more co-sign,
//! the receiver-paying state S′, and `backup_transactions: []`. This test proves both halves against
//! the live stack:
//!
//!   1. Alice's deposit is laddered at sight; she transfers the coin to Bob (clean statechain
//!      key-rotation). Bob's wallet books it with `locktime: None` and NO flat backup row.
//!   2. Bob — the new owner — holds the conveyed ladder, exiting to HIS key: `num_sigs == 4`, the
//!      census balances with flat term 0 and NOT with the retired flat term 1. He RENEWS it with the
//!      rotated key (the SE accepts exactly his two co-signs, X_1 + S_1), persists it, and
//!      unilaterally exits through it. Funds land at Bob's backup address with no operator
//!      cooperation: the transferred coin is fully usable by the receiver.
//!   3. Alice — the previous owner — can no longer co-sign ANYTHING for the coin: the SE rotated the
//!      owner auth key to Bob, so her co-sign attempt is refused and consumes no co-sign. Her retained
//!      S_0 sits at a strictly HIGHER CSV than Bob's S′ over the same outpoint, so it cannot win the
//!      maturity race either (the decrementing-CSV mechanic proven in sdk40 Part 3).
//!
//! Run with SDK_E2E=41 (needs the regtest + Mercury lockbox stack, Core 28+).

use std::{env, fs, process::Command};

use anyhow::{anyhow, Result};
use mercuryrustlib::{tesr::verify_bundle, CoinStatus};

use crate::sdk40_tesr_consensus::{
    broadcast, deposit_coin, is_outpoint_spent, mine, se_num_sigs, tx_exists, wait_for_address,
};

const NETWORK: &str = "regtest";

pub async fn execute() -> Result<()> {
    let _ = Command::new("rm").arg("wallet.db").arg("wallet.db-shm").arg("wallet.db-wal").output();
    let _ = fs::remove_dir_all("./rgb-data-sdk41");
    env::set_var("ML_NETWORK", "regtest");
    let cc = mercuryrustlib::client_config::load().await;

    // ---- 1. Alice deposits F (laddered at sight), then transfers to Bob (key-rotation; A + F invariant). ----
    let alice = deposit_coin(&cc, "sdk41_alice").await?;
    let alice_sid = alice.statechain_id.clone().ok_or(anyhow!("no statechain_id"))?;
    let f_txid = alice.utxo_txid.clone().ok_or(anyhow!("no F txid"))?;
    let f_vout = alice.utxo_vout.ok_or(anyhow!("no F vout"))?;
    let alice_ladder = mercuryrustlib::tesr::load(&cc, "sdk41_alice", &alice_sid)
        .await?
        .ok_or(anyhow!("the deposit must have been laddered at first sight — it has no other exit material"))?;
    assert_eq!(se_num_sigs(&cc, &alice_sid).await?, 3, "a deposited coin's enclave count is exactly its three tiers — no tx1");
    let alice_state_csv = alice_ladder.current().state.csv.ok_or(anyhow!("S_0 has no CSV"))?;

    let bob_wallet = mercuryrustlib::wallet::create_wallet("sdk41_bob", &cc).await?;
    mercuryrustlib::sqlite_manager::insert_wallet(&cc.pool, &bob_wallet).await?;
    let bob_addr = mercuryrustlib::transfer_receiver::new_transfer_address(&cc, "sdk41_bob").await?;
    mercuryrustlib::transfer_sender::execute(&cc, &bob_addr, "sdk41_alice", &alice_sid, None, false, None).await?;
    mercuryrustlib::transfer_receiver::execute(&cc, "sdk41_bob").await?;
    mercuryrustlib::coin_status::update_coins(&cc, "sdk41_bob").await?;

    let mut bob = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk41_bob")
        .await?
        .coins
        .iter()
        .find(|c| c.statechain_id.as_deref() == Some(&alice_sid))
        .ok_or(anyhow!("Bob did not receive the coin"))?
        .clone();
    let agg = bob.aggregated_address.clone().ok_or(anyhow!("no agg addr"))?;
    let f_value = bob.amount.ok_or(anyhow!("no F value"))? as u64;
    assert_eq!(bob.status, CoinStatus::CONFIRMED, "the received coin rests on the confirmed funding output F");
    assert_eq!(bob.utxo_txid.as_deref(), Some(f_txid.as_str()), "F unchanged across transfer (no on-chain tx)");
    assert_eq!(bob.utxo_vout, Some(f_vout), "F vout unchanged");
    assert_ne!(bob.user_privkey, alice.user_privkey, "owner key rotated to Bob");
    assert_eq!(bob.locktime, None, "a laddered coin carries NO absolute calendar: the receiver books locktime None");
    assert!(
        mercuryrustlib::sqlite_manager::try_get_backup_txs(&cc.pool, "sdk41_bob", &alice_sid).await?.is_none(),
        "the receiver books NO flat backup row — none was conveyed (`backup_transactions: []`) and none may be"
    );
    println!("SDK41 - coin transferred Alice→Bob (statechain_id {alice_sid}); F + A invariant, key rotated, no flat backup");

    // ---- 2. Bob holds the conveyed ladder: census (4, 0); he renews it with the ROTATED key. ----
    let mut bob_ladder = mercuryrustlib::tesr::load(&cc, "sdk41_bob", &alice_sid)
        .await?
        .ok_or(anyhow!("Bob's wallet has no `tesr-{alice_sid}` row — the conveyed ladder is the coin's only exit material"))?;
    assert_eq!(bob_ladder.trigger.txid, alice_ladder.trigger.txid, "the SAME ladder crossed the transfer: T over F is invariant");
    assert_eq!(bob_ladder.owner_exit_address, bob.backup_address, "the conveyed ladder exits to BOB's own key ([D2])");
    let bob_state_csv = bob_ladder.current().state.csv.ok_or(anyhow!("S′ has no CSV"))?;
    assert!(
        bob_state_csv < alice_state_csv,
        "Bob's S′ (csv {bob_state_csv}) matures strictly before Alice's retained S_0 (csv {alice_state_csv}) over the same outpoint"
    );
    let ns = se_num_sigs(&cc, &alice_sid).await?;
    assert_eq!(ns, 4, "the transfer cost exactly ONE co-sign — the receiver state S′ — on top of the three deposit tiers");
    verify_bundle(&bob_ladder, ns, 0)
        .map_err(|e| anyhow!("Bob's received ladder must pass the census at num_sigs={ns} with flat term 0: {e}"))?;
    assert!(
        verify_bundle(&bob_ladder, ns, 1).is_err(),
        "the retired flat term (one deposit tx1) must NOT balance the census: 4 != 1 + 3 + 1"
    );

    // Renewal is two co-signs (X_1 + S_1) under the NEW owner's auth key — the proof that the SE now
    // answers to Bob. The count grows by exactly the tiers disclosed, and the renewed bundle still
    // balances with the flat term 0.
    let _ = mercuryrustlib::tesr::renew_auto(&cc, &mut bob, &mut bob_ladder).await?;
    let ns_renew = se_num_sigs(&cc, &alice_sid).await?;
    assert_eq!(ns_renew, ns + 2, "a renewal co-signs exactly X_1 and S_1 — the SE accepts the rotated key");
    verify_bundle(&bob_ladder, ns_renew, 0)
        .map_err(|e| anyhow!("Bob's renewed ladder must pass the census at num_sigs={ns_renew} with flat term 0: {e}"))?;
    mercuryrustlib::tesr::persist(&cc, "sdk41_bob", &bob_ladder).await?;
    println!(
        "SDK41 - Bob renewed the received ladder with the rotated key (num_sigs {ns} -> {ns_renew}); exit chain T={} X={} S={}",
        bob_ladder.trigger.txid,
        bob_ladder.current().extension.txid,
        bob_ladder.current().state.txid
    );

    // ---- 3. Alice (previous owner) is locked out: the SE refuses her co-sign (auth key rotated). ----
    let t = mercurylib::tesr::build_trigger(&f_txid, f_vout, f_value, &agg, NETWORK, bob_ladder.fee_rate)?;
    let mut alice_stale = alice.clone();
    let alice_attempt =
        mercuryrustlib::tesr::cosign_tier(&cc, &mut alice_stale, t.tx_hex.clone(), f_value, NETWORK).await;
    assert!(alice_attempt.is_err(), "previous owner Alice MUST be refused by the SE after transfer");
    assert_eq!(
        se_num_sigs(&cc, &alice_sid).await?,
        ns_renew,
        "a refused co-sign attempt consumes no co-sign — the census Bob will convey is untouched"
    );
    println!("SDK41 - ✓ Alice locked out — SE refuses the previous owner's co-sign");

    // Bob completes the unilateral exit through the ladder ON DISK (T → wait E → X_1 → wait D → S_1):
    // funds land at Bob's backup address, no operator cooperation.
    let r = mercuryrustlib::tesr::load(&cc, "sdk41_bob", &alice_sid).await?.ok_or(anyhow!("Bob's ladder did not persist"))?;
    let chain: Vec<(String, String, Option<u16>)> =
        r.exit_tiers().iter().map(|t| (t.txid.clone(), t.signed_tx.clone(), t.csv)).collect();
    let final_state = r.current().state.clone();
    let _ = broadcast(&cc, &chain[0].1)?;
    for (_txid, signed, csv) in &chain[1..] {
        let _ = mine(csv.ok_or(anyhow!("a non-trigger tier has no CSV"))? as u32);
        let _ = broadcast(&cc, signed)?;
    }
    let _ = mine(1)?;
    assert!(tx_exists(&cc, &final_state.txid), "Bob's state confirms");
    assert!(is_outpoint_spent(&cc, &f_txid, f_vout), "F consumed by Bob's exit");
    assert!(
        wait_for_address(&cc, &r.owner_exit_address, final_state.out_value as u32).await.is_ok(),
        "Bob is paid on exit at his own backup address"
    );
    println!("SDK41 - ✓ PASS: transfer hands full control to Bob ({} sat exited through the conveyed ladder); Alice cannot spend", final_state.out_value);
    Ok(())
}
