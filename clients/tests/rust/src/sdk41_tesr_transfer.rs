//! E2E (SDK_E2E=41) — **TES-R off-chain transfer: receiver gains full control, sender is locked
//! out** — on the live SE + real bitcoind.
//!
//! A payment must (a) actually move control of the coin to the receiver and (b) lock the sender out.
//! In TES-R a transfer reuses the statechain key-rotation: the aggregate key `A` and the funding
//! UTXO `F` are INVARIANT (no on-chain tx), only the secret shares rotate and the SE deletes the old
//! share. This test proves both halves against the live stack:
//!
//!   1. Alice deposits F, then transfers the coin to Bob (clean statechain key-rotation).
//!   2. Bob — the new owner — blind-co-signs a FULL TES-R ladder (T→X→S) over the same F and
//!      unilaterally exits it. The transferred coin is fully usable by the receiver.
//!   3. Alice — the previous owner — can no longer co-sign ANYTHING for the coin: the SE rotated the
//!      owner auth key to Bob, so Alice's co-sign attempt is refused. She is cryptographically out.
//!
//! (The complementary threat — a sender who pre-signed a stale STATE *before* transferring, then
//! races the receiver — is defeated by the same decrementing-CSV mechanic proven in sdk40 Part 3;
//! carrying it end-to-end THROUGH the pre-TES-R receiver additionally needs the receiver R′ upgrade,
//! which is why sdk40's finding `num_sigs is not correct` fires — see docs/utexo/spec/PROTOCOL.md §5.11.)
//!
//! Run with SDK_E2E=41 (needs the regtest + Mercury lockbox stack, Core 28+).

use std::{env, fs, process::Command};

use anyhow::{anyhow, Result};

use crate::bitcoin_core;
use crate::sdk40_tesr_consensus::{broadcast, deposit_coin, is_outpoint_spent, mine, tx_exists, wait_for_address};

const NETWORK: &str = "regtest";
/// **THIS FIXTURE'S rate, not the protocol's.** The shipped committed rate is 3.0 since [D44]
/// (`TesrParams::for_network`). This test builds its own tiers at 2.0 and checks them against 2.0,
/// which is self-consistent and fine — but anything that must agree with a ladder the SDK built
/// must read the rate from the preset instead (see `sdk58`/`sdk70`, which broke by mixing the two).
const FEE_RATE: f64 = 2.0;
const CSV_E: u16 = 4;
const CSV_D: u16 = 6;

pub async fn execute() -> Result<()> {
    let _ = Command::new("rm").arg("wallet.db").arg("wallet.db-shm").arg("wallet.db-wal").output();
    let _ = fs::remove_dir_all("./rgb-data-sdk41");
    env::set_var("ML_NETWORK", "regtest");
    let cc = mercuryrustlib::client_config::load().await;

    // ---- 1. Alice deposits F, then transfers the coin to Bob (key-rotation; A + F invariant). ----
    let alice = deposit_coin(&cc, "sdk41_alice").await?;
    let alice_sid = alice.statechain_id.clone().ok_or(anyhow!("no statechain_id"))?;
    let f_txid = alice.utxo_txid.clone().ok_or(anyhow!("no F txid"))?;
    let f_vout = alice.utxo_vout.ok_or(anyhow!("no F vout"))?;

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
    assert_eq!(bob.utxo_txid.as_deref(), Some(f_txid.as_str()), "F unchanged across transfer (no on-chain tx)");
    assert_eq!(bob.utxo_vout, Some(f_vout), "F vout unchanged");
    assert_ne!(bob.user_privkey, alice.user_privkey, "owner key rotated to Bob");
    println!("SDK41 - coin transferred Alice→Bob (statechain_id {alice_sid}); F + A invariant, key rotated");

    // ---- 2. Bob (new owner) builds a FULL TES-R ladder over F and unilaterally exits it. ----
    let bob_exit = bitcoin_core::getnewaddress()?;
    let t = mercurylib::tesr::build_trigger(&f_txid, f_vout, f_value, &agg, NETWORK, FEE_RATE)?;
    let t_signed = mercuryrustlib::tesr::cosign_tier(&cc, &mut bob, t.tx_hex.clone(), f_value, NETWORK).await?;
    let x = mercurylib::tesr::build_extension(&t.txid, t.out_value, &agg, NETWORK, CSV_E, FEE_RATE)?;
    let x_signed = mercuryrustlib::tesr::cosign_tier(&cc, &mut bob, x.tx_hex.clone(), t.out_value, NETWORK).await?;
    let s = mercurylib::tesr::build_state(&x.txid, x.out_value, &bob_exit, NETWORK, CSV_D, FEE_RATE)?;
    let s_signed = mercuryrustlib::tesr::cosign_tier(&cc, &mut bob, s.tx_hex.clone(), x.out_value, NETWORK).await?;
    println!("SDK41 - Bob co-signed a full TES-R ladder T={} X={} S={} with the rotated key", t.txid, x.txid, s.txid);

    // ---- 3. Alice (previous owner) is locked out: the SE refuses her co-sign (auth key rotated). ----
    let mut alice_stale = alice.clone();
    let alice_attempt =
        mercuryrustlib::tesr::cosign_tier(&cc, &mut alice_stale, t.tx_hex.clone(), f_value, NETWORK).await;
    assert!(alice_attempt.is_err(), "previous owner Alice MUST be refused by the SE after transfer");
    println!("SDK41 - ✓ Alice locked out — SE refuses the previous owner's co-sign");

    // Bob completes the unilateral exit (T → wait E → X → wait D → S) — funds land at Bob.
    let _ = broadcast(&cc, &t_signed)?;
    let _ = mine(CSV_E as u32);
    let _ = broadcast(&cc, &x_signed)?;
    let _ = mine(CSV_D as u32);
    let _ = broadcast(&cc, &s_signed)?;
    let _ = mine(1)?;
    assert!(tx_exists(&cc, &s.txid), "Bob's state confirms");
    assert!(is_outpoint_spent(&cc, &f_txid, f_vout), "F consumed by Bob's exit");
    assert!(wait_for_address(&cc, &bob_exit, s.out_value as u32).await.is_ok(), "Bob is paid on exit");
    println!("SDK41 - ✓ PASS: transfer hands full control to Bob ({} sat exited); Alice cannot spend", s.out_value);
    Ok(())
}
