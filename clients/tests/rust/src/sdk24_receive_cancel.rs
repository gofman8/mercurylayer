//! E2E (abort path): Lightning → Mercury RECEIVE where the payer PAYS but the SSP ABORTS. This
//! exercises the fork's HODL-invoice **cancel** leg (`/cancelhodlinvoice`) — the third of the three
//! HODL primitives (create-hold / claim / cancel):
//!
//!  1. The SSP issues a HODL invoice (external payment hash → `InvoiceType::Hodl`) and latches its
//!     coin to the SE preimage.
//!  2. The payer PAYS: the HTLC parks **HELD** at the SSP node — status `Claimable` (the corrected
//!     "HTLC is held" signal; a HODL HTLC is deliberately NOT auto-settled).
//!  3. The SSP decides not to proceed and calls `cancel_receive` → `/cancelhodlinvoice` fails the
//!     held HTLC back to the payer (immediate refund) and closes the invoice.
//!
//! Safety invariant proven: because the SSP cancels **before** confirming the latch, it never
//! releases the coin — the SE still withholds the preimage, the receiver cannot claim, the payer is
//! refunded, and the SSP keeps its coin (reclaimable). An aborted receive loses nobody money.
//!
//! Run: SDK_E2E=24 ML_NETWORK=regtest cargo run  (regtest + lockbox stack + RLN binary built)

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

    // node A = payer, node B = the SSP's node (holds the HTLC).
    let (payer_node, ssp_node) = rln::setup_ln_pair("/tmp/rln-sdk24").await?;
    println!("SDK24 - LN pair up");

    // SSP: funded statechain wallet (fronts the coin) + its RLN node.
    let (ssp_wallet, _) = SparkWallet::initialize(SdkConfig::regtest("sdk24_ssp"), None).await?;
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
    // The receive swap mints the exact 20k coin off-chain (2 split slots).
    for _ in 0..2 {
        let t = prepaid_token(&cc).await?;
        ssp_wallet.add_prepaid_token(&t).await;
    }
    let ssp = SspService::new(ssp_wallet, RlnClient::new(&ssp_node.api), 0);
    println!("SDK24 - SSP funded: {} sats", ssp.wallet.get_balance().await?.available_sats);

    // Receiver asks for an invoice; the SSP latches the coin + issues the HODL invoice.
    let (alice, _) = SparkWallet::initialize(SdkConfig::regtest("sdk24_alice"), None).await?;
    let swap = alice.create_lightning_invoice(&ssp, 20_000).await?;
    println!(
        "SDK24 - SSP issued HODL invoice + latched the coin (hash {})",
        swap.payment_hash
    );

    // The payer PAYS — the HTLC parks HELD (Claimable) at the SSP node (HODL: not auto-settled).
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
    assert!(held, "the payer's HTLC must be HELD (Claimable) by the HODL invoice");
    println!("SDK24 - payer's HTLC is HELD (Claimable) by the HODL invoice \u{2713}");

    // The SSP ABORTS the swap. It has NOT confirmed the latch, so the coin is not released. This is
    // the fork's /cancelhodlinvoice leg: the held HTLC is failed back to the payer.
    ssp.cancel_receive(&swap).await?;
    println!("SDK24 - SSP called cancel_receive (/cancelhodlinvoice)");

    // 1) The invoice is now Cancelled (never Succeeded).
    let mut cancelled = false;
    for _ in 0..30 {
        match ssp.rln.invoice_status(&swap.invoice).await?.as_str() {
            "Cancelled" | "Failed" => {
                cancelled = true;
                break;
            }
            "Succeeded" => return Err(anyhow!("BREACH: a cancelled invoice reported Succeeded")),
            _ => tokio::time::sleep(Duration::from_secs(1)).await,
        }
    }
    assert!(cancelled, "the HODL invoice must be Cancelled after cancel_receive");
    println!("SDK24 - invoice Cancelled \u{2713}");

    // 2) The payer is REFUNDED — the held HTLC fails back, so the payment resolves to Failed.
    let mut refunded = false;
    for _ in 0..60 {
        let status = payer
            .payment(&pay_hash)
            .await
            .map(|(s, _)| s)
            .unwrap_or_else(|_| "Unknown".into());
        match status.as_str() {
            "Failed" => {
                refunded = true;
                break;
            }
            "Succeeded" => return Err(anyhow!("BREACH: payer's payment Succeeded on a cancelled swap")),
            _ => tokio::time::sleep(Duration::from_secs(1)).await,
        }
    }
    assert!(refunded, "the payer must be refunded (payment Failed) after the SSP cancels");
    println!("SDK24 - payer refunded (payment Failed) \u{2713}");

    // 3) The SSP never released its coin: the SE still withholds the preimage, so the receiver
    //    cannot claim, and the SSP's coin is intact/reclaimable.
    let sid = swap
        .statechain_id
        .clone()
        .ok_or_else(|| anyhow!("local swap should carry a statechain_id"))?;
    let got = mercuryrustlib::lightning_latch::retrieve_pre_image(
        ssp.wallet.client_config(),
        ssp.wallet.wallet_name(),
        &sid,
        &swap.batch_id,
    )
    .await;
    assert!(
        got.is_err(),
        "SE must NOT reveal the preimage — the SSP cancelled before releasing the coin"
    );
    for _ in 0..5 {
        let _ = alice.claim().await;
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    assert_eq!(
        alice.get_balance().await?.available_sats,
        0,
        "receiver must not obtain the coin on a cancelled swap"
    );
    println!("SDK24 - SE withholds preimage; receiver cannot claim; SSP coin intact \u{2713}");

    println!("SDK24 - SUCCESS: payer paid \u{2192} HTLC HELD by the fork's HODL invoice \u{2192} SSP aborted via /cancelhodlinvoice \u{2192} payer refunded, receiver got nothing, SSP kept its coin. The full HODL lifecycle (create-hold / claim / cancel) is now exercised end-to-end.");
    Ok(())
}
