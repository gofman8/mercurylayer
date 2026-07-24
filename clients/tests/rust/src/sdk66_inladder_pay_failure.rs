//! E2E (SDK_E2E=66) — **NON-EXACT V2 Lightning PAY failure → clean rollback** (V2-LN-HODL.md §2b).
//!
//! The failure path of the latched in-ladder split: Alice tries to pay a BOLT11 that exceeds the SSP's
//! outbound liquidity (unroutable), so `send_payment` FAILS after the coin was already split + the piece
//! conveyed. Because the split is un-broadcast, the OPTIMISTIC booking (parent WITHDRAWN, change
//! CONFIRMED) is wrong — realizing the change means broadcasting SP, which would hand the SSP the piece
//! for nothing. `pay_lightning_invoice_inladder` must ROLL BACK: restore the parent as exitable and drop
//! the piece + optimistic change, so the user recovers the WHOLE parent. This proves no LN-pay failure
//! silently costs the user the piece.
//!
//! Run: SDK_E2E=66 ML_NETWORK=regtest RLN_REGTEST=.../regtest.sh cargo run

use anyhow::{anyhow, Result};
use mercury_utexo_sdk::ssp::{RlnClient, SspService};
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};
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

    // merchant = A, SSP = B. A pushes ~300k to B, so B's outbound is ~300k.
    let (merchant_node, ssp_node) = rln::setup_ln_pair("/tmp/rln-sdk66").await?;
    println!("SDK66 - LN pair up (SSP outbound ~300k)");

    let (ssp_wallet, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk66_ssp"), None).await?;
    let ssp = SspService::new(ssp_wallet, RlnClient::new(&ssp_node.api), 0);

    // Alice funds 500k so she can split off a 400k+ piece.
    let deposit: u64 = 500_000;
    let (alice, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk66_alice"), None).await?;
    let t = prepaid_token(&cc).await?;
    alice.add_prepaid_token(&t).await;
    let addr = alice.get_deposit_address(deposit).await?;
    bitcoin_core::sendtoaddress(deposit as u32, &addr)?;
    let core = bitcoin_core::getnewaddress()?;
    bitcoin_core::generatetoaddress(3, &core)?;
    let mut waited = 0;
    while alice.get_balance().await?.available_sats != deposit {
        alice.claim().await?;
        waited += 1;
        if waited > 60 {
            return Err(anyhow!("deposit did not confirm"));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let parent_sid = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk66_alice")
        .await?
        .coins
        .iter()
        .find(|c| c.status == mercurylib::wallet::CoinStatus::CONFIRMED && c.duplicate_index == 0)
        .and_then(|c| c.statechain_id.clone())
        .ok_or(anyhow!("alice has no confirmed coin"))?;
    println!("SDK66 - alice funded: {deposit} sats on a V2 coin (sid {parent_sid})");

    // A 400k invoice EXCEEDS the SSP's ~300k outbound → the SSP's send_payment MUST fail.
    let invoice = merchant_node.ln_invoice(400_000_000, None, 3600).await?;
    println!("SDK66 - merchant issued a 400k invoice (exceeds SSP outbound)");

    // The pay splits the parent (piece conveyed + latched), then the SSP fails to route → rollback.
    let result = alice
        .pay_lightning_invoice_inladder(&ssp, &invoice, &parent_sid)
        .await;
    let err = match result {
        Ok(_) => return Err(anyhow!("the over-capacity in-ladder pay unexpectedly SUCCEEDED")),
        Err(e) => e,
    };
    let msg = err.to_string();
    assert!(
        msg.contains("lightning payment failed") || msg.contains("did not settle"),
        "the failure must be an LN routing failure, got: {msg}"
    );
    assert!(!msg.contains("refusing to pay"), "must not be the pre-payment gate, got: {msg}");
    println!("SDK66 - in-ladder pay failed as expected: {err}");

    // The SSP paid nothing and the merchant did not settle.
    assert_ne!(
        merchant_node.invoice_status(&invoice).await?,
        "Succeeded",
        "the invoice must NOT have settled"
    );

    // ROLLBACK: the parent is restored to CONFIRMED (exitable); the optimistic piece/change are dropped,
    // so alice's whole coin value is recoverable rather than stuck at change-only.
    mercuryrustlib::coin_status::update_coins(&cc, "sdk66_alice").await?;
    let alice_bal = alice.get_balance().await?;
    assert_eq!(
        alice_bal.available_sats, deposit,
        "after rollback alice's whole parent must be spendable again (got {}, expected {deposit})",
        alice_bal.available_sats
    );
    // And the parent still carries its TES-R ladder (recoverable via unilateral exit).
    assert!(
        mercuryrustlib::tesr::load(&cc, "sdk66_alice", &parent_sid).await?.is_some(),
        "the restored parent must still carry its exit ladder"
    );
    println!("SDK66 - rollback OK: alice's parent restored to {} sat (exitable)", alice_bal.available_sats);

    println!("SDK66 - ✓ SUCCESS: an unroutable NON-EXACT in-ladder pay failed; the SSP revealed no preimage and the un-broadcast split was rolled back, restoring alice's WHOLE parent. No LN-pay failure costs the user the piece.");
    Ok(())
}
