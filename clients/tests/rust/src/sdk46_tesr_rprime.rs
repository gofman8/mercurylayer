//! E2E (SDK_E2E=46) — **R′ census validated against the REAL SE sig count, at FIRST MEMPOOL SIGHT**
//! on the live stack.
//!
//! The R′ soundness linchpin is `se_num_sigs == tiers + superseded`, with **no flat term**: a coin's
//! only exit material is its TES-R ladder (T, X_0, S_0), co-signed the moment the funding transaction
//! is first seen in the mempool — where the flat `tx1` used to be co-signed — and nothing else is ever
//! co-signed for it at deposit. This test proves the count formula against what the actual SE
//! reports (not a unit-test mock), at the moment the rule says the ladder must exist:
//!
//!   1. A fresh coin's SE count is 0 before its funding is seen.
//!   2. One `update_coins` pass over the UNCONFIRMED funding books the coin `IN_MEMPOOL` and leaves a
//!      `tesr-<sid>` row behind; the SE count is now EXACTLY 3 (T, X_0, S_0) and there is NO flat
//!      backup row and NO absolute locktime on the coin.
//!   3. `verify_bundle` ACCEPTS the ladder at the true count with flat term 0, and REJECTS a hidden
//!      extra signature, an undercount, and a flat term of 1 (there is no deposit `tx1` to budget).
//!   4. Confirming the funding and re-running `update_coins` changes nothing: the count stays 3 and
//!      the ladder row is byte-identical — the ladder is established ONCE, at sight, never again at
//!      confirmation.
//!
//! Run with SDK_E2E=46 (needs the regtest + Mercury lockbox stack, Core 28+).

use std::{env, fs, process::Command};

use anyhow::{anyhow, Result};
use mercuryrustlib::{client_config::ClientConfig, tesr::verify_bundle, Coin, CoinStatus};

use crate::bitcoin_core;
use crate::sdk40_tesr_consensus::{mine, wait_for_address};

const COIN_SAT: u32 = 100_000;

async fn num_sigs(cc: &ClientConfig, sid: &str) -> Result<u32> {
    Ok(mercuryrustlib::utils::get_statechain_info(sid, cc)
        .await?
        .ok_or(anyhow!("no statechain_info"))?
        .num_sigs)
}

async fn coin_at(cc: &ClientConfig, wallet_name: &str, sc_address: &str) -> Result<Coin> {
    mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, wallet_name)
        .await?
        .coins
        .iter()
        .find(|c| c.aggregated_address.as_deref() == Some(sc_address))
        .cloned()
        .ok_or(anyhow!("coin not found for {sc_address}"))
}

/// A rejection is only a pass if it is THE rejection the case targets: `expect` is a substring of the
/// named census error, so a reject for any other reason fails the test instead of being laundered
/// into a green run.
fn must_reject(b: &mercuryrustlib::tesr::TesrBundle, se: u32, flat: u32, case: &str, expect: &str) -> Result<()> {
    match verify_bundle(b, se, flat) {
        Ok(()) => Err(anyhow!("SECURITY: {case} was ACCEPTED")),
        Err(e) => {
            let msg = e.to_string();
            if !msg.contains(expect) {
                return Err(anyhow!("{case} was rejected for the WRONG reason — expected {expect:?}, got: {msg}"));
            }
            println!("SDK46 - {case} correctly REJECTED: {msg}");
            Ok(())
        }
    }
}

