//! E2E: mercury-spark-sdk happy path — the Spark-parity UX on Mercury.
//!
//! alice deposits on-chain, then pays bob twice off-chain:
//!   1. an exact-subset amount (native key handover, any Mercury wallet could receive it),
//!   2. a non-round amount forcing the SDK's off-chain split (auto amount-making, sender-side
//!      change) — sub-coins are single-use SE-enforced.
//! bob's background watcher auto-claims. bob then exits cooperatively (withdraw to L1).
//!
//! Run: SDK_E2E=1 ML_NETWORK=regtest cargo run   (regtest + lockbox stack up)

use anyhow::{anyhow, Result};
use mercury_spark_sdk::{SdkConfig, SparkWallet, WalletEvent};
use std::time::Duration;

use crate::bitcoin_core;

async fn prepaid_token(cc: &mercuryrustlib::client_config::ClientConfig) -> Result<String> {
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    crate::utils::handle_token_response(cc, &token).await
}

pub async fn execute() -> Result<()> {
    let _ = std::fs::remove_file("wallet.db");
    let _ = std::fs::remove_file("wallet.db-shm");
    let _ = std::fs::remove_file("wallet.db-wal");

    // Plain client config only for the test-side token faucet.
    let cc = mercuryrustlib::client_config::load().await;

    let (alice, alice_mnemonic) =
        SparkWallet::initialize(SdkConfig::regtest("sdk_alice"), None).await?;
    let (bob, _) = SparkWallet::initialize(SdkConfig::regtest("sdk_bob"), None).await?;
    println!("SDK01 - wallets up (alice mnemonic: {} words)", alice_mnemonic.split_whitespace().count());

    let bob_address = bob.get_spark_address().await?;
    println!("SDK01 - bob spark address: {bob_address}");

    // --- Deposit: two coins for alice (60k + 40k) --------------------------------------------
    for amount in [60_000u64, 40_000] {
        let t = prepaid_token(&cc).await?;
        alice.add_prepaid_token(&t).await;
        let addr = alice.get_deposit_address(amount).await?;
        bitcoin_core::sendtoaddress(u32::try_from(amount)?, &addr)?;
    }
    let core = bitcoin_core::getnewaddress()?;
    bitcoin_core::generatetoaddress(3, &core)?;

    // Wait until the electrum index catches up and both deposits confirm.
    let mut waited = 0;
    loop {
        let r = alice.claim().await?;
        let b = alice.get_balance().await?;
        if b.available_sats == 100_000 {
            println!("SDK01 - alice deposits confirmed: {} sats available ({} deposits this pass)", b.available_sats, r.confirmed_deposits.len());
            break;
        }
        waited += 1;
        if waited > 60 {
            return Err(anyhow!("deposits did not confirm, balance {b:?}"));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    // --- Transfer 1: exact subset (60k) ------------------------------------------------------
    let mut bob_events = bob.subscribe();
    let bob_bg = bob.start_background();

    let r1 = alice.transfer(&bob_address, 60_000).await?;
    assert!(!r1.used_split, "60k should be an exact subset");
    assert_eq!(r1.total_sats, 60_000);
    println!("SDK01 - transfer#1 sent: 60000 sats, {} coin(s), no split", r1.coins.len());

    // bob's watcher should claim it.
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
    .map_err(|_| anyhow!("bob did not auto-claim transfer#1 in time"))?;
    println!("SDK01 - bob auto-claimed transfer#1 ({} coin(s))", claimed.len());

    let bob_balance = bob.get_balance().await?;
    assert_eq!(bob_balance.available_sats, 60_000, "bob balance after claim");

    // --- Transfer 2: non-round amount forcing the off-chain split ----------------------------
    // alice has a single 40k coin left; sending 15_000 requires split (15k piece + change).
    let t1 = prepaid_token(&cc).await?;
    let t2 = prepaid_token(&cc).await?;
    alice.add_prepaid_token(&t1).await;
    alice.add_prepaid_token(&t2).await;

    let r2 = alice.transfer(&bob_address, 15_000).await?;
    assert!(r2.used_split, "15k from a 40k coin must split");
    assert_eq!(r2.total_sats, 15_000);
    println!("SDK01 - transfer#2 sent: 15000 sats via off-chain split (branch-carrying transfer)");

    let claimed2 = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            match bob_events.recv().await {
                Ok(WalletEvent::TransferClaimed { statechain_ids }) => break statechain_ids,
                Ok(_) => continue,
                Err(e) => panic!("event stream closed: {e}"),
            }
        }
    })
    .await
    .map_err(|_| anyhow!("bob did not auto-claim transfer#2 in time"))?;
    println!("SDK01 - bob auto-claimed transfer#2 ({} coin(s))", claimed2.len());

    let bob_balance = bob.get_balance().await?;
    assert_eq!(bob_balance.available_sats, 75_000, "bob balance after both transfers");

    let alice_balance = alice.get_balance().await?;
    println!(
        "SDK01 - alice change balance: {} sats (split change, minus exit-fee reserve)",
        alice_balance.available_sats
    );
    assert!(alice_balance.available_sats > 20_000, "alice keeps the split change");

    // --- Cooperative exit: bob withdraws everything to L1 ------------------------------------
    bob_bg.abort();
    let exit_addr = bitcoin_core::getnewaddress()?;
    let withdrawn = bob.withdraw(&exit_addr, None, None).await?;
    println!("SDK01 - bob withdrew {} coin(s) to {exit_addr}", withdrawn.len());
    bitcoin_core::generatetoaddress(3, &core)?;

    let bob_after = bob.get_balance().await?;
    println!("SDK01 - bob post-withdraw balance: {bob_after:?}");
    assert_eq!(bob_after.available_sats, 0, "all bob coins should be WITHDRAWING");

    println!("SDK01 - SUCCESS: deposit -> exact-subset transfer -> auto-claim -> off-chain-split transfer (exact amount, sender change) -> auto-claim -> cooperative exit. No sats/UTXO/allocation management surfaced to the app.");
    Ok(())
}
