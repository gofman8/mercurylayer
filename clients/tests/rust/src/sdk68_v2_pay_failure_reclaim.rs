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
//! Re-derived for the ladder rule: the coin's ONLY exit material is its ladder — there is no flat
//! backup to fall back on and no calendar — so "restored as exitable" is measured as exactly that
//! shape, before the pay and again after the reclaim: a `tesr-` row exists, there are ZERO flat backup
//! rows (`create_tx1` is gone, and the latched hop co-signed `S'`, never a hop backup), and
//! `locktime == None`. At deposit the enclave count is exactly the 3 tiers (a 4 is a flat `tx1`); after
//! the failed pay it is 4 — the orphan `S'` — which is why the census-bound self-transfer would brick
//! and why the reclaim must not run one.
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

/// The exit-material shape the ladder rule gives every laddered coin, read the way a receiver reads
/// it: a `tesr-` row EXISTS, there are ZERO flat backup rows, and `locktime == None`.
async fn assert_ladder_shape(
    cc: &mercuryrustlib::client_config::ClientConfig,
    wallet: &str,
    sid: &str,
    what: &str,
) -> Result<mercuryrustlib::tesr::TesrBundle> {
    let bundle = mercuryrustlib::tesr::load(cc, wallet, sid)
        .await?
        .ok_or(anyhow!("{what} ({sid}) has no `tesr-` row in {wallet}: no exit material at all"))?;
    let flat_rows = mercuryrustlib::sqlite_manager::try_get_backup_txs(&cc.pool, wallet, sid)
        .await?
        .map_or(0, |rows| rows.len());
    assert_eq!(flat_rows, 0, "{what}: a laddered coin carries ZERO flat backup rows, found {flat_rows}");
    let coin = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, wallet)
        .await?
        .coins
        .into_iter()
        .find(|c| c.statechain_id.as_deref() == Some(sid) && c.duplicate_index == 0)
        .ok_or(anyhow!("{what} ({sid}) is not in wallet {wallet}"))?;
    assert_eq!(coin.locktime, None, "{what}: coin.locktime must be None for life — no absolute calendar");
    Ok(bundle)
}

/// The enclave-attested co-signature count for `sid`.
async fn num_sigs(cc: &mercuryrustlib::client_config::ClientConfig, sid: &str) -> Result<u32> {
    Ok(mercuryrustlib::utils::get_statechain_info(sid, cc)
        .await?
        .ok_or(anyhow!("no statechain info for {sid}"))?
        .num_sigs)
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

    // THE SHAPE before the pay: laddered, ZERO flat backup rows, no calendar, enclave count exactly
    // the 3 tiers (a 4 here would be a flat tx1 co-signed at deposit).
    let bundle = assert_ladder_shape(&cc, "sdk68_alice", &coin_sid, "alice's deposit").await?;
    let sigs_at_deposit = num_sigs(&cc, &coin_sid).await?;
    assert_eq!(
        sigs_at_deposit, 3,
        "alice's laddered deposit must have consumed exactly 3 co-signs (T, X, S); a 4 is a flat tx1"
    );
    mercuryrustlib::tesr::verify_bundle(&bundle, sigs_at_deposit, 0)
        .map_err(|e| anyhow!("alice's census must balance with a flat term of 0: {e}"))?;
    println!("SDK68 - alice's coin: `tesr-` row, 0 flat backup rows, locktime None, num_sigs 3 == 0 flat + 3 tiers");

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
    // "Restored as exitable" IS the ladder shape: the `tesr-` row is still there, and the failed hop
    // left behind NO flat backup (the latch co-signed S', never a receiver-paying hop backup) and no
    // calendar. The enclave count is now 4 — the orphan S' — which is exactly the count a
    // census-bound self-transfer would have bricked on.
    let _ = assert_ladder_shape(&cc, "sdk68_alice", &coin_sid, "the reclaimed coin").await?;
    let sigs_after_failure = num_sigs(&cc, &coin_sid).await?;
    assert_eq!(
        sigs_after_failure, 4,
        "the failed latched hop must have consumed exactly ONE more co-sign (the orphan S'), never a \
         hop backup: num_sigs must be 3 tiers + 1 orphan"
    );
    println!(
        "SDK68 - reclaim OK: coin restored to {} sat (exitable: `tesr-` row, 0 flat backup rows, \
         locktime None; num_sigs {sigs_after_failure} = 3 tiers + the orphan S')",
        alice_bal.available_sats
    );

    println!("SDK68 - ✓ SUCCESS: a failed whole-coin V2 Lightning pay no longer bricks the reclaim — the coin is restored as exitable (its ladder, with no flat backup and no calendar; recoverable via unilateral exit, re-transfer needs a refresh). The old self-transfer reclaim would have rejected on the orphan-inflated sig_count.");
    Ok(())
}
