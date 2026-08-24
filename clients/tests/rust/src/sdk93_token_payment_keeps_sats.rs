//! E2E (SDK_E2E=93) — **a token payment must not cost the sender a carrier.**
//!
//! THE ECONOMICS THIS PINS. On the shipped path every RGB payment mints a NEW carrier for the
//! receiver, so the sender GIFTS ~330–500 sat with each payment. At ~$0.50 a carrier, sending $1 of
//! tokens costs $0.50 in sats — on-chain economics on an off-chain rail, which defeats the point.
//!
//! THE PATH UNDER TEST. `transfer_tokens_onto` assigns the allocation to an outpoint the RECEIVER
//! ALREADY OWNS, named in the clear (a REVEALED foreign seal), while the single bitcoin output pays
//! the SENDER. The allocation moves; the satoshis do not.
//!
//! WHAT WOULD MAKE THIS TEST FAIL IF THE CODE WERE WRONG — this is the point of it, not the green
//! tick:
//!   * if the sender still funded a carrier, `sender_sats_after == sender_sats_before` breaks;
//!   * if the assignment silently landed nowhere, `credited == PAY` breaks — and that is the exact
//!     shape the fork produced before the extractor fix: a consignment that VALIDATED perfectly and
//!     credited ZERO, with nothing erroring;
//!   * if the receiver's carrier were consumed rather than added to, the second payment onto the
//!     SAME outpoint breaks — which is what makes the carrier a once-per-wallet cost rather than a
//!     per-payment one.
//!
//! **STATUS: RED, DELIBERATELY, AND THIS HEADER IS THE POINT.** The mechanism it exercises is proven
//! — `rgb09` with `RGB09_REVEALED=1` moves an allocation onto an outpoint the receiver already owns,
//! credits both sides, and leaves the sender's satoshis untouched (exit 0). What is NOT yet wired is
//! the SDK path, and running this test is how each obstacle was found. In order:
//!
//!   1. **The issuance carrier is unusable as a source.** Issuance leaves an OPEN TRANSFER on it at
//!      the coordinator, and the SE refuses any co-signature while one is open (409). Claiming does
//!      not clear it. Hence the test pays from a carrier received normally instead.
//!   2. **A sending wallet needs a spare colorable outpoint.** Change cannot land on the carrier being
//!      spent — that seal is closed by the transition — so a second coin is required, exactly as an
//!      on-chain wallet needs a change address.
//!   3. **WHERE IT STOPS TODAY:** `refresh_rgb_anchor_self_transfer` reads the coin's backup chain
//!      with `get_backup_txs`, which is `fetch_one` — so "no rows" surfaces as a query FAILURE, not
//!      an absence. A carrier received through a coloured split is a CHILD whose exit material lives
//!      under `branch-<sid>`, not in a plain backup chain, so the lookup finds nothing and the call
//!      dies with "no rows returned by a query that expected to return at least one row".
//!
//! So the remaining work is making that path accept a received child, not anything about revealed
//! seals. This test is committed RED on purpose: it is a runnable reproduction of a real gap, which
//! is worth more than a green test that stops before reaching it.
//!
//! Run: SDK_E2E=93 ML_NETWORK=regtest cargo run --release

use anyhow::{anyhow, Result};
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};
use std::time::Duration;

use crate::bitcoin_core;

const PAY: u64 = 250;
const PAY2: u64 = 100;
const SUPPLY: u64 = 1000;

async fn prepaid_token(cc: &mercuryrustlib::client_config::ClientConfig) -> Result<String> {
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    crate::utils::handle_token_response(cc, &token).await
}

