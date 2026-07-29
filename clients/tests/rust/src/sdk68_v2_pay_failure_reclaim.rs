//! E2E (SDK_E2E=68) — **V2 whole-coin exact-PAY failure → clean reclaim** (LIGHTNING.md §STATUS).
//!
//! Closes the flagged SE-reconcile gap on the failure path. When a V2 wallet pays a BOLT11 whose amount
//! exactly equals one of its coins, `pay_lightning_invoice` uses the WHOLE-coin latch (no split). If the
//! SSP cannot route the payment, the coin was latch-transferred and the orphan `S'` co-sign inflates its
//! `sig_count`, so the old reclaim (a self-transfer) BRICKED on `verify_bundle`. `reclaim_lightning_payment`
//! now detects a V2 coin and restores it locally as exitable instead — the value is fully recoverable
//! (the ladder is intact), and re-transfer is orphan-bricked only until a `refresh()`. This proves no LN
//! pay failure leaves the coin stuck.
//!
//! Run: SDK_E2E=68 ML_NETWORK=regtest RLN_REGTEST=.../regtest.sh cargo run

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

    // merchant = A, SSP = B. A pushes ~300k to B → B outbound ~300k.
    let (merchant_node, ssp_node) = rln::setup_ln_pair("/tmp/rln-sdk68").await?;
    println!("SDK68 - LN pair up (SSP outbound ~300k)");

    let (ssp_wallet, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk68_ssp"), None).await?;
    let ssp = SspService::new(ssp_wallet, RlnClient::new(&ssp_node.api), 0);

    // Alice deposits EXACTLY the invoice amount as a single V2 coin ⟹ the whole-coin latch (no split).
    let amount: u64 = 400_000;
    let (alice, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk68_alice"), None).await?;
    let t = prepaid_token(&cc).await?;
    alice.add_prepaid_token(&t).await;
    let addr = alice.get_deposit_address(amount).await?;
    bitcoin_core::sendtoaddress(amount as u32, &addr)?;
    let core = bitcoin_core::getnewaddress()?;
    bitcoin_core::generatetoaddress(3, &core)?;
    let mut waited = 0;
    while alice.get_balance().await?.available_sats != amount {
        alice.claim().await?;
        waited += 1;
        if waited > 60 {
            return Err(anyhow!("deposit did not confirm"));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let coin_sid = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk68_alice")
        .await?
        .coins
        .iter()
        .find(|c| c.status == mercurylib::wallet::CoinStatus::CONFIRMED && c.duplicate_index == 0)
        .and_then(|c| c.statechain_id.clone())
        .ok_or(anyhow!("alice has no confirmed coin"))?;
    println!("SDK68 - alice funded: an exact {amount}-sat V2 coin (sid {coin_sid})");

    // A 400k invoice == the whole coin AND exceeds the SSP's ~300k outbound ⟹ whole-coin latch, then
    // the SSP's send_payment fails.
    let invoice = merchant_node.ln_invoice(amount * 1000, None, 3600).await?;
    println!("SDK68 - merchant issued a {amount} invoice (exceeds SSP outbound)");

    let result = alice.pay_lightning_invoice_reclaimable(&ssp, &invoice).await;
    let (coin_id, err) = match result {
        Ok(_) => return Err(anyhow!("the over-capacity whole-coin pay unexpectedly SUCCEEDED")),
        Err((cid, e)) => (cid, e),
    };
    assert!(!coin_id.is_empty(), "the failure must carry the latched coin id for reclaim");
    let msg = err.to_string();
    assert!(
        msg.contains("lightning payment failed") || msg.contains("did not settle"),
        "the failure must be an LN routing failure, got: {msg}"
    );
    assert!(!msg.contains("refusing to pay"), "must not be the pre-payment gate, got: {msg}");
    assert_ne!(merchant_node.invoice_status(&invoice).await?, "Succeeded", "invoice must NOT settle");
    println!("SDK68 - whole-coin pay failed as expected (coin {coin_id}): {err}");

    // Reclaim: for a V2 coin this restores the coin locally as exitable (no bricking self-transfer). The
    // LN failure positively confirms non-payment, so it is safe to reclaim immediately.
    alice.reclaim_lightning_payment(&coin_id).await?;
    mercuryrustlib::coin_status::update_coins(&cc, "sdk68_alice").await?;
    let alice_bal = alice.get_balance().await?;
    assert_eq!(
        alice_bal.available_sats, amount,
        "after reclaim alice's coin must be restored spendable (got {}, expected {amount})",
        alice_bal.available_sats
    );
    assert!(
        mercuryrustlib::tesr::load(&cc, "sdk68_alice", &coin_sid).await?.is_some(),
        "the reclaimed coin must still carry its exit ladder"
    );
    println!("SDK68 - reclaim OK: coin restored to {} sat (exitable)", alice_bal.available_sats);

    println!("SDK68 - ✓ SUCCESS: a failed whole-coin V2 Lightning pay no longer bricks the reclaim — the coin is restored as exitable (recoverable via unilateral exit; re-transfer needs a refresh). The old self-transfer reclaim would have rejected on the orphan-inflated sig_count.");
    Ok(())
}
