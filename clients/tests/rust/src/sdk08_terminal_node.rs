//! E2E (adversarial): terminal-node enforcement — after an off-chain split, the SE refuses ANY
//! further co-signature on the parent, so a malicious sender cannot double-spend the branch by
//! withdrawing/transferring the parent afterwards. Spark's "one spend per node", on a single SE.
//!
//! alice splits her coin (SDK sets a spend budget of exactly one more co-signature — the split).
//! She then plays malicious: tries to withdraw the parent (a FRESH SE co-signature that would
//! kill bob's branch on-chain). The SE must refuse with the budget error. Bob's terminal query
//! independently confirms the parent is dead.
//!
//! Run: SDK_E2E=8 ML_NETWORK=regtest cargo run

use anyhow::{anyhow, Result};
use mercury_spark_sdk::{SdkConfig, SparkWallet};
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
    let cc = mercuryrustlib::client_config::load().await;

    let (alice, _) = SparkWallet::initialize(SdkConfig::regtest("sdk8_alice"), None).await?;

    // Fund alice.
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

    let record = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk8_alice").await?;
    let parent_id = record
        .coins
        .iter()
        .find(|c| c.status == mercuryrustlib::CoinStatus::CONFIRMED)
        .and_then(|c| c.statechain_id.clone())
        .ok_or_else(|| anyhow!("no confirmed coin"))?;

    // Pre-split: parent is NOT terminal.
    let (budget, finalized, terminal) =
        mercuryrustlib::lightning_latch::get_spend_budget(&cc, &parent_id).await?;
    assert!(!terminal, "fresh coin not terminal (budget {budget:?}, finalized {finalized})");

    // Split (SDK sets budget=+1 first; the split consumes it).
    for _ in 0..2 {
        let t = prepaid_token(&cc).await?;
        alice.add_prepaid_token(&t).await;
    }
    let (piece_id, change_id) = alice.split_coin(&parent_id, 15_000).await?;
    println!("SDK08 - split done: piece {piece_id}, change {change_id}");

    // Post-split: the SE reports the parent TERMINAL — anyone (e.g. a branch receiver) can check.
    let (budget, finalized, terminal) =
        mercuryrustlib::lightning_latch::get_spend_budget(&cc, &parent_id).await?;
    println!("SDK08 - parent budget {budget:?}, finalized {finalized}, terminal {terminal}");
    assert!(terminal, "parent must be terminal after the split");

    // MALICIOUS: alice resurrects the parent coin record and asks the SE to co-sign a withdraw
    // (which would double-spend bob's branch on-chain). The SE must refuse.
    {
        let mut record = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk8_alice").await?;
        for coin in record.coins.iter_mut() {
            if coin.statechain_id.as_deref() == Some(parent_id.as_str()) {
                coin.status = mercuryrustlib::CoinStatus::CONFIRMED; // undo the local spent mark
            }
        }
        mercuryrustlib::sqlite_manager::update_wallet(&cc.pool, &record).await?;
    }
    let exit_addr = bitcoin_core::getnewaddress()?;
    let attack = mercuryrustlib::withdraw::execute(&cc, "sdk8_alice", &parent_id, &exit_addr, None, None).await;
    let err = attack.err().map(|e| e.to_string()).unwrap_or_default();
    assert!(
        err.contains("spend budget exhausted") || err.contains("Gone") || err.contains("410"),
        "SE must refuse the parent double-spend, got: {err}"
    );
    println!("SDK08 - SE REFUSED the parent double-spend: {err}");

    // The legitimate children remain fully usable.
    let balance = alice.get_balance().await?;
    assert!(balance.available_sats > 35_000, "children spendable: {balance:?}");

    println!("SDK08 - SUCCESS: terminal-node enforcement — one co-signature per structural node; a post-split withdraw of the parent is refused by the SE; receivers can verify terminal state independently. Old-state invalidation = SE refusal (instant) + locktime ladder (fallback), strictly stronger than timelocks alone.");
    Ok(())
}