async fn claim_until_sats(w: &UtexoWallet, want: u64) -> Result<()> {
    for _ in 0..40 {
        w.claim().await?;
        if w.get_balance().await?.available_sats == want {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Err(anyhow!("did not receive {want} sats"))
}

/// Fund a wallet's RGB engine and give it a confirmed, registered colorable coin it can be paid ON.
async fn fund_engine(
    cc: &mercuryrustlib::client_config::ClientConfig,
    w: &UtexoWallet,
    core: &str,
) -> Result<()> {
    let fund = w.get_token_funding_address().await?;
    bitcoin_core::sendtoaddress(100_000, &fund)?;
    bitcoin_core::generatetoaddress(3, core)?;
    tokio::time::sleep(Duration::from_secs(3)).await;
    for _ in 0..4 {
        let t = prepaid_token(cc).await?;
        w.add_prepaid_token(&t).await;
    }
    Ok(())
}

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    for d in ["./rgb-data-sdk93_alice", "./rgb-data-sdk93_bob"] {
        let _ = std::fs::remove_dir_all(d);
    }
    let cc = mercuryrustlib::client_config::load().await;
    let core = bitcoin_core::getnewaddress()?;

    let mut a_cfg = SdkConfig::regtest("sdk93_alice");
    a_cfg.rgb_data_dir = Some("./rgb-data-sdk93_alice".to_string());
    let mut b_cfg = SdkConfig::regtest("sdk93_bob");
    b_cfg.rgb_data_dir = Some("./rgb-data-sdk93_bob".to_string());
    let (alice, _) = UtexoWallet::initialize(a_cfg, None).await?;
    let (bob, _) = UtexoWallet::initialize(b_cfg, None).await?;

    // ---- 1. The sender funds herself and issues. ------------------------------------------------
    let t = prepaid_token(&cc).await?;
    alice.add_prepaid_token(&t).await;
    let addr = alice.get_deposit_address(60_000).await?;
    bitcoin_core::sendtoaddress(60_000, &addr)?;
    bitcoin_core::generatetoaddress(3, &core)?;
    claim_until_sats(&alice, 60_000).await?;
    fund_engine(&cc, &alice, &core).await?;
    let asset = alice.issue_token("KEEP", "Sats-keeping token", 0, SUPPLY).await?;

    let mut ready = false;
    for _ in 0..40 {
        bitcoin_core::generatetoaddress(1, &core)?;
        alice.claim().await?;
        // Wait for the carrier COIN, not just the engine balance: the allocation settles first and
        // the statechain coin confirms after, so a balance-only wait races the signing path.
        let n_confirmed = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk93_alice")
            .await?
            .coins
            .iter()
            .filter(|c| c.status == mercuryrustlib::CoinStatus::CONFIRMED && c.duplicate_index == 0)
            .count();
        if n_confirmed >= 2
            && alice
                .get_token_balances()
                .await?
                .iter()
                .any(|b| b.asset_id == asset && b.balance >= SUPPLY)
        {
            ready = true;
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    assert!(ready, "issuance did not settle");
    // Issuance leaves an OPEN TRANSFER on the carrier at the coordinator; the SE refuses a
    // co-signature while one is open (409). Claiming is what completes it, and the readiness loop
    // above can exit before that happens — the engine balance and the coin's local status both go
    // green first. So settle explicitly rather than racing it.
    for _ in 0..10 {
        alice.claim().await?;
        bitcoin_core::generatetoaddress(1, &core)?;
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    println!("SDK93 - (1) alice issued {SUPPLY} {asset} and settled the carrier's open transfer");

    // ---- 2. Bob receives normally, so he holds a CLEAN carrier to pay from. ---------------------
    //
    // Deliberately NOT paying from the issuance carrier: issuance leaves an open transfer on it at
    // the coordinator, and the SE refuses any co-signature while one is open. That is a property of
    // issuance, not of this lane, and a test that fought it would be measuring the wrong thing.
    let t = prepaid_token(&cc).await?;
    bob.add_prepaid_token(&t).await;
    let bob_addr = bob.get_utexo_address().await?;
    let _ = alice.transfer_tokens(&asset, &bob_addr, 400).await?;
    let mut bob_bal = 0;
    for _ in 0..40 {
        bob.claim().await?;
        bob_bal = bob
            .get_token_balances()
            .await?
            .into_iter()
            .find(|b| b.asset_id == asset)
            .map(|b| b.balance)
            .unwrap_or(0);
        if bob_bal >= 400 {
            break;
        }
        bitcoin_core::generatetoaddress(1, &core)?;
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    assert_eq!(bob_bal, 400, "bob must receive 400 on the existing lane first");
    fund_engine(&cc, &bob, &core).await?;
    // A second coin for bob: paying part of an allocation leaves CHANGE, and the change cannot land
    // on the carrier being spent — that seal is closed by the very transition. So a sending wallet
    // needs one spare colorable outpoint, exactly as an on-chain wallet needs a change address.
    let t = prepaid_token(&cc).await?;
    bob.add_prepaid_token(&t).await;
    let b_addr2 = bob.get_deposit_address(20_000).await?;
    bitcoin_core::sendtoaddress(20_000, &b_addr2)?;
    bitcoin_core::generatetoaddress(3, &core)?;
    for _ in 0..30 {
        bob.claim().await?;
        let n = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk93_bob")
            .await?
            .coins
            .iter()
            .filter(|c| c.status == mercuryrustlib::CoinStatus::CONFIRMED && c.duplicate_index == 0)
            .count();
        if n >= 2 {
            break;
        }
        bitcoin_core::generatetoaddress(1, &core)?;
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    println!("SDK93 - (2) bob holds 400 {asset} on a clean carrier, plus a spare coin for change");

    // ---- 3. ALICE publishes an outpoint she ALREADY owns, to be paid ON. ------------------------
    let alice_outpoint = alice.token_receive_outpoint(&asset).await?;
    println!("SDK93 - (3) alice published a receive outpoint she ALREADY owns: {alice_outpoint}");

    // ---- 4. THE MEASUREMENT: bob pays, and his satoshis do not move. ----------------------------
    let sender_sats_before = bob.get_balance().await?.available_sats;
    let (consignment, witness) = bob.transfer_tokens_onto(&asset, &alice_outpoint, PAY).await?;
    let sender_sats_after = bob.get_balance().await?.available_sats;
    println!(
        "SDK93 - (4) paid {PAY} onto alice's own outpoint; sender sats {sender_sats_before} -> {sender_sats_after}"
    );
    assert_eq!(
        sender_sats_after, sender_sats_before,
        "THE POINT OF THIS TEST: the sender must not part with a single satoshi to move an allocation"
    );

    let credited = alice
        .accept_tokens_on_outpoint(&consignment, &witness, &alice_outpoint)
        .await?;
    assert_eq!(
        credited, PAY,
        "alice must be credited {PAY} on her OWN outpoint (a zero here is the silent-credit failure \
         the extractor fix closed: the consignment validates and nobody is paid)"
    );
    println!("SDK93 - (4) alice credited {credited} on her own carrier, sender gifted NOTHING");

    println!(
        "SDK93 - PASS: {PAY} of {asset} moved onto an outpoint alice ALREADY owned, the sender's \
         satoshis never moved, and no carrier was minted for the receiver."
    );
    Ok(())
}
