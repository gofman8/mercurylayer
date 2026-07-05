//! E2E (failure path): Mercury → Lightning PAY that cannot be routed, then RECLAIM. The user
//! latch-transfers the coin to the SSP, but the SSP's Lightning payment FAILS (the invoice exceeds
//! the SSP node's outbound liquidity, so there is no route). Safety invariants: the SSP never
//! reveals a preimage and never claims the coin; the coin stays the user's and, once the SE
//! batch_timeout (120s) elapses, `reclaim_lightning_payment` returns full custody. This is the
//! trustless "no payment → coin stays yours" guarantee, end-to-end.
//!
//! Run: SDK_E2E=18 ML_NETWORK=regtest RLN_REGTEST=.../regtest.sh cargo run  (>2min: crosses 120s)

use anyhow::{anyhow, Result};
use mercury_spark_sdk::ssp::{RlnClient, SspService};
use mercury_spark_sdk::{SdkConfig, SparkWallet};
use std::time::Duration;

use crate::{bitcoin_core, rln};

async fn prepaid_token(cc: &mercuryrustlib::client_config::ClientConfig) -> Result<String> {
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    crate::utils::handle_token_response(cc, &token).await
}

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    let cc = mercuryrustlib::client_config::load().await;

    // merchant = A, SSP = B. A pushes 300k to B, so B's outbound is ~300k.
    let (merchant_node, ssp_node) = rln::setup_ln_pair("/tmp/rln-sdk18").await?;
    println!("SDK18 - LN pair up (SSP outbound ~300k)");

    let (ssp_wallet, _) = SparkWallet::initialize(SdkConfig::regtest("sdk18_ssp"), None).await?;
    let ssp = SspService::new(ssp_wallet, RlnClient::new(&ssp_node.api), 0);

    // alice funded 500k so she can mint a 400k coin.
    let (alice, _) = SparkWallet::initialize(SdkConfig::regtest("sdk18_alice"), None).await?;
    let t = prepaid_token(&cc).await?;
    alice.add_prepaid_token(&t).await;
    let addr = alice.get_deposit_address(500_000).await?;
    bitcoin_core::sendtoaddress(500_000, &addr)?;
    let core = bitcoin_core::getnewaddress()?;
    bitcoin_core::generatetoaddress(3, &core)?;
    let mut waited = 0;
    while alice.get_balance().await?.available_sats != 500_000 {
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
    let alice_pre = alice.get_balance().await?.available_sats;
    println!("SDK18 - alice funded: {alice_pre} sats");

    // A 400k invoice EXCEEDS the SSP's ~300k outbound → unroutable → the pay must fail.
    let invoice = merchant_node.ln_invoice(400_000_000, None, 3600).await?;
    let (_, invoice_hash) = ssp.rln.decode_invoice(&invoice).await?;
    println!("SDK18 - merchant issued a 400k invoice (exceeds SSP outbound)");

    // pay_lightning_invoice_reclaimable latches the coin, then the SSP tries (and fails) to pay.
    let result = alice.pay_lightning_invoice_reclaimable(&ssp, &invoice).await;
    let (coin_id, err) = match result {
        Ok(_) => return Err(anyhow!("the over-capacity payment unexpectedly SUCCEEDED")),
        Err((cid, e)) => (cid, e),
    };
    assert!(!coin_id.is_empty(), "the failure must carry the latched coin id for reclaim");
    println!("SDK18 - pay failed as expected (coin {coin_id}): {err}");

    // The SSP paid nothing and claimed nothing.
    assert_eq!(
        ssp.wallet.get_balance().await?.available_sats,
        0,
        "the SSP must not have claimed the latched coin"
    );
    assert_ne!(
        merchant_node.invoice_status(&invoice).await?,
        "Succeeded",
        "the invoice must NOT have settled"
    );
    let (pay_status, _) = ssp.rln.payment(&invoice_hash).await.unwrap_or(("None".into(), None));
    assert_ne!(pay_status, "Succeeded", "no outbound payment from the SSP");

    let alice_latched = alice.get_balance().await?.available_sats;
    assert!(
        alice_latched < alice_pre,
        "the coin is latched (in transfer), so available dropped ({alice_latched} < {alice_pre})"
    );
    println!("SDK18 - coin is latched to the SSP; alice available now {alice_latched}");

    // Reclaim: after the batch_timeout (120s) the latch no longer holds; re-take custody.
    println!("SDK18 - waiting out the 120s batch_timeout before reclaim...");
    tokio::time::sleep(Duration::from_secs(125)).await;
    alice.reclaim_lightning_payment(&coin_id).await?;
    // Let the reclaimed coin settle back in.
    for _ in 0..15 {
        alice.claim().await?;
        if alice.get_balance().await?.available_sats >= 480_000 {
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let alice_post = alice.get_balance().await?.available_sats;
    assert!(
        alice_post >= 480_000,
        "after reclaim alice's coin must be spendable again (got {alice_post}, expected ~{alice_pre})"
    );
    println!("SDK18 - reclaimed: alice available back to {alice_post} sats");

    println!("SDK18 - SUCCESS: an unroutable pay failed, the SSP revealed no preimage and claimed nothing, and alice reclaimed her coin after the batch_timeout. Trustless: no payment → the coin stays yours.");
    Ok(())
}
