//! E2E: Lightning → Mercury. alice — who has NO on-chain presence at all — receives a real
//! Lightning payment straight into a statechain coin via the SSP.
//!
//! Atomicity under test: the SSP latch-transfers the coin to alice bound to an SE-held preimage
//! and issues a HODL invoice on that hash. The SE will not reveal the preimage until the latch
//! is confirmed (coin released), so the SSP can only take the Lightning money after alice's coin
//! is claimable. No payment → the latch expires and the SSP keeps its coin.
//!
//! Run: SDK_E2E=6 ML_NETWORK=regtest cargo run  (regtest + lockbox stack + RLN binary built)

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

    // Lightning: node A = the payer, node B = the SSP's node (receives the HTLC).
    let (payer_node, ssp_node) = rln::setup_ln_pair("/tmp/rln-sdk06").await?;
    println!("SDK06 - LN pair up");

    // SSP: funded statechain wallet (it fronts the coin) + its RLN node.
    let (ssp_wallet, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk6_ssp"), None).await?;
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
    // The receive swap will mint the exact 20k coin off-chain (2 split slots).
    for _ in 0..2 {
        let t = prepaid_token(&cc).await?;
        ssp_wallet.add_prepaid_token(&t).await;
    }
    let ssp = SspService::new(ssp_wallet, RlnClient::new(&ssp_node.api), 0);
    println!("SDK06 - SSP funded: 100k sats");

    // alice: brand-new wallet. No deposit, no on-chain anything.
    let (alice, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk6_alice"), None).await?;
    let mut alice_events = alice.subscribe();
    let alice_bg = alice.start_background();

    // alice asks for a 20k-sat Lightning invoice.
    let swap = alice.create_lightning_invoice(&ssp, 20_000).await?;
    println!("SDK06 - invoice for alice (20k sats), hash {}", swap.payment_hash);

    // The payer pays it (hodl: HTLC parks as Pending until the SSP claims with the preimage).
    let payer = RlnClient::new(&payer_node.api);
    let inv = swap.invoice.clone();
    let pay_task = tokio::spawn(async move { payer.send_payment(&inv).await });

    // SSP drives settlement: HTLC pending -> confirm latch (coin released) -> preimage -> claim.
    ssp.settle_receive(&swap).await?;
    println!("SDK06 - swap settled: invoice Succeeded on the SSP node");

    let _ = pay_task.await;

    // alice's watcher claims the coin.
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
    .map_err(|_| anyhow!("alice did not claim the coin"))?;
    alice_bg.abort();
    println!("SDK06 - alice claimed her coin ({} coin)", claimed.len());

    assert_eq!(alice.get_balance().await?.available_sats, 20_000, "alice owns 20k sats");
    let (payer_status, _) = RlnClient::new(&payer_node.api).payment(&swap.payment_hash).await?;
    assert_eq!(payer_status, "Succeeded", "payer's outbound payment settled");
    let ssp_balance = ssp.wallet.get_balance().await?;
    assert!(ssp_balance.available_sats < 80_000, "SSP gave up the 20k coin");

    println!("SDK06 - SUCCESS: Lightning -> Mercury. A wallet with ZERO on-chain presence received a real LN payment as a statechain coin; the SE's locked-preimage gating made coin release a precondition of the SSP claiming the HTLC.");
    Ok(())
}
