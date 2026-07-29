//! E2E (SDK_E2E=64) — **Lightning → Mercury into a V2 (TES-R) coin** (HODL-latch pivot).
//!
//! The RECEIVE half on V2: alice (zero on-chain presence) receives a real Lightning payment as a
//! **V2 (TES-R laddered) statechain coin**. The SSP fronts its OWN coin under a HODL invoice; the SE
//! reveals the preimage only once alice's coin is claimable (`settle_receive`'s coordinated clock),
//! so the SSP can take the LN money only after releasing the coin. No operator trust is needed for
//! the RECEIVE direction: the SSP owns the coin throughout its risk window.
//!
//! To isolate the guard-lift from the in-ladder-split machinery, the SSP fronts an EXACT coin
//! (`create_receive` → `ensure_exact_coin` returns it whole, no split). NOTE: no
//! the SSP's coin is laddered and the
//! latched transfer exercises the (now-lifted) sdk53 path.
//!
//! Run: SDK_E2E=64 ML_NETWORK=regtest cargo run  (regtest + lockbox stack + RLN binary built)

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
    let (payer_node, ssp_node) = rln::setup_ln_pair("/tmp/rln-sdk64").await?;
    println!("SDK64 - LN pair up");

    // SSP: fronts an EXACT-amount V2 coin (no split → isolate the guard-lift from in-ladder split).
    let amount: u64 = 20_000;
    let (ssp_wallet, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk64_ssp"), None).await?;
    let t = prepaid_token(&cc).await?;
    ssp_wallet.add_prepaid_token(&t).await;
    let addr = ssp_wallet.get_deposit_address(amount).await?;
    bitcoin_core::sendtoaddress(amount as u32, &addr)?;
    let core = bitcoin_core::getnewaddress()?;
    bitcoin_core::generatetoaddress(3, &core)?;
    let mut waited = 0;
    while ssp_wallet.get_balance().await?.available_sats != amount {
        ssp_wallet.claim().await?;
        waited += 1;
        if waited > 60 {
            return Err(anyhow!("SSP deposit did not confirm"));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let ssp = SspService::new(ssp_wallet, RlnClient::new(&ssp_node.api), 0);
    println!("SDK64 - SSP funded: an exact {amount}-sat V2 coin");

    // alice: brand-new wallet. No deposit, no on-chain anything.
    let (alice, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk64_alice"), None).await?;
    let mut alice_events = alice.subscribe();
    let alice_bg = alice.start_background();

    // alice asks for a Lightning invoice for the exact amount.
    let swap = alice.create_lightning_invoice(&ssp, amount).await?;
    println!("SDK64 - invoice for alice ({amount} sats), hash {}", swap.payment_hash);

    // The payer pays it (HODL: HTLC parks until the SSP claims with the preimage).
    let payer = RlnClient::new(&payer_node.api);
    let inv = swap.invoice.clone();
    let pay_task = tokio::spawn(async move { payer.send_payment(&inv).await });

    // SSP drives settlement: HTLC held -> confirm latch (coin released) -> preimage -> claim HTLC.
    ssp.settle_receive(&swap).await?;
    println!("SDK64 - swap settled: invoice Succeeded on the SSP node");
    let _ = pay_task.await;

    // alice's watcher claims the V2 coin (its claim re-ran verify_bundle over the conveyed ladder).
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
    println!("SDK64 - alice claimed her coin ({} coin)", claimed.len());

    assert_eq!(alice.get_balance().await?.available_sats, amount, "alice owns the {amount}-sat coin");
    // The received coin is V2 (carries a TES-R ladder) — the point of RECEIVE-on-V2.
    let alice_sid = claimed.first().cloned().ok_or(anyhow!("no claimed sid"))?;
    let is_v2 = mercuryrustlib::tesr::load(&cc, "sdk64_alice", &alice_sid).await?.is_some();
    assert!(is_v2, "alice's received coin must carry a TES-R ladder (V2)");

    let (payer_status, _) = RlnClient::new(&payer_node.api).payment(&swap.payment_hash).await?;
    assert_eq!(payer_status, "Succeeded", "payer's outbound payment settled");
    assert_eq!(ssp.wallet.get_balance().await?.available_sats, 0, "SSP gave up its coin");

    println!("SDK64 - ✓ SUCCESS: Lightning -> Mercury into a V2 coin. A zero-on-chain wallet received a real LN payment as a TES-R laddered statechain coin; the SSP fronted a V2 coin (sdk53 guard lifted) and the SE's coordinated-clock preimage gating made coin release a precondition of the SSP claiming the HTLC. RECEIVE-on-V2 works.");
    Ok(())
}
