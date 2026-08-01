//! E2E (SDK_E2E=76) — **splitting a RECEIVED laddered coin: the child is adoptable**.
//!
//! The regression for the `PARENT_V2_BASELINE` defect. `verify_child_bundle`'s ancestor census is
//! `num_sigs(parent) == flat_backups + tiers + superseded`, and `flat_backups` used to be supplied
//! by the receiver as the CONSTANT `PARENT_V2_BASELINE = 1`. That constant is the count of a coin
//! this wallet DEPOSITED. Every whole-coin hop co-signs one more flat backup
//! (`transfer_sender::create_backup_tx_to_receiver`), so a parent received `k` times carries `1 + k`
//! and the census came up exactly `k` short — an in-ladder split of a RECEIVED laddered coin minted
//! a child that NO receiver could ever adopt, and it did so AFTER terminalizing the parent and
//! booking the piece WITHDRAWN. Fail-closed, but unrecoverable through any supported path.
//!
//! Every existing in-ladder-split E2E is structurally blind to this: sdk58, sdk59 and sdk69 all
//! DEPOSIT the parent, so `k = 0` and the constant is accidentally correct. This test is the one
//! that puts a hop in front of the split:
//!
//!   1. alice deposits and `claim()` ladders the coin — `flat_backups = 1`;
//!   2. alice transfers the WHOLE coin to bob; bob claims it. Bob's coin is now a RECEIVED laddered
//!      coin with `flat_backups = 2` — asserted from bob's own backup rows, so the premise of the
//!      whole test is measured, not assumed;
//!   3. bob pays carol a NON-EXACT amount, which routes through the in-ladder split. On the old code
//!      this REFUSED outright ("this coin was RECEIVED rather than deposited by this wallet");
//!   4. **carol claims and ADOPTS the child** — the property under test;
//!   5. carol exits the child unilaterally and the sats land at her own key.
//!
//! Plus the NEGATIVE CONTROL that keeps the test from being vacuous: the very bundle carol adopted
//! is re-verified with the ancestor `flat_backups` forced back to `PARENT_V2_BASELINE`, and that
//! MUST be rejected with the census's own "num_sigs mismatch" message. If the control ever passes,
//! this test has stopped exercising the defect.
//!
//! Run: SDK_E2E=76 ML_NETWORK=regtest cargo +stable run

use std::time::Duration;

use anyhow::{anyhow, Result};
use electrum_client::ElectrumApi;
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};

use crate::bitcoin_core;

const DEPOSIT: u64 = 100_000;
/// Non-exact w.r.t. every coin bob holds, so `transfer()` must split rather than hand over a whole
/// coin — that is the path this test exists to reach.
const PAY: u64 = 30_000;

async fn prepaid_token(cc: &mercuryrustlib::client_config::ClientConfig) -> Result<String> {
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    crate::utils::handle_token_response(cc, &token).await
}

async fn ladder_wallet(name: &str) -> Result<UtexoWallet> {
    let (w, _) = UtexoWallet::initialize(SdkConfig::regtest(name), None).await?;
    Ok(w)
}

async fn num_sigs(cc: &mercuryrustlib::client_config::ClientConfig, sid: &str) -> Result<u32> {
    Ok(mercuryrustlib::utils::get_statechain_info(sid, cc)
        .await?
        .ok_or(anyhow!("no statechain info for {sid}"))?
        .num_sigs)
}

