//! E2E (onboarding): a user **enters the statechain with nothing** — no deposit, zero balance — and
//! can RECEIVE both BTC and an RGB asset, then EXIT unilaterally. This is the "ideal onboarding"
//! flow: the receiver never funds anything on-chain; the sender's off-chain split creates the
//! receiver's coin (and the RGB carrier), and the receiver holds fully pre-signed exit material.
//!
//! Run: SDK_E2E=16 ML_NETWORK=regtest cargo run

use anyhow::{anyhow, Result};
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};
use std::time::Duration;

use crate::bitcoin_core;

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

async fn token_balance(w: &UtexoWallet, asset: &str) -> Result<u64> {
    Ok(w.get_token_balances().await?.into_iter().find(|t| t.asset_id == asset).map(|t| t.balance).unwrap_or(0))
}

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    for d in ["./rgb-data-sdk16_alice", "./rgb-data-sdk16_bob"] {
        let _ = std::fs::remove_dir_all(d);
    }
    std::env::set_var("UTEXO_PROTOCOL_DEFAULT", "2"); // V2DEF-5: onboarding on V2-native wallets
    let cc = mercuryrustlib::client_config::load().await;

    let mut alice_cfg = SdkConfig::regtest("sdk16_alice");
    alice_cfg.rgb_data_dir = Some("./rgb-data-sdk16_alice".to_string());
    let mut bob_cfg = SdkConfig::regtest("sdk16_bob");
    bob_cfg.rgb_data_dir = Some("./rgb-data-sdk16_bob".to_string());
    let (alice, _) = UtexoWallet::initialize(alice_cfg, None).await?;
    // bob: a BRAND-NEW wallet — nothing but an identity/address.
    let (bob, _) = UtexoWallet::initialize(bob_cfg, None).await?;
    let bob_addr = bob.get_utexo_address().await?;

    // Enter with nothing: zero sats, zero tokens.
    let start = bob.get_balance().await?;
    assert_eq!(start.available_sats, 0, "bob starts with no BTC");
    assert!(start.tokens.is_empty(), "bob starts with no tokens");
    println!("SDK16 - bob entered the statechain with NOTHING (0 sats, 0 tokens), only a utexo address");

    // The sender (alice) funds herself and issues an RGB asset to distribute.
    let t = prepaid_token(&cc).await?;
    alice.add_prepaid_token(&t).await;
    let addr = alice.get_deposit_address(60_000).await?;
    bitcoin_core::sendtoaddress(60_000, &addr)?;
    let core = bitcoin_core::getnewaddress()?;
    bitcoin_core::generatetoaddress(3, &core)?;
    claim_until_sats(&alice, 60_000).await?;
    // Fund alice's RGB engine (issuance needs a colorable UTXO + witness fees). This is the ISSUER's
    // cost, not the receiver's — bob never funds anything.
    let rgb_fund = alice.get_token_funding_address().await?;
    bitcoin_core::sendtoaddress(100_000, &rgb_fund)?;
    bitcoin_core::generatetoaddress(3, &core)?;
    tokio::time::sleep(Duration::from_secs(3)).await;
    for _ in 0..4 { let t = prepaid_token(&cc).await?; alice.add_prepaid_token(&t).await; }
    let asset = alice.issue_token("ONB", "Onboarding token", 0, 1000).await?;
    // Wait for the colored carrier COIN to CONFIRM. V2DEF-5: under V2 the carrier's sats are excluded
    // from available_sats (carrier ⊥ ladder), and the engine token balance settles BEFORE the statechain
    // coin, so the old `available_sats >= pre+10_000` proxy no longer signals the coin confirmed. The
    // version-agnostic signal is the raw coin count: alice now has TWO confirmed non-dup coins (the 60k
    // deposit + the issuance carrier) AND the supply is bound.
    let mut ready = false;
    for _ in 0..40 {
        bitcoin_core::generatetoaddress(1, &core)?;
        alice.claim().await?;
        let n_confirmed = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk16_alice")
            .await?
            .coins
            .iter()
            .filter(|c| c.status == mercuryrustlib::CoinStatus::CONFIRMED && c.duplicate_index == 0)
            .count();
        let b = alice.get_balance().await?;
        if n_confirmed >= 2 && b.tokens.iter().any(|t| t.asset_id == asset && t.balance >= 1000) {
            ready = true;
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    assert!(ready, "issuer's token carrier coin did not confirm");
    println!("SDK16 - sender funded 60k and issued 1000 {asset}");

    // --- ONBOARD #1: receive an RGB asset with nothing (done first, while alice's coins are clean) -
    assert_eq!(token_balance(&bob, &asset).await?, 0, "bob has no tokens yet");
    let _rt = alice.transfer_tokens(&asset, &bob_addr, 250).await?;
    let mut got = 0;
    for _ in 0..40 {
        bob.claim().await?;
        got = token_balance(&bob, &asset).await?;
        if got == 250 { break; }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    assert_eq!(got, 250, "bob received 250 RGB tokens without ever depositing");
    let sats_after_rgb = bob.get_balance().await?.available_sats;
    println!("SDK16 - ONBOARD (RGB): bob received 250 {asset} tokens on a fresh wallet (0 -> 250) \u{2713}");

    // --- ONBOARD #2: receive BTC with nothing -----------------------------------------------------
    let r = alice.transfer(&bob_addr, 15_000).await?;
    assert!(r.used_split);
    let want = sats_after_rgb + 15_000;
    for _ in 0..40 {
        bob.claim().await?;
        if bob.get_balance().await?.available_sats >= want { break; }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    assert_eq!(bob.get_balance().await?.available_sats, want, "bob received 15k BTC without ever depositing");
    let bob_coin = r.coins[0].statechain_id.clone();
    println!("SDK16 - ONBOARD (BTC): bob received 15k BTC on the same fresh wallet (+15_000), coin {bob_coin} \u{2713}");

    // --- EXIT: the onboarded user can leave unilaterally, with only pre-signed material -----------
    let statuses = bob.unilateral_exit(Some(vec![bob_coin.clone()]), None).await?;
    assert!(!statuses.is_empty(), "exit produced a status");
    bitcoin_core::generatetoaddress(1, &core)?;
    println!("SDK16 - EXIT: bob broadcast his branch to leave to L1 (backup then waits {} blocks) — no SE cooperation, no prior deposit",
        statuses[0].wait_blocks);

    println!("SDK16 - SUCCESS: a user with NOTHING onboarded to the statechain, received 15k BTC and 250 RGB tokens (the sender's off-chain split created both coins), and can exit unilaterally with pre-signed material. Zero on-chain footprint or funds required to enter and receive.");
    Ok(())
}
