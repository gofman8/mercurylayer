//! E2E (SDK_E2E=67) — **NON-EXACT Lightning RECEIVE via a latched in-ladder split** (LIGHTNING.md).
//!
//! The RECEIVE symmetry of sdk65, and the last piece needed before the pre-TES-R LN lane could be
//! deleted (it handled any amount by splitting the SSP's un-laddered coin). The SSP holds only a large
//! laddered coin, so `create_receive` cannot mint an exact coin — it falls back to an IN-LADDER split, conveying
//! a piece worth the invoiced amount to the user under an SE-minted preimage. Alice (zero on-chain
//! presence) pays the HODL invoice; `settle_receive` (unchanged — it operates on the piece sid) releases
//! the piece and claims the HTLC; Alice's watcher adopts the piece.
//!
//! Run: SDK_E2E=67 ML_NETWORK=regtest cargo run  (regtest + lockbox stack + RLN binary built)

use anyhow::{anyhow, Result};
use mercury_utexo_sdk::ssp::{RlnClient, SspService};
use mercury_utexo_sdk::{SdkConfig, UtexoWallet, WalletEvent};
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

    let (payer_node, ssp_node) = rln::setup_ln_pair("/tmp/rln-sdk67").await?;
    println!("SDK67 - LN pair up");

    // SSP: a single LARGE laddered coin (100k). ensure_exact_coin(20k) can't split it exactly ⟹
    // create_receive falls back to the in-ladder split.
    let (ssp_wallet, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk67_ssp"), None).await?;
    let t = prepaid_token(&cc).await?;
    ssp_wallet.add_prepaid_token(&t).await;
    let ssp_deposit: u64 = 100_000;
    let addr = ssp_wallet.get_deposit_address(ssp_deposit).await?;
    bitcoin_core::sendtoaddress(ssp_deposit as u32, &addr)?;
    let core = bitcoin_core::getnewaddress()?;
    bitcoin_core::generatetoaddress(3, &core)?;
    let mut waited = 0;
    while ssp_wallet.get_balance().await?.available_sats != ssp_deposit {
        ssp_wallet.claim().await?;
        waited += 1;
        if waited > 60 {
            return Err(anyhow!("SSP deposit did not confirm"));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let ssp = SspService::new(ssp_wallet, RlnClient::new(&ssp_node.api), 0);
    println!("SDK67 - SSP funded: one {ssp_deposit}-sat laddered coin");

    // alice: brand-new wallet, no on-chain presence. Receives a NON-EXACT amount vs the SSP's coin.
    let amount: u64 = 20_000;
    let (alice, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk67_alice"), None).await?;
    let mut alice_events = alice.subscribe();
    let alice_bg = alice.start_background();

    let swap = alice.create_lightning_invoice(&ssp, amount).await?;
    println!("SDK67 - invoice for alice ({amount} sats), hash {}", swap.payment_hash);

    let payer = RlnClient::new(&payer_node.api);
    let inv = swap.invoice.clone();
    let pay_task = tokio::spawn(async move { payer.send_payment(&inv).await });

    ssp.settle_receive(&swap).await?;
    println!("SDK67 - swap settled: invoice Succeeded on the SSP node");
    let _ = pay_task.await;

    let claimed = tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            match alice_events.recv().await {
                Ok(WalletEvent::TransferClaimed { statechain_ids }) => break statechain_ids,
                Ok(_) => continue,
                Err(e) => panic!("event stream closed: {e}"),
            }
        }
    })
    .await
    .map_err(|_| anyhow!("alice did not claim the piece"))?;
    alice_bg.abort();
    println!("SDK67 - alice claimed her piece ({} coin)", claimed.len());

    // alice received a piece worth at least the invoiced amount; it carries a child exit bundle.
    let alice_bal = alice.get_balance().await?;
    assert!(
        alice_bal.available_sats >= amount,
        "alice must receive a piece worth >= {amount} (got {})",
        alice_bal.available_sats
    );
    let alice_sid = claimed.first().cloned().ok_or(anyhow!("no claimed sid"))?;
    assert!(
        mercuryrustlib::tesr::load_child(&cc, "sdk67_alice", &alice_sid).await?.is_some(),
        "alice's received coin must be an adopted split child"
    );
    let (payer_status, _) = RlnClient::new(&payer_node.api).payment(&swap.payment_hash).await?;
    assert_eq!(payer_status, "Succeeded", "payer's outbound payment settled");
    println!("SDK67 - alice's piece: {} sat (child bundle)", alice_bal.available_sats);

    println!("SDK67 - ✓ SUCCESS: NON-EXACT Lightning RECEIVE into a TES-R coin. The SSP had only a laddered coin, so create_receive in-ladder-split off a piece worth the invoice and conveyed it under an SE-minted preimage; alice paid the HODL and adopted the piece. Both LN directions now support arbitrary amounts on laddered coins.");
    Ok(())
}