async fn aggregate(
    cc: &mercuryrustlib::client_config::ClientConfig,
    sid: &str,
) -> Result<Option<String>> {
    Ok(mercuryrustlib::utils::get_statechain_info(sid, cc)
        .await?
        .ok_or(anyhow!("no statechain info for {sid}"))?
        .aggregate_pubkey)
}

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    let cc = mercuryrustlib::client_config::load().await;
    let core = bitcoin_core::getnewaddress()?;

    let alice = ladder_wallet("sdk76_alice").await?;
    let bob = ladder_wallet("sdk76_bob").await?;
    let carol = ladder_wallet("sdk76_carol").await?;
    let bob_address = bob.get_utexo_address().await?;
    let carol_address = carol.get_utexo_address().await?;

    // ---- 1. alice deposits; claim() auto-establishes the ladder. --------------------------------
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
            return Err(anyhow!("alice's deposit did not confirm"));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let alice_sid = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk76_alice")
        .await?
        .coins
        .iter()
        .find(|c| c.status == mercurylib::wallet::CoinStatus::CONFIRMED && c.duplicate_index == 0)
        .and_then(|c| c.statechain_id.clone())
        .ok_or(anyhow!("alice has no confirmed coin"))?;
    assert!(
        mercuryrustlib::tesr::load(&cc, "sdk76_alice", &alice_sid).await?.is_some(),
        "alice's coin must be laddered — otherwise this exercises the plain-BTC lane"
    );
    let alice_flat =
        mercuryrustlib::sqlite_manager::get_backup_txs(&cc.pool, "sdk76_alice", &alice_sid)
            .await?
            .len();
    assert_eq!(
        alice_flat,
        mercuryrustlib::tesr::PARENT_V2_BASELINE as usize,
        "a DEPOSITED laddered coin carries exactly the baseline flat backup — this is the case the \
         old constant described, and the case sdk58/59/69 all test"
    );
    println!(
        "SDK76 - alice deposited {DEPOSIT} and laddered it (sid {alice_sid}); flat backups = {alice_flat}"
    );

    // ---- 2. THE HOP the other split E2Es never make: the WHOLE coin moves to bob. ----------------
    let r = alice.transfer(&bob_address, DEPOSIT).await?;
    assert!(!r.used_split, "an exact-amount payment must hand over the WHOLE coin, not split it");
    let mut waited = 0;
    let bob_sid = loop {
        bob.claim().await?;
        if bob.get_balance().await?.available_sats == DEPOSIT {
            break mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk76_bob")
                .await?
                .coins
                .iter()
                .find(|c| {
                    c.status == mercurylib::wallet::CoinStatus::CONFIRMED
                        && c.amount == Some(DEPOSIT as u32)
                })
                .and_then(|c| c.statechain_id.clone())
                .ok_or(anyhow!("bob has no confirmed whole coin"))?;
        }
        waited += 1;
        if waited > 60 {
            return Err(anyhow!("bob did not receive the whole coin"));
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    };
    assert!(
        mercuryrustlib::tesr::load(&cc, "sdk76_bob", &bob_sid).await?.is_some(),
        "bob's received coin must still be laddered"
    );

    // THE PREMISE, MEASURED. One whole-coin hop == one extra flat backup. If this ever reads 1 the
    // hop stopped happening and the rest of the test would be testing sdk59 again.
    let bob_flat = mercuryrustlib::sqlite_manager::get_backup_txs(&cc.pool, "sdk76_bob", &bob_sid)
        .await?
        .len();
    assert_eq!(
        bob_flat, alice_flat + 1,
        "a RECEIVED laddered coin carries 1 + k flat backups after k hops; got {bob_flat} after 1 hop"
    );
    assert!(
        bob_flat as u32 > mercuryrustlib::tesr::PARENT_V2_BASELINE,
        "the received parent must be OUTSIDE the baseline constant — that gap IS the defect"
    );
    println!(
        "SDK76 - bob RECEIVED the whole coin (sid {bob_sid}); flat backups = {bob_flat} \
         (baseline constant is {})",
        mercuryrustlib::tesr::PARENT_V2_BASELINE
    );

    // ---- 3. bob splits the RECEIVED coin in-ladder to pay carol. ---------------------------------
    // On the old code this returned "in-ladder split refused: this coin holds 2 flat backup
    // transaction(s) but a split child's receiver censuses the ancestor segment at
    // PARENT_V2_BASELINE = 1"; before THAT it silently minted an unadoptable child.
    let r = bob.transfer(&carol_address, PAY).await?;
    assert!(
        r.used_split,
        "a {PAY} payment out of a single {DEPOSIT} laddered coin must take the in-ladder split"
    );
    assert_eq!(r.total_sats, PAY, "the payment total is the piece amount");
    println!("SDK76 - bob split his RECEIVED laddered coin in-ladder and paid carol {PAY}");

    // ---- 4. THE PROPERTY: carol ADOPTS the child. ------------------------------------------------
    let mut waited = 0;
    let carol_child_sid = loop {
        carol.claim().await?;
        let bal = carol.get_balance().await?;
        if bal.available_sats == PAY {
            break mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk76_carol")
                .await?
                .coins
                .iter()
                .find(|c| {
                    c.status == mercurylib::wallet::CoinStatus::CONFIRMED
                        && c.amount == Some(PAY as u32)
                })
                .and_then(|c| c.statechain_id.clone())
                .ok_or(anyhow!("carol has no confirmed child coin"))?;
        }
        waited += 1;
        if waited > 60 {
            return Err(anyhow!(
                "carol could NOT adopt the child of a RECEIVED laddered parent (balance {bal:?}) \
                 — this is the PARENT_V2_BASELINE regression"
            ));
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    };
    let cb = mercuryrustlib::tesr::load_child(&cc, "sdk76_carol", &carol_child_sid)
        .await?
        .ok_or(anyhow!("carol did not persist the adopted child bundle"))?;
    println!("SDK76 - carol ADOPTED the child (sid {carol_child_sid}, {PAY} sat) — the census balanced");

    // The conveyed bundle must carry the parent's REAL chain, not a baseline stand-in.
    assert_eq!(
        cb.parent_flat_backups.len(),
        bob_flat,
        "the child bundle must convey the parent's whole flat backup chain — the receiver counts it"
    );

    // ---- 5. THE NEGATIVE CONTROL. The same bundle, censused at the OLD constant, must REJECT. -----
    // Without this the test would still pass if `verify_child_bundle` stopped censusing the ancestor
    // segment at all, which would be a far worse bug than the one being fixed.
    let f_txid = electrum_client::bitcoin::Txid::from_str(&cb.parent.f_txid)
        .map_err(|_| anyhow!("bad parent f_txid"))?;
    let f_tx = cc.electrum_client.transaction_get(&f_txid).map_err(|_| anyhow!("F not on chain"))?;
    let f_spk_hex =
        hex::encode(f_tx.output[cb.parent.f_vout as usize].script_pubkey.as_bytes());
    let p_ns = num_sigs(&cc, &cb.parent_statechain_id).await?;
    let p_agg = aggregate(&cc, &cb.parent_statechain_id).await?;
    let c_ns = num_sigs(&cc, &cb.child_statechain_id).await?;
    let c_agg = aggregate(&cc, &cb.child_statechain_id).await?;
    let (_, _, p_term) =
        mercuryrustlib::lightning_latch::get_spend_budget(&cc, &cb.parent_statechain_id).await?;
    let carol_backup_addr = {
        let coin = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk76_carol")
            .await?
            .coins
            .iter()
            .find(|c| c.statechain_id.as_deref() == Some(&carol_child_sid))
            .cloned()
            .ok_or(anyhow!("carol's child coin missing"))?;
        mercurylib::transaction::get_user_backup_address(&coin, "regtest".to_string())?
    };

    // Positive control at the REAL count: the same call carol's claim made.
    mercuryrustlib::tesr::verify_child_bundle(
        &cb,
        &f_spk_hex,
        p_ns,
        cb.parent_flat_backups.len() as u32,
        p_agg.as_deref(),
        p_term,
        c_ns,
        mercuryrustlib::tesr::CHILD_V2_BASELINE,
        c_agg.as_deref(),
        &[],
        &carol_backup_addr,
    )
    .map_err(|e| anyhow!("the child of a RECEIVED parent was REJECTED at the real count: {e}"))?;

    // Negative control at the OLD constant.
    let err = mercuryrustlib::tesr::verify_child_bundle(
        &cb,
        &f_spk_hex,
        p_ns,
        mercuryrustlib::tesr::PARENT_V2_BASELINE,
        p_agg.as_deref(),
        p_term,
        c_ns,
        mercuryrustlib::tesr::CHILD_V2_BASELINE,
        c_agg.as_deref(),
        &[],
        &carol_backup_addr,
    )
    .expect_err(
        "SECURITY/REGRESSION: censusing the ancestor segment at PARENT_V2_BASELINE must NOT accept \
         the child of a RECEIVED parent — if it does, the census is no longer exact",
    )
    .to_string();
    assert!(
        err.contains("parent segment/census invalid: num_sigs mismatch"),
        "the baseline census must fail on the PARENT census specifically, got: {err}"
    );
    println!(
        "SDK76 - negative control: the same bundle censused at PARENT_V2_BASELINE is REJECTED \
         ({err}) — the fix is load-bearing, not cosmetic"
    );

    // ---- 6. carol exits the child unilaterally; the sats land at her own key. --------------------
    let mut passes = 0;
    loop {
        let st = carol.unilateral_exit(Some(vec![carol_child_sid.clone()]), None).await?;
        let s = st.into_iter().next().ok_or(anyhow!("no exit status"))?;
        if s.complete {
            break;
        }
        bitcoin_core::generatetoaddress(s.wait_blocks.max(1) + 1, &core)?;
        passes += 1;
        if passes > 40 {
            return Err(anyhow!("carol's child exit did not complete"));
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    bitcoin_core::generatetoaddress(1, &core)?;
    let child_value = cb.child_state.out_value;
    assert!(
        crate::sdk40_tesr_consensus::wait_for_address(&cc, &carol_backup_addr, child_value as u32)
            .await
            .is_ok(),
        "the child's {child_value} sat must land at carol's own key"
    );

    println!(
        "SDK76 - ✓ PASS: a RECEIVED (transferred-once) laddered coin was split IN-LADDER and its \
         child was ADOPTED by the receiver and EXITED for {child_value} sat. The ancestor census now \
         runs on the parent's REAL conveyed flat-backup count ({bob_flat}), not the \
         PARENT_V2_BASELINE constant ({}) — which the negative control proves would still reject \
         this exact bundle.",
        mercuryrustlib::tesr::PARENT_V2_BASELINE
    );
    Ok(())
}

use std::str::FromStr;
