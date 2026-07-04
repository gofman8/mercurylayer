//! E2E (parity): the newly-added Spark-parity SDK methods — identity message signing,
//! multi-recipient sats transfer (transferV2), Spark invoices (create + fulfill), and the
//! query/history API. Run: SDK_E2E=11 ML_NETWORK=regtest cargo run

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

    let (alice, _) = SparkWallet::initialize(SdkConfig::regtest("sdk11_alice"), None).await?;
    let (bob, _) = SparkWallet::initialize(SdkConfig::regtest("sdk11_bob"), None).await?;
    let (carol, _) = SparkWallet::initialize(SdkConfig::regtest("sdk11_carol"), None).await?;
    let bob_addr = bob.get_spark_address().await?;
    let carol_addr = carol.get_spark_address().await?;

    // --- P-B: identity message signing ---------------------------------------------------------
    let pubkey = alice.get_identity_public_key().await?;
    // identity key is STABLE across calls
    assert_eq!(pubkey, alice.get_identity_public_key().await?, "identity key is stable");
    let msg = b"spark parity attestation";
    let sig = alice.sign_message_with_identity_key(msg).await?;
    assert!(
        SparkWallet::validate_message_with_identity_key(msg, &sig, &pubkey)?,
        "valid signature verifies"
    );
    assert!(
        !SparkWallet::validate_message_with_identity_key(b"tampered", &sig, &pubkey)?,
        "tampered message fails"
    );
    println!("SDK11 - P-B: identity message sign + verify OK (stable key)");

    // --- fund alice ----------------------------------------------------------------------------
    let t = prepaid_token(&cc).await?;
    alice.add_prepaid_token(&t).await;
    let addr = alice.get_deposit_address(60_000).await?;
    bitcoin_core::sendtoaddress(60_000, &addr)?;
    let core = bitcoin_core::getnewaddress()?;
    bitcoin_core::generatetoaddress(3, &core)?;
    let mut waited = 0;
    while alice.get_balance().await?.available_sats != 60_000 {
        alice.claim().await?;
        waited += 1;
        if waited > 60 {
            return Err(anyhow!("deposit did not confirm"));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    println!("SDK11 - alice funded (60k)");

    // --- P-D: multi-recipient sats transfer ----------------------------------------------------
    for _ in 0..3 {
        let t = prepaid_token(&cc).await?;
        alice.add_prepaid_token(&t).await;
    }
    let results = alice
        .transfer_many(&[(bob_addr.clone(), 10_000), (carol_addr.clone(), 15_000)])
        .await?;
    assert_eq!(results.len(), 2);
    assert_eq!(bob.claim().await?.claimed_transfers, 1, "bob claims his piece");
    assert_eq!(carol.claim().await?.claimed_transfers, 1, "carol claims her piece");
    assert_eq!(bob.get_balance().await?.available_sats, 10_000, "bob got 10k");
    assert_eq!(carol.get_balance().await?.available_sats, 15_000, "carol got 15k");
    println!("SDK11 - P-D: one split -> bob 10k + carol 15k (multi-recipient)");

    // --- P-E: Spark invoice create + fulfill ---------------------------------------------------
    for _ in 0..2 {
        let t = prepaid_token(&cc).await?;
        alice.add_prepaid_token(&t).await;
    }
    let invoice = carol.create_sats_invoice(5_000, Some("for coffee".into()), None).await?;
    assert!(invoice.starts_with("sparkinv1"));
    // Expired invoice is refused.
    let expired = carol.create_sats_invoice(1, None, Some(1)).await?;
    assert!(alice.fulfill_spark_invoice(&expired).await.is_err(), "expired invoice refused");
    // Valid invoice pays.
    alice.fulfill_spark_invoice(&invoice).await?;
    assert_eq!(carol.claim().await?.claimed_transfers, 1, "carol claims the invoice payment");
    assert_eq!(carol.get_balance().await?.available_sats, 20_000, "carol 15k + 5k invoice");
    println!("SDK11 - P-E: Spark invoice created + fulfilled (expired one refused)");

    // --- P-A/F: query API ----------------------------------------------------------------------
    let coins = alice.list_coins().await?;
    assert!(!coins.is_empty(), "alice has coins");
    let transfers = alice.get_transfers().await?;
    assert!(transfers.iter().any(|t| t.action == "Transfer"), "transfer history present");
    let quote = alice.get_withdrawal_fee_quote(None).await?;
    assert!(quote.fee_sats > 0 && quote.est_vbytes > 0, "fee quote populated: {quote:?}");
    println!(
        "SDK11 - P-A/F: {} coins, {} transfer-history entries, withdraw fee quote {} sats @ {:.1} sat/vB",
        coins.len(), transfers.len(), quote.fee_sats, quote.fee_rate_sat_vb
    );

    println!("SDK11 - SUCCESS: identity signing, multi-recipient sats transfer, Spark invoices (create/fulfill/expiry), and the query/history + fee-quote API all work. Parity gaps closed.");
    Ok(())
}
