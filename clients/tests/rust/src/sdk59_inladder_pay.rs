//! E2E (SDK_E2E=59) — **in-ladder split PAYMENT over the real SDK API: Alice pays Bob, Bob exits**.
//!
//! The end-to-end proof that the in-ladder split (sdk58's `verify_child_bundle` core) is a usable
//! payment through `UtexoWallet::transfer` / `claim` / `unilateral_exit`, NOT just a verifier. With
//! Alice's deposit auto-establishes a TES-R ladder; a
//! non-exact payment to Bob can no longer be split as plain BTC (B1), so `transfer()` runs the
//! IN-LADDER split: `SP` descends from the trigger, the PIECE child pays Bob (Model A) and is conveyed
//! to his mailbox, the CHANGE child pays Alice back. Bob's `claim()` adopts the child via
//! `verify_child_bundle` (parent F on-chain, both sids terminal); Bob then `unilateral_exit`s the child
//! and the funds land at Bob's own key. Alice keeps the change.
//!
//! This is the transfer/claim integration the split needed to make V2 the default. Run: SDK_E2E=59.

use std::time::Duration;

use anyhow::{anyhow, Result};
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};

use crate::bitcoin_core;

const DEPOSIT: u64 = 100_000;
const PAY: u64 = 30_000;

async fn prepaid_token(cc: &mercuryrustlib::client_config::ClientConfig) -> Result<String> {
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    crate::utils::handle_token_response(cc, &token).await
}

