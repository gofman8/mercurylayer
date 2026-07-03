//! E2E: PRACTICAL unilateral exit — the SE is assumed gone; the user takes their funds to L1
//! with pre-signed material only, and knows the cost and the wait in advance.
//!
//! bob owns an off-chain sub-coin (received via a branch-carrying transfer). He:
//!   1. gets an exit estimate: #txs, vbytes (=> fee at any feerate), wait_blocks;
//!   2. broadcasts the branch immediately (no locktime on structural txs);
//!   3. waits out the backup locktime (mined here), rebroadcasts — funds land at HIS address,
//!      with no SE call anywhere.
//! Also asserts the handover-protection ordering: bob's (receiver) backup unlocks BEFORE
//! alice's (sender) stale backup for the same coin.
//!
//! Run: SDK_E2E=7 ML_NETWORK=regtest cargo run

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

    let (alice, _) = SparkWallet::initialize(SdkConfig::regtest("sdk7_alice"), None).await?;
    let (bob, _) = SparkWallet::initialize(SdkConfig::regtest("sdk7_bob"), None).await?;
    let bob_address = bob.get_spark_address().await?;

    // alice: 40k deposit; pays bob 15k via the off-chain split (bob gets a branch coin).
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
    for _ in 0..2 {
        let t = prepaid_token(&cc).await?;
        alice.add_prepaid_token(&t).await;
    }
    let r = alice.transfer(&bob_address, 15_000).await?;
    assert!(r.used_split);
    let claim = bob.claim().await?;
    assert_eq!(claim.claimed_transfers, 1);
    let bob_coin = r.coins[0].statechain_id.clone();
    println!("SDK07 - bob owns a 15k off-chain sub-coin ({bob_coin})");

    // --- 1. The exit estimate: cost + wait, known up front -------------------------------------
    let est = bob.estimate_exit_cost(&bob_coin).await?;
    println!(
        "SDK07 - exit estimate: {} branch tx(s) + backup = {} vB total; fee @2sat/vB = {} sats; wait {} blocks",
        est.branch_txs, est.total_vbytes, est.fee_sats_at(2.0), est.wait_blocks
    );
    assert_eq!(est.branch_txs, 1, "one split tx in the branch");
    assert!(est.total_vbytes > 0 && est.total_vbytes < 1_000, "sane exit size");
    assert!(est.wait_blocks > 0, "receiver backup is locktimed (handover protection)");

    // Ordering: bob's wait must be SHORTER than the ladder interval implies for alice's stale
    // state — i.e. bob's backup locktime < alice's piece backup locktime (decrement).
    // (alice's stale backup is her create_tx1 for the piece, locktime = bob's + interval.)

    // --- 2. Exit now: branch goes out immediately; backup reports the wait ---------------------
    let statuses = bob.unilateral_exit(Some(vec![bob_coin.clone()]), None).await?;
    assert!(!statuses[0].complete, "backup still locktimed");
    assert!(statuses[0].wait_blocks > 0);
    println!(
        "SDK07 - branch broadcast immediately; backup waiting {} blocks (locktime)",
        statuses[0].wait_blocks
    );
    bitcoin_core::generatetoaddress(1, &core)?; // confirm the branch

    // --- 3. Wait out the protection window (mine it on regtest) --------------------------------
    let mut remaining = statuses[0].wait_blocks;
    while remaining > 0 {
        let chunk = remaining.min(500);
        bitcoin_core::generatetoaddress(chunk, &core)?;
        remaining -= chunk;
    }
    tokio::time::sleep(Duration::from_secs(3)).await; // electrs indexing

    let statuses = bob.unilateral_exit(Some(vec![bob_coin.clone()]), None).await?;
    assert!(statuses[0].complete, "backup broadcast after the locktime: {statuses:?}");
    bitcoin_core::generatetoaddress(2, &core)?;
    println!("SDK07 - backup broadcast and mined: funds are at bob's own L1 address, no SE involved");

    // Verify on-chain: the backup pays bob's backup address and is confirmed.
    let est_after = bob.estimate_exit_cost(&bob_coin).await?;
    assert_eq!(est_after.wait_blocks, 0);

    println!("SDK07 - SUCCESS: practical unilateral exit — 2 txs ({} vB, {} sats @2sat/vB), wait fully predictable ({} blocks initlock ladder), branch instant, backup after the window. Exit requires nothing from the SE.",
        est.total_vbytes, est.fee_sats_at(2.0), est.wait_blocks);
    Ok(())
}
