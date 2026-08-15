//! E2E (SDK_E2E=60) — **first-class children: a received split child is RE-TRANSFERRED off-chain**.
//!
//! This is the Spark-parity property the in-ladder split was missing. Until now a received non-exact
//! payment (an in-ladder split CHILD) was an exit-only claim: you could materialize it on-chain, but
//! you could not pay it onward off-chain. It is now a first-class coin
//! (`docs/utexo/spec/CHILDREN.md`):
//!
//!   * Alice pays Bob a NON-EXACT amount ⟹ in-ladder split; the piece child is conveyed WITH the
//!     standard key handover.
//!   * Bob's `claim()` censuses the bundle and COMPLETES the handover — the SE rotates its share, so
//!     `A_child` is invariant (every pre-signed child tier stays valid) and Alice is locked out.
//!   * **Bob then pays the WHOLE child onward to Carol, entirely off-chain** — `child_retransfer`
//!     co-signs a fresh state over `ext_child.out[0]` at a strictly LOWER CSV (so it out-races the one
//!     it replaces) paying Carol, and discloses the replaced state as superseded.
//!   * Carol's claim runs the SAME census, now including the CHILD-SUPERSEDED segment
//!     (`child_num_sigs == 0 + 2 + 1`), and she unilaterally exits — the funds land at Carol's key.
//!   * Throughout, the original funding outpoint `F` stays UNSPENT: two payments, zero on-chain
//!     footprint, only the final exit touches the chain.
//!
//! Run: SDK_E2E=60 ML_NETWORK=regtest cargo +stable run

use std::str::FromStr;
use std::time::Duration;

use anyhow::{anyhow, Result};
use electrum_client::ElectrumApi;
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};

use crate::bitcoin_core;

const DEPOSIT: u64 = 100_000;
const PAY: u64 = 30_000;

async fn prepaid_token(cc: &mercuryrustlib::client_config::ClientConfig) -> Result<String> {
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    crate::utils::handle_token_response(cc, &token).await
}

async fn num_sigs(cc: &mercuryrustlib::client_config::ClientConfig, sid: &str) -> Result<u32> {
    Ok(mercuryrustlib::utils::get_statechain_info(sid, cc)
        .await?
        .ok_or(anyhow!("no statechain info"))?
        .num_sigs)
}

async fn v2_wallet(name: &str) -> Result<UtexoWallet> {
    let (w, _) = UtexoWallet::initialize(SdkConfig::regtest(name), None).await?;
    Ok(w)
}

fn is_outpoint_spent(cc: &mercuryrustlib::client_config::ClientConfig, txid: &str, vout: u32) -> bool {
    use electrum_client::bitcoin::Txid;
    let raw = match cc.electrum_client.transaction_get_raw(&Txid::from_str(txid).unwrap()) {
        std::result::Result::Ok(r) => r,
        _ => return true,
    };
    let tx: electrum_client::bitcoin::Transaction =
        electrum_client::bitcoin::consensus::deserialize(&raw).unwrap();
    let spk = tx.output[vout as usize].script_pubkey.clone();
    match cc.electrum_client.script_list_unspent(&spk) {
        std::result::Result::Ok(utxos) => !utxos.iter().any(|u| u.tx_hash.to_string() == txid && u.tx_pos == vout as usize),
        _ => true,
    }
}