/// A V2-native SDK wallet (auto-establishes ladders at claim/deposit).
async fn v2_wallet(name: &str) -> Result<UtexoWallet> {
    let (w, _) = UtexoWallet::initialize(SdkConfig::regtest(name), None).await?;
    Ok(w)
}

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    let cc = mercuryrustlib::client_config::load().await;

    let alice = v2_wallet("sdk59_alice").await?;
    let bob = v2_wallet("sdk59_bob").await?;
    let bob_address = bob.get_utexo_address().await?;

    // --- Alice deposits a single V2 coin; claim() auto-establishes its TES-R ladder. -------------
    let t = prepaid_token(&cc).await?;
    alice.add_prepaid_token(&t).await;
    let addr = alice.get_deposit_address(DEPOSIT).await?;
    bitcoin_core::sendtoaddress(u32::try_from(DEPOSIT)?, &addr)?;
    let core = bitcoin_core::getnewaddress()?;
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
    // Confirm the coin is laddered (V2) — otherwise this would test the old plain-BTC split path.
    let alice_sid = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk59_alice")
        .await?
        .coins
        .iter()
        .find(|c| c.status == mercurylib::wallet::CoinStatus::CONFIRMED && c.duplicate_index == 0)
        .and_then(|c| c.statechain_id.clone())
        .ok_or(anyhow!("alice has no confirmed coin"))?;
    assert!(
        mercuryrustlib::tesr::load(&cc, "sdk59_alice", &alice_sid).await?.is_some(),
        "alice's coin must carry a TES-R ladder (V2) for an in-ladder split"
    );
    println!("SDK59 - alice deposited {DEPOSIT} and auto-established a ladder (sid {alice_sid})");

    // --- Alice pays Bob a NON-EXACT amount → in-ladder split (piece to Bob, change to Alice). -----
    let r = alice.transfer(&bob_address, PAY).await?;
    assert!(r.used_split, "a {PAY} payment from a single {DEPOSIT} laddered coin must split in-ladder");
    assert_eq!(r.total_sats, PAY, "the payment total is the piece amount");
    println!("SDK59 - alice paid bob {PAY} via IN-LADDER split ({} piece coin conveyed)", r.coins.len());

    // --- Bob claims: censuses the conveyed child (verify_child_bundle) then COMPLETES the standard
    // key handover, so the child becomes a first-class coin and alice is locked out of it. ----------
    let mut waited = 0;
    let bob_child_sid = loop {
        bob.claim().await?;
        let bal = bob.get_balance().await?;
        if bal.available_sats == PAY {
            break mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk59_bob")
                .await?
                .coins
                .iter()
                .find(|c| c.status == mercurylib::wallet::CoinStatus::CONFIRMED && c.amount == Some(PAY as u32))
                .and_then(|c| c.statechain_id.clone())
                .ok_or(anyhow!("bob has no confirmed child coin"))?;
        }
        waited += 1;
        if waited > 30 {
            return Err(anyhow!("bob did not adopt the split child; balance {bal:?}"));
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    };
    assert!(
        mercuryrustlib::tesr::load_child(&cc, "sdk59_bob", &bob_child_sid).await?.is_some(),
        "bob must have adopted (persisted) the child bundle"
    );

    // FIRST-CLASS: bob did not merely book an exit bundle — he COMPLETED the standard key handover,
    // so the SE rotated its share (and bob's auth key) for the child slot. The payoff is structural:
    // the coin now carries the full 2-of-2 material, and its aggregate is UNCHANGED — still exactly
    // the key `SP.out[j]` pays — which is what keeps every pre-signed child tier valid.
    let bob_child_coin = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk59_bob")
        .await?
        .coins
        .iter()
        .find(|c| c.statechain_id.as_deref() == Some(&bob_child_sid))
        .cloned()
        .ok_or(anyhow!("bob child coin missing"))?;
    assert!(bob_child_coin.server_pubkey.is_some(), "handover completed: the child carries the SE's rotated share");
    assert!(bob_child_coin.aggregated_pubkey.is_some(), "handover completed: the child has its 2-of-2 aggregate");
    assert!(bob_child_coin.signed_statechain_id.is_some(), "handover completed: bob holds the SE's ownership attestation");
    assert_eq!(bob_child_coin.status, mercurylib::wallet::CoinStatus::CONFIRMED, "an adopted child is a confirmed, spendable coin");
    // A_child INVARIANCE — the rotated aggregate must still be the address SP.out[j] pays.
    {
        use electrum_client::bitcoin as btc;
        let cb = mercuryrustlib::tesr::load_child(&cc, "sdk59_bob", &bob_child_sid).await?.unwrap();
        let sp: btc::Transaction =
            btc::consensus::deserialize(&hex::decode(&cb.parent.current().state.signed_tx)?)?;
        let sp_out = sp.output.get(cb.sp_vout as usize).ok_or(anyhow!("SP has no such output"))?;
        let want = btc::Address::from_script(&sp_out.script_pubkey, btc::Network::Regtest)?.to_string();
        assert_eq!(
            bob_child_coin.aggregated_address.as_deref(), Some(want.as_str()),
            "A_child must be INVARIANT across the handover — otherwise every pre-signed child tier is dead"
        );
    }
    // The exit ladder must NOT look like a V1 absolute-locktime coin: a child exits by relative CSV.
    // A stamped locktime here would mark the coin permanently due for re-anchor and bill every quote.
    assert!(bob_child_coin.locktime.is_none(), "a child has no absolute-locktime backup; locktime must stay None");
    println!("SDK59 - bob adopted the split child (sid {bob_child_sid}, {PAY} sat): census verified, key handover COMPLETED, A_child invariant — a FIRST-CLASS coin");

    // --- Alice keeps the change as an exitable child claim. ---------------------------------------
    let alice_change = alice.get_balance().await?.available_sats;
    assert!(alice_change > 0 && alice_change < DEPOSIT, "alice keeps split change ({alice_change})");
    println!("SDK59 - alice change balance: {alice_change} sat");

    // --- Bob unilaterally EXITS the child through its full pre-co-signed chain; Bob is paid. -------
    let bob_exit_key = {
        let bob_coin = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk59_bob")
            .await?
            .coins
            .iter()
            .find(|c| c.statechain_id.as_deref() == Some(&bob_child_sid))
            .cloned()
            .ok_or(anyhow!("bob child coin missing"))?;
        mercurylib::transaction::get_user_backup_address(&bob_coin, "regtest".to_string())?
    };
    let mut passes = 0;
    loop {
        let st = bob.unilateral_exit(Some(vec![bob_child_sid.clone()]), None).await?;
        let s = st.into_iter().next().ok_or(anyhow!("no exit status"))?;
        if s.complete {
            println!("SDK59 - bob's child exit COMPLETE after {passes} pass(es)");
            break;
        }
        // Advance past the next tier's relative-timelock (and one block to confirm the last broadcast).
        bitcoin_core::generatetoaddress(s.wait_blocks.max(1) + 1, &core)?;
        passes += 1;
        if passes > 40 {
            return Err(anyhow!("bob's child exit did not complete"));
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    bitcoin_core::generatetoaddress(1, &core)?;

    // The child's final state must have paid Bob's own key on-chain.
    use crate::sdk40_tesr_consensus::wait_for_address;
    let child_value = {
        let cb = mercuryrustlib::tesr::load_child(&cc, "sdk59_bob", &bob_child_sid).await?.unwrap();
        cb.child_state.out_value
    };
    assert!(
        wait_for_address(&cc, &bob_exit_key, child_value as u32).await.is_ok(),
        "the split child's {child_value} sat must land at Bob's own key"
    );
    println!("SDK59 - ✓ PASS: in-ladder split PAYMENT over the SDK — alice paid bob {PAY} (change kept), bob adopted + exited the child, {child_value} sat landed at bob's key. B1-safe split is a real, usable payment.");
    Ok(())
}
