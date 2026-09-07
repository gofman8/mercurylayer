//! E2E (SDK_E2E=64) — **Lightning → Mercury into a V2 (TES-R) coin** (HODL-latch pivot).
//!
//! The RECEIVE half on V2: alice (zero on-chain presence) receives a real Lightning payment as a
//! **V2 (TES-R laddered) statechain coin**. The SSP fronts its OWN coin under a HODL invoice; the SE
//! reveals the preimage only once alice's coin is claimable (`settle_receive`'s coordinated clock),
//! so the SSP can take the LN money only after releasing the coin. No operator trust is needed for
//! the RECEIVE direction: the SSP owns the coin throughout its risk window.
//!
//! To isolate the guard-lift from the in-ladder-split machinery, the SSP fronts an EXACT coin
//! (`create_receive` → `ensure_exact_coin` returns it whole, no split): the SSP's coin is laddered
//! and the latched transfer exercises the (now-lifted) sdk53 path.
//!
//! Re-derived for the ladder rule, which this test now pins on both ends of the hop:
//!   * the SSP's fronted deposit is laddered AT SIGHT with NO flat backup: a `tesr-` row exists,
//!     ZERO flat backup rows (`create_tx1` is gone), `locktime == None`, and the enclave count is
//!     exactly the 3 tiers (a 4 would be a `tx1` co-signed at deposit);
//!   * alice's RECEIVED coin is that same laddered coin one hop on: a `tesr-` row in alice's wallet,
//!     ZERO flat rows (the hop conveyed `backup_transactions: []` — the receiver refuses any flat
//!     backup by name, so the row alice persists is empty), `locktime == None` (the receiver books
//!     no calendar), and a census that balances with the flat term 0 and NOT with 1.
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

/// The statechain id of `wallet`'s one CONFIRMED (index-0) coin.
async fn confirmed_sid(cc: &mercuryrustlib::client_config::ClientConfig, wallet: &str) -> Result<String> {
    mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, wallet)
        .await?
        .coins
        .iter()
        .find(|c| c.status == mercuryrustlib::CoinStatus::CONFIRMED && c.duplicate_index == 0)
        .and_then(|c| c.statechain_id.clone())
        .ok_or(anyhow!("{wallet} has no confirmed coin"))
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

    // THE SHAPE of the coin the SSP will front, pinned before any Lightning leg: laddered, ZERO flat
    // backup rows, no calendar, and an enclave count of exactly the 3 tiers (a 4 is a flat tx1).
    let ssp_sid = confirmed_sid(&cc, "sdk64_ssp").await?;
    let ssp_bundle = assert_ladder_shape(&cc, "sdk64_ssp", &ssp_sid, "the SSP's deposit").await?;
    let sigs_at_deposit = num_sigs(&cc, &ssp_sid).await?;
    assert_eq!(
        sigs_at_deposit, 3,
        "the SSP's laddered deposit must have consumed exactly 3 co-signs (T, X, S); a 4 is a flat tx1"
    );
    mercuryrustlib::tesr::verify_bundle(&ssp_bundle, sigs_at_deposit, 0)
        .map_err(|e| anyhow!("the SSP's census must balance with a flat term of 0: {e}"))?;
    println!("SDK64 - the SSP's coin {ssp_sid}: `tesr-` row, 0 flat backup rows, locktime None, num_sigs 3 == 0 flat + 3 tiers");

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
    // The received coin is V2 (carries a TES-R ladder) — the point of RECEIVE-on-V2 — and it is the
    // SSP's coin one hop on, conveyed with NO flat backup: a `tesr-` row in alice's wallet, ZERO flat
    // rows, no calendar, and a census that balances with the flat term 0 and NOT with 1.
    let alice_sid = claimed.first().cloned().ok_or(anyhow!("no claimed sid"))?;
    assert_eq!(alice_sid, ssp_sid, "alice must have received the very coin the SSP fronted");
    let alice_bundle = assert_ladder_shape(&cc, "sdk64_alice", &alice_sid, "alice's received coin").await?;
    let sigs_after_hop = num_sigs(&cc, &alice_sid).await?;
    mercuryrustlib::tesr::verify_bundle(&alice_bundle, sigs_after_hop, 0).map_err(|e| {
        anyhow!("alice's census must balance with a flat term of 0 (num_sigs {sigs_after_hop}): {e}")
    })?;
    assert!(
        mercuryrustlib::tesr::verify_bundle(&alice_bundle, sigs_after_hop, 1).is_err(),
        "no phantom flat backup may be counted: the census must NOT balance with a flat term of 1"
    );
    println!(
        "SDK64 - alice's coin {alice_sid}: `tesr-` row, 0 flat backup rows, locktime None, census \
         num_sigs {sigs_after_hop} == 0 flat + {} tiers + {} superseded",
        alice_bundle.exit_tiers().len(),
        (sigs_after_hop as usize).saturating_sub(alice_bundle.exit_tiers().len())
    );

    let (payer_status, _) = RlnClient::new(&payer_node.api).payment(&swap.payment_hash).await?;
    assert_eq!(payer_status, "Succeeded", "payer's outbound payment settled");
    assert_eq!(ssp.wallet.get_balance().await?.available_sats, 0, "SSP gave up its coin");

    println!("SDK64 - ✓ SUCCESS: Lightning -> Mercury into a V2 coin. A zero-on-chain wallet received a real LN payment as a TES-R laddered statechain coin carrying no flat backup and no calendar; the SSP fronted a V2 coin (sdk53 guard lifted) and the SE's coordinated-clock preimage gating made coin release a precondition of the SSP claiming the HTLC. RECEIVE-on-V2 works.");
    Ok(())
}
