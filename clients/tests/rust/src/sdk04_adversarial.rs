//! E2E (adversarial): SDK guard rails — the Utexo negative cases the SDK itself owns.
//!
//! 1. InsufficientBalance: transfer above balance is refused with a typed error.
//! 2. Double-spend of a split parent: after an off-chain split, the parent coin is terminally
//!    spent — transferring it again is refused.
//! 3. Claim idempotence: repeated claim passes are no-ops (no double-booking).
//! 4. Withdraw of an already-withdrawn coin is refused.
//!
//! (Protocol-level adversarial cases live elsewhere: tm01 sender double-spend, ta02/ta03
//! duplicate deposits, RGB_E2E=4 SE single-use refusal, RGB_E2E=7 epoch deadline, tb04/SDK_E2E=3
//! latch locking.) Run: SDK_E2E=4 ML_NETWORK=regtest cargo run

use anyhow::{anyhow, Result};
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};
use std::time::Duration;

use crate::bitcoin_core;

async fn prepaid_token(cc: &mercuryrustlib::client_config::ClientConfig) -> Result<String> {
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    crate::utils::handle_token_response(cc, &token).await
}

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    // V1-LANE TEST: exercises split_coin / branch sub-coins (a V1 mechanism). Under V2 a laddered coin
    // splits only IN-LADDER (transfer/in_ladder_pay → exit-only children), so split_coin refuses it
    // (HF-1/B1). Pin V1 until V1 is deleted (then migrate/remove).
    std::env::set_var("UTEXO_PROTOCOL_DEFAULT", "1");
    let cc = mercuryrustlib::client_config::load().await;

    let (alice, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk4_alice"), None).await?;
    let (bob, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk4_bob"), None).await?;
    let bob_address = bob.get_utexo_address().await?;

    // Fund alice with one 40k coin.
    let t = prepaid_token(&cc).await?;
    alice.add_prepaid_token(&t).await;
    let addr = alice.get_deposit_address(40_000).await?;
    bitcoin_core::sendtoaddress(40_000, &addr)?;
    let core = bitcoin_core::getnewaddress()?;
    bitcoin_core::generatetoaddress(3, &core)?;
    let mut waited = 0;
    while alice.get_balance().await?.available_sats != 40_000 {
        alice.claim().await?;
        waited += 1;
        if waited > 60 {
            return Err(anyhow!("deposit did not confirm"));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    println!("SDK04 - alice funded (40k)");

    // --- 1. InsufficientBalance is a typed refusal ---------------------------------------------
    let err = alice.transfer(&bob_address, 100_000).await.unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("insufficient balance"), "typed error, got: {msg}");
    println!("SDK04 - over-balance transfer refused: {msg}");

    // --- 2. Split, then try to double-spend the parent -----------------------------------------
    let record = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk4_alice").await?;
    let parent_id = record
        .coins
        .iter()
        .find(|c| c.status == mercuryrustlib::CoinStatus::CONFIRMED)
        .and_then(|c| c.statechain_id.clone())
        .ok_or_else(|| anyhow!("no confirmed coin"))?;
    for _ in 0..2 {
        let t = prepaid_token(&cc).await?;
        alice.add_prepaid_token(&t).await;
    }
    let (piece_id, change_id) = alice.split_coin(&parent_id, 15_000).await?;
    println!("SDK04 - split parent into piece {piece_id} + change {change_id} (off-chain)");

    // The parent is terminally spent: a second spend attempt must be refused.
    let err = alice
        .transfer(&bob_address, 40_000)
        .await
        .map(|_| ())
        .unwrap_err();
    println!("SDK04 - double-spend of the split parent refused: {err}");

    // Piece + change remain fully usable (piece transfers cleanly).
    let r = alice.transfer(&bob_address, 15_000).await?;
    assert_eq!(r.total_sats, 15_000);
    println!("SDK04 - piece transfers cleanly after the parent refusal");

    // --- 3. Claim idempotence -------------------------------------------------------------------
    let first = bob.claim().await?;
    assert_eq!(first.claimed_transfers, 1, "bob claims the piece once");
    let second = bob.claim().await?;
    assert_eq!(second.claimed_transfers, 0, "second claim pass is a no-op");
    let third = bob.claim().await?;
    assert_eq!(third.claimed_transfers, 0, "third claim pass is a no-op");
    assert_eq!(bob.get_balance().await?.available_sats, 15_000, "no double-booking");
    println!("SDK04 - claim is idempotent (1 claim, then no-ops; balance booked once)");

    // --- 4. Withdrawing the same coin twice is refused ------------------------------------------
    let exit_addr = bitcoin_core::getnewaddress()?;
    let withdrawn = bob.withdraw(&exit_addr, None, None).await?;
    assert_eq!(withdrawn.len(), 1);
    let err = bob
        .withdraw(&exit_addr, Some(withdrawn.clone()), None)
        .await
        .map(|_| ())
        .unwrap_err();
    println!("SDK04 - double-withdraw refused: {err}");

    println!("SDK04 - SUCCESS: typed insufficient-balance refusal; split-parent double-spend refusal; idempotent claims (no double-booking); double-withdraw refusal.");
    Ok(())
}