pub async fn execute() -> Result<()> {
    let _ = Command::new("rm").arg("wallet.db").arg("wallet.db-shm").arg("wallet.db-wal").output();
    let _ = fs::remove_dir_all("./rgb-data-sdk46");
    env::set_var("ML_NETWORK", "regtest");
    let cc = mercuryrustlib::client_config::load().await;
    let wallet_name = "sdk46_owner";

    // ---- 1. A fresh coin, funded but NOT mined: the funding sits in the mempool. -------------------
    let wallet = mercuryrustlib::wallet::create_wallet(wallet_name, &cc).await?;
    mercuryrustlib::sqlite_manager::insert_wallet(&cc.pool, &wallet).await?;
    let token = mercuryrustlib::deposit::get_token(&cc).await?;
    let token_id = crate::utils::handle_token_response(&cc, &token).await?;
    let sc_address =
        mercuryrustlib::deposit::get_deposit_bitcoin_address(&cc, &wallet.name, &token_id, COIN_SAT).await?;
    let sid = coin_at(&cc, wallet_name, &sc_address)
        .await?
        .statechain_id
        .ok_or(anyhow!("no statechain_id after deposit init"))?;

    // The SE has co-signed NOTHING for this coin before its funding is seen.
    let before_sight = num_sigs(&cc, &sid).await?;
    assert_eq!(before_sight, 0, "a coin whose funding has not been seen has no co-signs at all");
    assert!(
        mercuryrustlib::tesr::load(&cc, wallet_name, &sid).await?.is_none(),
        "no ladder row may exist before the funding is seen"
    );

    let _ = bitcoin_core::sendtoaddress(COIN_SAT, &sc_address)?;
    wait_for_address(&cc, &sc_address, COIN_SAT).await?;
    // Deliberately NO mining here: the funding is in the mempool, unconfirmed.

    // ---- 2. ONE update pass over the unconfirmed funding: booked IN_MEMPOOL, laddered, 3 co-signs. --
    mercuryrustlib::coin_status::update_coins(&cc, wallet_name).await?;
    let coin = coin_at(&cc, wallet_name, &sc_address).await?;
    assert_eq!(coin.status, CoinStatus::IN_MEMPOOL, "the funding is unconfirmed, so the coin is booked IN_MEMPOOL");
    assert!(coin.utxo_txid.is_some() && coin.utxo_vout.is_some(), "the funding outpoint is recorded at sight");
    assert_eq!(coin.locktime, None, "a laddered coin carries NO absolute locktime — there is no flat backup to mature");

    let bundle = mercuryrustlib::tesr::load(&cc, wallet_name, &sid)
        .await?
        .ok_or(anyhow!("the ladder must exist while the coin is still IN_MEMPOOL — it is the coin's only exit material"))?;
    let tier_count = bundle.exit_tiers().len() as u32;
    assert_eq!(tier_count, 3, "single-level ladder = trigger + extension + state");
    assert!(bundle.superseded_states.is_empty() && bundle.superseded_extensions.is_empty(), "a fresh ladder has no superseded tiers");
    assert_eq!(bundle.f_txid, coin.utxo_txid.clone().unwrap(), "the ladder is rooted at the coin's funding outpoint");
    assert_eq!(bundle.f_vout, coin.utxo_vout.unwrap(), "the ladder is rooted at the coin's funding vout");
    assert_eq!(bundle.owner_exit_address, coin.backup_address, "the deposit ladder exits to the depositor's own key");
    assert!(
        mercuryrustlib::sqlite_manager::try_get_backup_txs(&cc.pool, wallet_name, &sid).await?.is_none(),
        "NO flat backup row may be co-signed at deposit — the ladder replaced tx1"
    );

    let at_sight = num_sigs(&cc, &sid).await?;
    println!("SDK46 - SE num_sigs before sight: {before_sight}; after the IN_MEMPOOL pass: {at_sight} ({tier_count} tiers)");
    assert_eq!(
        at_sight,
        before_sight + tier_count,
        "SE incremented by EXACTLY the tier count at first sight — the R′ count formula holds against the real SE"
    );
    assert_eq!(at_sight, 3, "the enclave count after deposit is exactly 3 (T, X, S): no flat co-sign exists");

    // ---- 3. The R′ verifier at the true SE count, flat term 0. -----------------------------------
    verify_bundle(&bundle, at_sight, 0)
        .map_err(|e| anyhow!("verify_bundle rejected the deposit ladder at the true SE count with flat term 0: {e}"))?;
    println!("SDK46 - ✓ verify_bundle ACCEPTS the sight-time ladder at the true SE count {at_sight} (flat term 0)");

    // A hidden extra co-signed state (one more than the ladder discloses) must be refused...
    must_reject(&bundle, at_sight + 1, 0, "a hidden extra co-sign (num_sigs one above the disclosure)", "possible hidden state")?;
    // ...and so must an undercount...
    must_reject(&bundle, at_sight - 1, 0, "an undercount (num_sigs one below the disclosure)", "num_sigs mismatch")?;
    // ...and so must a flat term of 1: there is no deposit `tx1` for the census to budget. A verifier
    // that still budgeted one would refuse every honest deposit — or, worse, launder one hidden
    // state per coin — which is exactly why the receiver passes 0.
    must_reject(&bundle, at_sight, 1, "a flat term of 1 (a deposit tx1 that was never co-signed)", "num_sigs mismatch")?;

    // ---- 4. Confirmation changes nothing: the ladder is established ONCE, at sight. ----------------
    let row_at_sight = serde_json::to_string(&bundle)?;
    mine(cc.confirmation_target.max(1))?;
    mercuryrustlib::coin_status::update_coins(&cc, wallet_name).await?;
    let confirmed = coin_at(&cc, wallet_name, &sc_address).await?;
    assert_eq!(confirmed.status, CoinStatus::CONFIRMED, "the funding confirmed");
    assert_eq!(confirmed.locktime, None, "confirmation adds no calendar either");
    let after_confirm = num_sigs(&cc, &sid).await?;
    assert_eq!(after_confirm, at_sight, "confirmation must not re-establish: the SE count is unchanged (no second rival ladder over F)");
    let bundle_confirmed = mercuryrustlib::tesr::load(&cc, wallet_name, &sid)
        .await?
        .ok_or(anyhow!("the ladder row vanished at confirmation"))?;
    assert_eq!(serde_json::to_string(&bundle_confirmed)?, row_at_sight, "the ladder row is byte-identical across confirmation");
    assert!(
        mercuryrustlib::sqlite_manager::try_get_backup_txs(&cc.pool, wallet_name, &sid).await?.is_none(),
        "still no flat backup row after confirmation"
    );
    verify_bundle(&bundle_confirmed, after_confirm, 0)
        .map_err(|e| anyhow!("verify_bundle rejected the confirmed coin's ladder: {e}"))?;
    println!("SDK46 - ✓ confirmation left the ladder and the SE count untouched (num_sigs={after_confirm})");

    println!("SDK46 - ✓ PASS: the ladder is the coin's exit from first mempool sight; the census is exactly tiers + superseded with flat term 0, validated against the live SE");
    Ok(())
}
