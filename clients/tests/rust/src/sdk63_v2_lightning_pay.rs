//! E2E (SDK_E2E=63) — **V2 (TES-R) coin → Lightning PAY through the SSP** (HODL-latch pivot).
//!
//! The user-facing blocker the pivot removes: with V2 as the default, a user's coins are all
//! laddered, and the old sdk53 guard refused a Lightning-latched transfer of a V2 coin — so a V2
//! wallet could not pay over LN at all. This proves it now works: alice pays a real BOLT11 from a
//! **V2** statechain coin, and the SSP's **pre-pay census** (verify_bundle over the conveyed ladder,
//! num_sigs == v1_backups + tiers read from the enclave sig-count) runs before send_payment.
//!
//! To isolate the census + guard-lift from the in-ladder-split machinery, alice deposits the EXACT
//! invoice amount so `ensure_exact_coin` uses the whole V2 coin (no split). NOTE: no
//! `UTEXO_PROTOCOL_DEFAULT` pin — this runs on the real V2 default.
//!
//! Run: SDK_E2E=63 ML_NETWORK=regtest cargo run  (regtest + lockbox stack + RLN binary built)

use anyhow::{anyhow, Result};
use mercury_utexo_sdk::ssp::{RlnClient, SspService};
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};
use sha2::{Digest, Sha256};
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

    // Lightning: node A = merchant, node B = the SSP's node.
    let (merchant_node, ssp_node) = rln::setup_ln_pair("/tmp/rln-sdk63").await?;
    println!("SDK63 - LN pair up (channel usable)");

    // SSP: statechain wallet + its RLN node, zero fee (so the exact coin == invoice amount).
    let (ssp_wallet, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk63_ssp"), None).await?;
    let ssp = SspService::new(ssp_wallet, RlnClient::new(&ssp_node.api), 0);

    // alice: deposit EXACTLY the invoice amount as a single V2 coin (no split needed).
    let amount: u64 = 25_000;
    let (alice, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk63_alice"), None).await?;
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
    println!("SDK63 - alice funded: {amount} sats on a V2 (TES-R laddered) coin");

    // Merchant issues an invoice for the exact amount on its own node.
    let invoice = merchant_node.ln_invoice(amount * 1000, None, 3600).await?;
    println!("SDK63 - merchant invoice created ({amount} sats)");

    // alice pays it from her V2 statechain coin through the SSP. The SSP's execute_pay runs the
    // pre-pay ladder census (verify_bundle over the conveyed V2 ladder) BEFORE send_payment.
    let preimage = alice.pay_lightning_invoice(&ssp, &invoice).await?;
    println!("SDK63 - invoice paid from a V2 coin; alice holds the preimage as proof");

    // Proof: preimage hashes to the invoice hash; merchant sees Succeeded.
    let (_, invoice_hash) = ssp.rln.decode_invoice(&invoice).await?;
    assert_eq!(
        hex::encode(Sha256::digest(hex::decode(&preimage)?)),
        invoice_hash,
        "preimage is the invoice's"
    );
    assert_eq!(merchant_node.invoice_status(&invoice).await?, "Succeeded");

    // The SSP now owns the V2 coin (its claim re-ran verify_bundle and accepted the Model-A ladder).
    let ssp_balance = ssp.wallet.get_balance().await?;
    assert_eq!(ssp_balance.available_sats, amount, "SSP claimed the latched V2 coin");
    println!(
        "SDK63 - settled: SSP owns {amount} (V2 coin, census passed pre-pay AND at claim); merchant paid on LN"
    );

    println!("SDK63 - ✓ SUCCESS: a V2 (TES-R) coin paid a real BOLT11 through the SSP. The sdk53 guard is lifted; the SSP's pre-pay verify_bundle census guards rob-SSP; the LN preimage unlocked the coin and proved payment. V2 wallets can now pay over Lightning.");
    Ok(())
}