/// Poll `claim()` until the wallet books a coin of `want` sats, returning its statechain id.
async fn claim_until(
    cc: &mercuryrustlib::client_config::ClientConfig,
    w: &UtexoWallet,
    name: &str,
    want: u64,
) -> Result<String> {
    for _ in 0..40 {
        w.claim().await?;
        if w.get_balance().await?.available_sats == want {
            return mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, name)
                .await?
                .coins
                .iter()
                .find(|c| {
                    c.status == mercurylib::wallet::CoinStatus::CONFIRMED
                        && c.amount == Some(want as u32)
                })
                .and_then(|c| c.statechain_id.clone())
                .ok_or_else(|| anyhow!("{name} booked no confirmed {want}-sat coin"));
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Err(anyhow!("{name} did not claim a {want}-sat coin"))
}

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    let cc = mercuryrustlib::client_config::load().await;
    let core = bitcoin_core::getnewaddress()?;

    let alice = v2_wallet("sdk60_alice").await?;
    let bob = v2_wallet("sdk60_bob").await?;
    let carol = v2_wallet("sdk60_carol").await?;
    let bob_address = bob.get_utexo_address().await?;
    let carol_address = carol.get_utexo_address().await?;

    // ---- Alice deposits: the ONLY on-chain funding in the whole flow. ---------------------------
    let t = prepaid_token(&cc).await?;
    alice.add_prepaid_token(&t).await;
    let addr = alice.get_deposit_address(DEPOSIT).await?;
    bitcoin_core::sendtoaddress(u32::try_from(DEPOSIT)?, &addr)?;
    bitcoin_core::generatetoaddress(3, &core)?;
    let mut waited = 0;
    loop {
        alice.claim().await?;
        if alice.get_balance().await?.available_sats == DEPOSIT {
            break;
        }
        waited += 1;
        if waited > 60 {
            return Err(anyhow!("alice deposit did not confirm"));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let alice_sid = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk60_alice")
        .await?
        .coins
        .iter()
        .find(|c| c.status == mercurylib::wallet::CoinStatus::CONFIRMED && c.duplicate_index == 0)
        .and_then(|c| c.statechain_id.clone())
        .ok_or(anyhow!("alice has no confirmed coin"))?;
    let bundle = mercuryrustlib::tesr::load(&cc, "sdk60_alice", &alice_sid)
        .await?
        .ok_or(anyhow!("alice's coin must be laddered (V2)"))?;
    let (f_txid, f_vout) = (bundle.f_txid.clone(), bundle.f_vout);
    println!("SDK60 - alice deposited {DEPOSIT}; funding outpoint F = {f_txid}:{f_vout} (the only on-chain tx)");

    // ---- HOP 1: Alice -> Bob, NON-EXACT ⟹ in-ladder split, piece conveyed with the handover. -----
    let r = alice.transfer(&bob_address, PAY).await?;
    assert!(r.used_split, "a {PAY} payment from one {DEPOSIT} laddered coin must split in-ladder");
    let bob_child_sid = claim_until(&cc, &bob, "sdk60_bob", PAY).await?;
    let bob_cb = mercuryrustlib::tesr::load_child(&cc, "sdk60_bob", &bob_child_sid)
        .await?
        .ok_or(anyhow!("bob did not adopt the child bundle"))?;

    // FIRST-CLASS: the handover completed, so bob co-owns A_child and alice is locked out.
    let bob_coin = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk60_bob")
        .await?
        .coins
        .iter()
        .find(|c| c.statechain_id.as_deref() == Some(&bob_child_sid))
        .cloned()
        .ok_or(anyhow!("bob child coin missing"))?;
    assert!(bob_coin.server_pubkey.is_some(), "bob completed the key handover (holds the SE share)");
    assert!(bob_coin.signed_statechain_id.is_some(), "bob holds the SE's ownership attestation");
    // The child is NOT terminal — that is what makes the onward hop possible.
    let (_, _, child_terminal) =
        mercuryrustlib::lightning_latch::get_spend_budget(&cc, &bob_child_sid).await?;
    assert!(!child_terminal, "a plain child must be non-terminal so it can be re-transferred");
    let sigs_after_hop1 = num_sigs(&cc, &bob_child_sid).await?;
    assert_eq!(
        sigs_after_hop1, 2,
        "a fresh child has exactly its 2 tiers co-signed (CHILD_V2_BASELINE=0); the handover is census-NEUTRAL"
    );
    assert!(!is_outpoint_spent(&cc, &f_txid, f_vout), "hop 1 was OFF-CHAIN: F is still unspent");
    println!("SDK60 - HOP 1: alice -> bob {PAY} sat off-chain; bob adopted a FIRST-CLASS child (sid {bob_child_sid}, num_sigs {sigs_after_hop1}, non-terminal) ✓");

    // ---- HOP 2: Bob pays the WHOLE child onward to Carol — off-chain, no materialization. --------
    // This is the property that did not exist before: `transfer` routes a child through
    // `child_retransfer` (a fresh lower-CSV state paying carol + the replaced state disclosed).
    let r2 = bob.transfer(&carol_address, PAY).await?;
    assert!(!r2.used_split, "the whole child is handed onward — no split");
    assert_eq!(r2.total_sats, PAY, "carol is paid the whole child value");

    let carol_child_sid = claim_until(&cc, &carol, "sdk60_carol", PAY).await?;
    assert_eq!(carol_child_sid, bob_child_sid, "the SAME child statechain id moved to carol");
    let carol_cb = mercuryrustlib::tesr::load_child(&cc, "sdk60_carol", &carol_child_sid)
        .await?
        .ok_or(anyhow!("carol did not adopt the re-transferred child"))?;

    // The onward hop costs EXACTLY one co-sign and discloses EXACTLY one superseded state — which is
    // what carol's census counted (`0 + 2 tiers + 1 superseded == 3`).
    let sigs_after_hop2 = num_sigs(&cc, &carol_child_sid).await?;
    assert_eq!(sigs_after_hop2, sigs_after_hop1 + 1, "one onward hop = exactly one more co-sign");
    assert_eq!(carol_cb.child_superseded_states.len(), 1, "the replaced state is disclosed, not hidden");
    // Replace-by-lower-timelock: carol's live state must mature BEFORE the one it replaced, or the
    // superseded state could confirm first and pay bob.
    let live_csv = carol_cb.child_state.csv.ok_or(anyhow!("no live child CSV"))?;
    let old_csv = carol_cb.child_superseded_states[0].csv.ok_or(anyhow!("no superseded CSV"))?;
    assert!(
        live_csv < old_csv,
        "carol's state (CSV {live_csv}) must out-race the state it replaced (CSV {old_csv})"
    );
    // Bob no longer owns it, and the funding outpoint is STILL untouched after two payments.
    assert_eq!(bob.get_balance().await?.available_sats, 0, "bob paid the whole child away");
    assert!(!is_outpoint_spent(&cc, &f_txid, f_vout), "hop 2 was OFF-CHAIN: F is STILL unspent");
    println!("SDK60 - HOP 2: bob -> carol {PAY} sat off-chain, re-transferring the RECEIVED child (num_sigs {sigs_after_hop1} -> {sigs_after_hop2}, 1 superseded state disclosed, CSV {old_csv} -> {live_csv}) ✓");

    // Sanity: the two hops describe the same lineage (same parent, same SP output).
    assert_eq!(carol_cb.parent_statechain_id, bob_cb.parent_statechain_id, "same parent lineage");
    assert_eq!(carol_cb.sp_vout, bob_cb.sp_vout, "same SP output funds the child");

    // ---- Carol EXITS unilaterally: the value lands at her own key, on-chain. ---------------------
    let carol_exit_key = {
        let c = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk60_carol")
            .await?
            .coins
            .iter()
            .find(|c| c.statechain_id.as_deref() == Some(&carol_child_sid))
            .cloned()
            .ok_or(anyhow!("carol child coin missing"))?;
        mercurylib::transaction::get_user_backup_address(&c, "regtest".to_string())?
    };
    let mut passes = 0;
    loop {
        let st = carol.unilateral_exit(Some(vec![carol_child_sid.clone()]), None).await?;
        let s = st.into_iter().next().ok_or(anyhow!("no exit status"))?;
        if s.complete {
            println!("SDK60 - carol's child exit COMPLETE after {passes} pass(es)");
            break;
        }
        // Advance past the NEXT tier's relative timelock (plus one block to confirm the last
        // broadcast). The chain is F -> T -> X_m -> SP -> ext_child -> state_child, so each pass
        // waits out one CSV; mining a single block per pass would never mature them.
        bitcoin_core::generatetoaddress(s.wait_blocks.max(1) + 1, &core)?;
        passes += 1;
        if passes > 40 {
            return Err(anyhow!("carol's exit did not complete"));
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    bitcoin_core::generatetoaddress(2, &core)?;
    assert!(
        is_outpoint_spent(&cc, &f_txid, f_vout),
        "only carol's final exit touches the chain — F is spent at the very end"
    );
    println!("SDK60 - carol exited the twice-transferred child; F is finally spent (the ONLY on-chain spend) ✓");

    println!("SDK60 - ✓ PASS: a RECEIVED in-ladder split child is FIRST-CLASS. Value moved alice -> bob -> carol across TWO off-chain hops with the funding outpoint unspent throughout; each hop completed the SE key handover (locking the previous owner out), cost exactly one co-signature, and disclosed exactly one superseded state that the receiver's census counted and proved out-raced. Only the final unilateral exit touched the chain. This is Spark's off-chain leaf transfer / Ark's out-of-round chain, over a Mercury statechain.");
    Ok(())
}
