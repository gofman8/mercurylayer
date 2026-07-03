//! E2E: mercury-spark-sdk Lightning-swap legs — Spark's SSP preimage swap on the Mercury latch.
//!
//! alice swaps a statechain coin to bob (the LSP role) locked on an SE-held preimage:
//! bob's claim stays LOCKED until alice settles (simulating "Lightning payment received"), then
//! bob's watcher claims the coin and alice's revealed preimage matches the payment hash — the
//! HTLC-settlement invariant. Run: SDK_E2E=3 ML_NETWORK=regtest cargo run

use anyhow::{anyhow, Result};
use mercury_spark_sdk::{SdkConfig, SparkWallet, WalletEvent};
use sha2::{Digest, Sha256};
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

    let (alice, _) = SparkWallet::initialize(SdkConfig::regtest("sdk3_alice"), None).await?;
    let (bob, _) = SparkWallet::initialize(SdkConfig::regtest("sdk3_bob"), None).await?;
    let bob_address = bob.get_spark_address().await?;

    // alice deposits one coin.
    let t = prepaid_token(&cc).await?;
    alice.add_prepaid_token(&t).await;
    let addr = alice.get_deposit_address(50_000).await?;
    bitcoin_core::sendtoaddress(50_000, &addr)?;
    let core = bitcoin_core::getnewaddress()?;
    bitcoin_core::generatetoaddress(3, &core)?;
    let mut waited = 0;
    loop {
        alice.claim().await?;
        if alice.get_balance().await?.available_sats == 50_000 {
            break;
        }
        waited += 1;
        if waited > 60 {
            return Err(anyhow!("deposit did not confirm"));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    println!("SDK03 - alice funded with 50k sats");

    // --- Swap leg 1: alice latch-transfers the coin to bob, locked on the SE preimage ----------
    let swap = alice.start_lightning_swap(&bob_address, None).await?;
    println!("SDK03 - swap started: batch {}, payment hash {}", swap.batch_id, swap.payment_hash);

    // Bob (LSP) verifies the hash with the SE before paying the Lightning invoice.
    let se_hash = bob
        .get_swap_payment_hash(&swap.batch_id)
        .await?
        .ok_or_else(|| anyhow!("SE does not know the batch"))?;
    assert_eq!(se_hash, swap.payment_hash, "SE-registered hash matches");
    println!("SDK03 - bob verified the payment hash with the SE");

    // --- While unsettled, bob's claim is LOCKED ------------------------------------------------
    let locked = bob.claim().await?;
    assert_eq!(locked.claimed_transfers, 0, "claim locked before settlement");
    let bob_balance = bob.get_balance().await?;
    assert_eq!(bob_balance.available_sats, 0);
    println!("SDK03 - bob's claim is latch-LOCKED before the Lightning payment");

    // --- Settlement: 'Lightning payment arrived' -> alice unlocks + reveals the preimage -------
    let mut bob_events = bob.subscribe();
    let bob_bg = bob.start_background();
    let preimage = alice.settle_lightning_swap(&swap).await?;
    let digest = hex::encode(Sha256::digest(hex::decode(&preimage)?));
    assert_eq!(digest, swap.payment_hash, "preimage settles the payment hash");
    println!("SDK03 - alice settled: preimage matches the payment hash (HTLC-settleable)");

    let claimed = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            match bob_events.recv().await {
                Ok(WalletEvent::TransferClaimed { statechain_ids }) => break statechain_ids,
                Ok(_) => continue,
                Err(e) => panic!("event stream closed: {e}"),
            }
        }
    })
    .await
    .map_err(|_| anyhow!("bob did not claim the unlocked coin in time"))?;
    bob_bg.abort();
    println!("SDK03 - bob claimed the swapped coin ({} coin)", claimed.len());
    assert_eq!(bob.get_balance().await?.available_sats, 50_000);

    println!("SDK03 - SUCCESS: latch swap = Spark preimage swap on a single SE — coin locked to a payment hash, claim gated on settlement, preimage revealed to settle the Lightning HTLC.");
    Ok(())
}
