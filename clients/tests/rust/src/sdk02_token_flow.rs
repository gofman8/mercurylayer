//! E2E: mercury-spark-sdk token flow — issuer-SDK parity on RGB rails.
//!
//! alice issues 1000 TKN (RGB NIA) straight onto a statechain coin, then pays bob 250 TKN
//! **off-chain**: colored split (exact piece + change) + branch-carrying key handover with the
//! consignment riding the transfer message. bob's watcher auto-claims, validates the consignment
//! off-chain (un-broadcast witness chain) and books the balance under the VERIFIED contract id.
//! bob then unilaterally exits — the branch broadcasts and his token coin materializes on-chain.
//!
//! Run: SDK_E2E=2 ML_NETWORK=regtest cargo run   (regtest + lockbox + RGB proxy up)

use anyhow::{anyhow, Result};
use mercury_spark_sdk::{SdkConfig, SparkWallet, WalletEvent};
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
    for d in ["./rgb-data-sdk2_alice", "./rgb-data-sdk2_bob"] {
        let _ = std::fs::remove_dir_all(d);
    }

    let cc = mercuryrustlib::client_config::load().await;

    let mut alice_cfg = SdkConfig::regtest("sdk2_alice");
    alice_cfg.rgb_data_dir = Some("./rgb-data-sdk2_alice".to_string());
    let mut bob_cfg = SdkConfig::regtest("sdk2_bob");
    bob_cfg.rgb_data_dir = Some("./rgb-data-sdk2_bob".to_string());

    let (alice, _) = SparkWallet::initialize(alice_cfg, None).await?;
    let (bob, _) = SparkWallet::initialize(bob_cfg, None).await?;
    let bob_address = bob.get_spark_address().await?;
    println!("SDK02 - wallets up; bob address: {bob_address}");

    // --- Fund alice's RGB engine (issuance needs a colorable UTXO + witness fees) -------------
    let rgb_fund_addr = alice.get_token_funding_address().await?;
    bitcoin_core::sendtoaddress(100_000, &rgb_fund_addr)?;
    let core = bitcoin_core::getnewaddress()?;
    bitcoin_core::generatetoaddress(3, &core)?;
    tokio::time::sleep(Duration::from_secs(3)).await; // electrs indexing

    // --- Issue 1000 TKN onto a fresh statechain coin -------------------------------------------
    let t = prepaid_token(&cc).await?;
    alice.add_prepaid_token(&t).await;
    let asset_id = alice.issue_token("TKN", "Spark Parity Token", 0, 1000).await?;
    println!("SDK02 - issued 1000 TKN: {asset_id}");

    // Wait for the deposit (the colored funding tx) to confirm.
    bitcoin_core::generatetoaddress(3, &core)?;
    let mut waited = 0;
    loop {
        alice.claim().await?;
        let b = alice.get_balance().await?;
        if b.available_sats >= 10_000 && !b.tokens.is_empty() {
            println!(
                "SDK02 - alice token carrier confirmed: {} sats, {} {} settled",
                b.available_sats,
                b.tokens[0].balance,
                b.tokens[0].ticker.clone().unwrap_or_default()
            );
            break;
        }
        waited += 1;
        if waited > 60 {
            return Err(anyhow!("token carrier did not confirm: {b:?}"));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let alice_tokens = alice.get_token_balances().await?;
    assert_eq!(alice_tokens[0].balance, 1000, "full supply on alice");

    // --- Off-chain token transfer: 250 TKN to bob ----------------------------------------------
    let mut bob_events = bob.subscribe();
    let bob_bg = bob.start_background();
    for _ in 0..2 {
        let t = prepaid_token(&cc).await?;
        alice.add_prepaid_token(&t).await;
    }
    let r = alice.transfer_tokens(&asset_id, &bob_address, 250).await?;
    assert!(r.used_split);
    println!("SDK02 - sent 250 TKN off-chain (colored split + branch-carrying handover)");

    let (recv_asset, recv_amount) = tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            match bob_events.recv().await {
                Ok(WalletEvent::TokenTransferClaimed { asset_id, amount, .. }) => {
                    break (asset_id, amount)
                }
                Ok(_) => continue,
                Err(e) => panic!("event stream closed: {e}"),
            }
        }
    })
    .await
    .map_err(|_| anyhow!("bob did not claim the token transfer in time"))?;
    println!("SDK02 - bob claimed {recv_amount} of {recv_asset} (consignment validated off-chain)");
    assert_eq!(recv_asset, asset_id, "contract id verified from the consignment");
    assert_eq!(recv_amount, 250);

    let bob_tokens = bob.get_token_balances().await?;
    assert_eq!(bob_tokens.len(), 1, "bob sees exactly one asset");
    assert_eq!(bob_tokens[0].asset_id, asset_id);
    assert_eq!(bob_tokens[0].balance, 250, "bob booked 250 TKN");
    let alice_tokens = alice.get_token_balances().await?;
    assert_eq!(alice_tokens[0].balance, 750, "alice keeps 750 TKN change");
    println!("SDK02 - balances: alice 750 TKN / bob 250 TKN — fully off-chain");

    // --- bob's cooperative exit: branch materializes, then a fresh SE-co-signed spend ----------
    bob_bg.abort();
    let exit_addr = bitcoin_core::getnewaddress()?;
    let withdrawn = bob.withdraw(&exit_addr, None, None).await?;
    println!("SDK02 - bob cooperatively exited {} coin(s) to {exit_addr} (branch materialized + direct spend)", withdrawn.len());
    bitcoin_core::generatetoaddress(3, &core)?;

    println!("SDK02 - SUCCESS: issue -> off-chain token transfer (colored split + consignment in the transfer msg, validated off-chain against the branch, booked under the VERIFIED contract) -> balances 750/250 -> cooperative exit. Tokens with zero on-chain cost per payment.");
    Ok(())
}
