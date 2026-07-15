//! E2E (adversarial): the malicious RECEIVER who PAYS attention to the clock — audit [2]/[5]. The
//! payer pays the HODL invoice (HTLC held), but the receiver deliberately DELAYS their claim past
//! the SE latch window, trying to end up with the coin while the payer's HTLC times out and refunds.
//!
//! With the coordinated two-clock fix this must FAIL for the attacker:
//!  - the receiver's claim gate (validate_batch, minus a grace) and the SSP's preimage retrieval
//!    (get_preimage) are both bound to the SAME SE latch expiry, which is set SHORTER than the HODL
//!    HTLC. Once the latch's receiver-window closes, the receiver can NO LONGER claim the coin
//!    (validate_batch → ExpiredBatchTimeError), so the coin stays with the SSP; the SSP then cancels
//!    the HTLC and the payer is refunded. Nobody loses, and the attacker gets nothing.
//!
//! REQUIRES the SE to run with a SHORT latch so the window elapses fast, e.g.:
//!   RECEIVE_LATCH_TIMEOUT=40 RECEIVE_LATCH_GRACE=10   (receiver window ~30 s, SSP window ~40 s)
//!
//! Run: SDK_E2E=25 ML_NETWORK=regtest RLN_REGTEST=.../regtest.sh cargo run

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
    // LN swaps require a V1 coin until adaptor-sig LN lands (V2-MIGRATION-PLAN); pin regardless of the V2 default.
    std::env::set_var("UTEXO_PROTOCOL_DEFAULT", "1");
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    let cc = mercuryrustlib::client_config::load().await;

    let (payer_node, ssp_node) = rln::setup_ln_pair("/tmp/rln-sdk25").await?;
    println!("SDK25 - LN pair up");

    let (ssp_wallet, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk25_ssp"), None).await?;
    let t = prepaid_token(&cc).await?;
    ssp_wallet.add_prepaid_token(&t).await;
    let addr = ssp_wallet.get_deposit_address(100_000).await?;
    bitcoin_core::sendtoaddress(100_000, &addr)?;
    let core = bitcoin_core::getnewaddress()?;
    bitcoin_core::generatetoaddress(3, &core)?;
    let mut waited = 0;
    while ssp_wallet.get_balance().await?.available_sats != 100_000 {
        ssp_wallet.claim().await?;
        waited += 1;
        if waited > 60 {
            return Err(anyhow!("SSP deposit did not confirm"));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    for _ in 0..2 {
        let t = prepaid_token(&cc).await?;
        ssp_wallet.add_prepaid_token(&t).await;
    }
    let ssp = SspService::new(ssp_wallet, RlnClient::new(&ssp_node.api), 0);
    println!("SDK25 - SSP funded");

    // Receiver asks for an invoice; SSP latches the coin + issues the HODL invoice.
    let (alice, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk25_alice"), None).await?;
    let swap = alice.create_lightning_invoice(&ssp, 20_000).await?;
    let sid = swap.statechain_id.clone().ok_or_else(|| anyhow!("local swap should carry a statechain_id"))?;
    println!("SDK25 - SSP latched the coin + issued HODL invoice");

    // Payer PAYS — the HTLC parks HELD (Claimable) at the SSP node.
    let payer = RlnClient::new(&payer_node.api);
    let pay_hash = payer.send_payment(&swap.invoice).await?;
    let mut held = false;
    for _ in 0..45 {
        if ssp.rln.invoice_status(&swap.invoice).await? == "Claimable" {
            held = true;
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    assert!(held, "payer's HTLC must be HELD (Claimable)");
    println!("SDK25 - payer's HTLC is HELD; receiver now DELAYS past the latch window...");

    // The malicious receiver deliberately does NOT claim. Wait out the SE latch receiver-window.
    // (The SE must be running with a SHORT RECEIVE_LATCH_TIMEOUT for this to elapse quickly.)
    tokio::time::sleep(Duration::from_secs(45)).await;

    // 1) The receiver can NO LONGER claim the coin — validate_batch refuses once the latch window
    //    (minus grace) has passed. alice's claim yields nothing.
    for _ in 0..5 {
        let _ = alice.claim().await;
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    assert_eq!(
        alice.get_balance().await?.available_sats,
        0,
        "BREACH: the delayed receiver obtained the coin after the latch window"
    );
    println!("SDK25 - receiver cannot claim after the latch window \u{2713}");

    // 2) The SSP still holds the coin: the SE refuses the preimage after latch expiry, so the SSP
    //    cannot (and need not) claim the HTLC.
    let got = mercuryrustlib::lightning_latch::retrieve_pre_image(
        ssp.wallet.client_config(),
        ssp.wallet.wallet_name(),
        &sid,
        &swap.batch_id,
    )
    .await;
    assert!(got.is_err(), "SE must withhold the preimage after the latch expired");
    println!("SDK25 - SE withholds the preimage after expiry (SSP retains its coin) \u{2713}");

    // 3) The payer is refunded — the SSP cancels the now-abandoned HODL HTLC.
    let _ = ssp.rln.cancel_hodl(&swap.payment_hash).await;
    let mut refunded = false;
    for _ in 0..40 {
        let status = payer.payment(&pay_hash).await.map(|(s, _)| s).unwrap_or_else(|_| "Unknown".into());
        if status == "Failed" {
            refunded = true;
            break;
        }
        if status == "Succeeded" {
            return Err(anyhow!("BREACH: payer's payment Succeeded on an abandoned swap"));
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    assert!(refunded, "the payer must be refunded after the swap is abandoned");
    println!("SDK25 - payer refunded \u{2713}");

    println!("SDK25 - SUCCESS: a receiver who delayed their claim past the coordinated SE latch window got NOTHING — validate_batch refused the late claim, the SSP kept its coin, and the payer was refunded. The two-clock [2]/[5] atomicity hole is closed.");
    Ok(())
}
