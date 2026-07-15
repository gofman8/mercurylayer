//! E2E: Mercury → Lightning. alice pays a real BOLT11 invoice from her statechain balance
//! through the SSP, over a real rgb-lightning-node channel.
//!
//! Atomicity under test: the coin is latch-transferred bound to the INVOICE's payment hash (SE
//! latch v2); the SSP can only claim it with the LN preimage — which exists iff the invoice was
//! paid — and that same preimage is alice's proof of payment.
//!
//! Run: SDK_E2E=5 ML_NETWORK=regtest cargo run  (regtest + lockbox stack + RLN binary built)

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
    // LN swaps require a V1 coin until adaptor-sig LN lands (V2-MIGRATION-PLAN); pin regardless of the V2 default.
    std::env::set_var("UTEXO_PROTOCOL_DEFAULT", "1");
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    let cc = mercuryrustlib::client_config::load().await;

    // Lightning: node A = merchant, node B = the SSP's node (B holds pushed outbound liquidity).
    let (merchant_node, ssp_node) = rln::setup_ln_pair("/tmp/rln-sdk05").await?;
    println!("SDK05 - LN pair up (channel usable)");

    // SSP: statechain wallet + its RLN node. No coins needed for the pay direction.
    let (ssp_wallet, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk5_ssp"), None).await?;
    let ssp = SspService::new(ssp_wallet, RlnClient::new(&ssp_node.api), 0);

    // alice: 50k statechain balance.
    let (alice, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk5_alice"), None).await?;
    let t = prepaid_token(&cc).await?;
    alice.add_prepaid_token(&t).await;
    let addr = alice.get_deposit_address(50_000).await?;
    bitcoin_core::sendtoaddress(50_000, &addr)?;
    let core = bitcoin_core::getnewaddress()?;
    bitcoin_core::generatetoaddress(3, &core)?;
    let mut waited = 0;
    while alice.get_balance().await?.available_sats != 50_000 {
        alice.claim().await?;
        waited += 1;
        if waited > 60 {
            return Err(anyhow!("deposit did not confirm"));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    println!("SDK05 - alice funded: 50k sats on a statechain coin");

    // Merchant issues a 25k-sat invoice on its own node.
    let invoice = merchant_node.ln_invoice(25_000_000, None, 3600).await?;
    println!("SDK05 - merchant invoice created (25k sats)");

    // alice pays it from her statechain balance (exact coin minted off-chain automatically;
    // split slots consume deposit tokens).
    for _ in 0..2 {
        let t = prepaid_token(&cc).await?;
        alice.add_prepaid_token(&t).await;
    }
    let preimage = alice.pay_lightning_invoice(&ssp, &invoice).await?;
    println!("SDK05 - invoice paid; alice holds the preimage as proof");

    // Proof checks: preimage hashes to the invoice hash; merchant sees Succeeded.
    let (_, invoice_hash) = ssp.rln.decode_invoice(&invoice).await?;
    assert_eq!(
        hex::encode(Sha256::digest(hex::decode(&preimage)?)),
        invoice_hash,
        "preimage is the invoice's"
    );
    assert_eq!(merchant_node.invoice_status(&invoice).await?, "Succeeded");

    // The SSP now owns the 25k coin; alice keeps her off-chain change.
    let ssp_balance = ssp.wallet.get_balance().await?;
    assert_eq!(ssp_balance.available_sats, 25_000, "SSP claimed the latched coin");
    let alice_balance = alice.get_balance().await?;
    assert!(alice_balance.available_sats > 20_000, "alice keeps the split change");
    println!(
        "SDK05 - settled: SSP owns 25k (coin), alice change {} sats, merchant paid on LN",
        alice_balance.available_sats
    );

    println!("SDK05 - SUCCESS: Mercury -> Lightning. Statechain coin latch-bound to the invoice hash; SSP paid the BOLT11 over a real channel; the LN preimage unlocked the coin at the SE and proved payment to alice. Atomic both ways.");
    Ok(())
}
