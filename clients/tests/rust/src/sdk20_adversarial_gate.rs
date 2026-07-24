//! E2E (adversarial): the SSP pre-payment gate (review C2/C3) over the LIVE SE + LIVE RLN.
//!
//! The SSP must REFUSE to pay a Lightning invoice unless every coin latched to the swap is a
//! pending transfer ADDRESSED TO THE SSP and worth at least invoice+fee. This exercises the real
//! wiring `get_statechain_ids_by_batch_id` (server) + `peek_pending_transfers` (SDK) that the unit
//! test can only approximate — a live id/pending mismatch is exactly the bug class the gate closes.
//!
//! C2: attacker latches a coin addressed to a THIRD party (bob), bound to the invoice hash, then
//!     drives the SSP's execute_pay -> the coin's id is not in the SSP's pending set -> REFUSED.
//! C3: attacker latches an UNDERSIZED coin addressed to the SSP -> value < invoice+fee -> REFUSED.
//! In both cases NO Lightning payment must go out and the merchant invoice must NOT settle.
//!
//! Requires the deployed `/transfer/batch_statechains` route (P0-2).
//! Run: SDK_E2E=20 ML_NETWORK=regtest RLN_REGTEST=.../regtest.sh cargo run

use anyhow::{anyhow, Result};
use mercury_utexo_sdk::ssp::{RlnClient, SspService};
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};
use std::time::Duration;

use crate::{bitcoin_core, rln};

async fn prepaid_token(cc: &mercuryrustlib::client_config::ClientConfig) -> Result<String> {
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    crate::utils::handle_token_response(cc, &token).await
}

/// Fund `wallet` with `sats` on a confirmed statechain coin.
async fn fund(
    cc: &mercuryrustlib::client_config::ClientConfig,
    wallet: &UtexoWallet,
    sats: u64,
) -> Result<()> {
    let t = prepaid_token(cc).await?;
    wallet.add_prepaid_token(&t).await;
    let addr = wallet.get_deposit_address(sats).await?;
    bitcoin_core::sendtoaddress(sats as u32, &addr)?;
    let core = bitcoin_core::getnewaddress()?;
    bitcoin_core::generatetoaddress(3, &core)?;
    let mut waited = 0;
    while wallet.get_balance().await?.available_sats < sats {
        wallet.claim().await?;
        waited += 1;
        if waited > 60 {
            return Err(anyhow!("deposit did not confirm"));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Ok(())
}

pub async fn execute() -> Result<()> {
    // Runs on the V2 (TES-R) default. The pre-pay SSP gate under test (recipient + census + value)
    // is already V2-aware; conveying an EXACT-amount V2 coin also exercises the pre-pay verify_bundle
    // ladder census that the V1 lane could not — strengthening the test at no cost.
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    let cc = mercuryrustlib::client_config::load().await;

    // LN: merchant issues the invoices; the SSP node would pay them (but must refuse here).
    let (merchant_node, ssp_node) = rln::setup_ln_pair("/tmp/rln-sdk20").await?;
    println!("SDK20 - LN pair up");

    let (ssp_wallet, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk20_ssp"), None).await?;
    let ssp = SspService::new(ssp_wallet, RlnClient::new(&ssp_node.api), 0);
    let ssp_address = ssp.wallet.get_utexo_address().await?;

    // attacker (alice) funded; a THIRD party (bob) for the wrong-recipient case.
    let (alice, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk20_alice"), None).await?;
    let (bob, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk20_bob"), None).await?;
    let bob_address = bob.get_utexo_address().await?;
    // alice holds TWO EXACT V2 (TES-R) coins so `ensure_exact_coin(25_000)` / `(10_000)` each return a
    // coin WHOLE (early exact-match, no split — V2 refuses splitting a laddered coin [B1]). The
    // in-ladder split uses FREE derived tokens, so the old "split slots" prepaid top-up is gone.
    let t = prepaid_token(&cc).await?;
    alice.add_prepaid_token(&t).await;
    let addr = alice.get_deposit_address(25_000).await?;
    bitcoin_core::sendtoaddress(25_000, &addr)?;
    let t = prepaid_token(&cc).await?;
    alice.add_prepaid_token(&t).await;
    let addr = alice.get_deposit_address(10_000).await?;
    bitcoin_core::sendtoaddress(10_000, &addr)?;
    let core = bitcoin_core::getnewaddress()?;
    bitcoin_core::generatetoaddress(3, &core)?;
    let mut waited = 0;
    while alice.get_balance().await?.available_sats != 35_000 {
        alice.claim().await?;
        waited += 1;
        if waited > 60 {
            return Err(anyhow!("alice's exact deposits did not confirm"));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    println!("SDK20 - SSP + attacker(alice) + third-party(bob) ready");

    // ---- C2: coin addressed to bob, not the SSP -------------------------------------------------
    let invoice_c2 = merchant_node.ln_invoice(25_000_000, None, 3600).await?;
    let (_, hash_c2) = ssp.rln.decode_invoice(&invoice_c2).await?;
    let coin_c2 = alice.ensure_exact_coin(25_000).await?; // exact amount, but WRONG recipient
    let batch_c2 = mercuryrustlib::lightning_latch::create_external_hash_latch(
        alice.client_config(),
        alice.wallet_name(),
        &coin_c2,
        &hash_c2,
    )
    .await?;
    mercuryrustlib::transfer_sender::execute(
        alice.client_config(),
        &bob_address, // <-- latched to BOB, not the SSP
        alice.wallet_name(),
        &coin_c2,
        None,
        false,
        Some(batch_c2.clone()),
    )
    .await?;
    let r_c2 = ssp.execute_pay(&invoice_c2, &batch_c2).await;
    let e_c2 = r_c2.expect_err("SSP must REFUSE a coin not addressed to it (C2)");
    println!("SDK20 - C2 refused: {e_c2}");
    assert!(
        e_c2.to_string().contains("not a pending transfer addressed to the SSP"),
        "C2 must fail the recipient check, got: {e_c2}"
    );
    assert_ne!(
        merchant_node.invoice_status(&invoice_c2).await?,
        "Succeeded",
        "C2: the invoice must NOT have been paid"
    );

    // ---- C3: undersized coin addressed to the SSP -----------------------------------------------
    let invoice_c3 = merchant_node.ln_invoice(25_000_000, None, 3600).await?;
    let (_, hash_c3) = ssp.rln.decode_invoice(&invoice_c3).await?;
    let coin_c3 = alice.ensure_exact_coin(10_000).await?; // BELOW invoice (25k) + fee
    let batch_c3 = mercuryrustlib::lightning_latch::create_external_hash_latch(
        alice.client_config(),
        alice.wallet_name(),
        &coin_c3,
        &hash_c3,
    )
    .await?;
    mercuryrustlib::transfer_sender::execute(
        alice.client_config(),
        &ssp_address, // correctly addressed to the SSP this time...
        alice.wallet_name(),
        &coin_c3,
        None,
        false,
        Some(batch_c3.clone()),
    )
    .await?;
    let r_c3 = ssp.execute_pay(&invoice_c3, &batch_c3).await;
    let e_c3 = r_c3.expect_err("SSP must REFUSE an undersized coin (C3)");
    println!("SDK20 - C3 refused: {e_c3}");
    let m_c3 = e_c3.to_string();
    // Pin the OBSERVED numbers so an amount-read-as-0 regression cannot false-pass the amount gate
    // (a coin read as 0 would still be "below the required", but the true value must be seen). ([6])
    assert!(
        m_c3.contains("below the required") && m_c3.contains("10000") && m_c3.contains("25000"),
        "C3 must report the true undersized value (10000) vs required (25000), got: {m_c3}"
    );
    assert_ne!(
        merchant_node.invoice_status(&invoice_c3).await?,
        "Succeeded",
        "C3: the invoice must NOT have been paid"
    );

    // The SSP sent no Lightning money on either attack.
    let (st_c2, _) = ssp.rln.payment(&hash_c2).await.unwrap_or(("None".into(), None));
    let (st_c3, _) = ssp.rln.payment(&hash_c3).await.unwrap_or(("None".into(), None));
    assert_ne!(st_c2, "Succeeded", "C2: no outbound payment from the SSP");
    assert_ne!(st_c3, "Succeeded", "C3: no outbound payment from the SSP");

    println!("SDK20 - SUCCESS: the SSP pre-payment gate (C2 wrong-recipient + C3 undersized) refuses BEFORE paying, over the live SE (batch_statechains) + live RLN. No invoice settled; the SSP lost nothing.");
    Ok(())
}
