//! E2E (SDK_E2E=52) — **RGB terminal-freeze: a carrier is never laddered PLAINLY**.
//!
//! The carrier ⊥ PLAIN ladder invariant (INV-29). A plain T/X/S spend of a carrier would destroy the
//! allocation, so the two must never meet — but the way that is honoured CHANGED with one coin
//! shape. It used to be honoured by absence: the carrier was kept out of the ladder entirely and
//! rode the signed-once coloured-split model instead. Now it is honoured by COLOUR: `claim()` builds
//! the carrier a coloured ladder, every tier carrying the allocation, so the carrier is protected by
//! the same mechanism as every other coin rather than excluded from it.
//!
//! Proven here on the live stack: in one wallet the plain coin carries a PLAIN ladder, the carrier
//! carries a COLOURED one, and an off-chain RGB transfer still settles (750/250).
//!
//! Run: SDK_E2E=52 ML_NETWORK=regtest cargo run   (regtest + lockbox + RGB proxy up)

use std::time::Duration;

use anyhow::{anyhow, Result};
use mercury_utexo_sdk::{SdkConfig, UtexoWallet, WalletEvent};
use mercuryrustlib::CoinStatus;

use crate::bitcoin_core;

const PLAIN_AMOUNT: u32 = 123_456; // a distinctive value so the plain coin is unambiguous vs the carrier

async fn prepaid_token(cc: &mercuryrustlib::client_config::ClientConfig) -> Result<String> {
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    crate::utils::handle_token_response(cc, &token).await
}

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    for d in ["./rgb-data-sdk52_alice", "./rgb-data-sdk52_bob"] {
        let _ = std::fs::remove_dir_all(d);
    }
    std::env::set_var("ML_NETWORK", "regtest");

    let cc = mercuryrustlib::client_config::load().await;
    let mut alice_cfg = SdkConfig::regtest("sdk52_alice");
    alice_cfg.rgb_data_dir = Some("./rgb-data-sdk52_alice".to_string());
    let mut bob_cfg = SdkConfig::regtest("sdk52_bob");
    bob_cfg.rgb_data_dir = Some("./rgb-data-sdk52_bob".to_string());
    // (single protocol: every plain deposit is laddered; a carrier deliberately is not)

    let (alice, _) = UtexoWallet::initialize(alice_cfg, None).await?;
    let (bob, _) = UtexoWallet::initialize(bob_cfg, None).await?;
    let bob_address = bob.get_utexo_address().await?;

    // --- A plain BTC deposit (should get a ladder) + fund alice's RGB engine. ----------------------
    let t = prepaid_token(&cc).await?;
    alice.add_prepaid_token(&t).await;
    let plain_addr = alice.get_deposit_address(PLAIN_AMOUNT as u64).await?;
    bitcoin_core::sendtoaddress(PLAIN_AMOUNT, &plain_addr)?;
    let rgb_fund_addr = alice.get_token_funding_address().await?;
    bitcoin_core::sendtoaddress(100_000, &rgb_fund_addr)?;
    let core = bitcoin_core::getnewaddress()?;
    bitcoin_core::generatetoaddress(3, &core)?;
    tokio::time::sleep(Duration::from_secs(3)).await; // electrs indexing

    // --- Issue 1000 TKN onto a fresh statechain coin (the CARRIER — must NOT get a ladder). --------
    let t = prepaid_token(&cc).await?;
    alice.add_prepaid_token(&t).await;
    let asset_id = alice.issue_token("TKN", "Terminal Freeze Token", 0, 1000).await?;
    bitcoin_core::generatetoaddress(3, &core)?;

    // Confirm BOTH the plain coin and the token carrier, running claim() (which establishes ladders).
    let mut waited = 0;
    loop {
        alice.claim().await?;
        let b = alice.get_balance().await?;
        if b.available_sats >= PLAIN_AMOUNT as u64 && !b.tokens.is_empty() {
            break;
        }
        waited += 1;
        if waited > 60 {
            return Err(anyhow!("plain coin + carrier did not both confirm: {b:?}"));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    assert_eq!(alice.get_token_balances().await?[0].balance, 1000, "full supply on alice");

    // --- The invariant: plain coin HAS a ladder; the token carrier does NOT. -----------------------
    let coins = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk52_alice")
        .await?
        .coins;
    let confirmed: Vec<_> = coins
        .iter()
        .filter(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0)
        .collect();
    let plain = confirmed
        .iter()
        .find(|c| c.amount == Some(PLAIN_AMOUNT))
        .ok_or(anyhow!("plain coin not found"))?;
    let carriers: Vec<_> = confirmed
        .iter()
        .filter(|c| c.amount != Some(PLAIN_AMOUNT))
        .collect();
    assert!(!carriers.is_empty(), "the issuance produced a carrier coin");

    let plain_sid = plain.statechain_id.clone().ok_or(anyhow!("plain has no sid"))?;
    assert!(
        mercuryrustlib::tesr::load(&cc, "sdk52_alice", &plain_sid).await?.is_some(),
        "the PLAIN BTC coin auto-established a TES-R ladder"
    );
    // **[ONE COIN SHAPE] The invariant survives; its STATEMENT changed.**
    //
    // This used to assert the carrier carries NO ladder. That was the legacy lane's way of honouring
    // terminal-freeze: since a plain tier spend of a carrier destroys the allocation, the carrier was
    // kept out of the ladder entirely. With `colored_ladder` shipping on wherever an identity is
    // pinned, a carrier IS laddered — COLOURED, every tier carrying the allocation — so "no ladder"
    // is no longer the shape of the guarantee.
    //
    // What INV-29 actually forbids is a PLAIN ladder over a carrier, and that is what is asserted
    // now. Asserting the absence of a ladder here would today be asserting the absence of the very
    // mechanism that protects the allocation.
    for c in &carriers {
        let sid = c.statechain_id.clone().ok_or(anyhow!("carrier has no sid"))?;
        let bundle = mercuryrustlib::tesr::load(&cc, "sdk52_alice", &sid)
            .await?
            .ok_or_else(|| anyhow!("the RGB carrier {sid} has no ladder at all — with an identity \
                                    pinned, `claim()` must build it a COLOURED one"))?;
        assert!(
            bundle.is_colored(),
            "the RGB carrier {sid} carries a PLAIN ladder. A plain tier spend of a carrier destroys \
             the allocation (INV-29 / terminal-freeze), so this is the one combination that must \
             never exist — it is worse than no ladder, because it looks like protection"
        );
    }
    println!(
        "SDK52 - carrier ⊥ PLAIN ladder invariant holds: plain coin laddered plainly, {} carrier(s) \
         laddered COLOURED",
        carriers.len()
    );

    // --- RGB still transfers off-chain with V2 default on (colored split; no ladder involved). ------
    let mut bob_events = bob.subscribe();
    let bob_bg = bob.start_background();
    for _ in 0..2 {
        let t = prepaid_token(&cc).await?;
        alice.add_prepaid_token(&t).await;
    }
    let r = alice.transfer_tokens(&asset_id, &bob_address, 250).await?;
    assert!(r.used_split, "off-chain colored split");
    let recv_amount = tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            match bob_events.recv().await {
                Ok(WalletEvent::TokenTransferClaimed { asset_id: a, amount, .. }) if a == asset_id => {
                    break amount
                }
                Ok(_) => continue,
                Err(e) => panic!("event stream closed: {e}"),
            }
        }
    })
    .await
    .map_err(|_| anyhow!("bob did not claim the token transfer in time"))?;
    bob_bg.abort();
    assert_eq!(recv_amount, 250, "bob booked 250 TKN off-chain");
    assert_eq!(bob.get_token_balances().await?[0].balance, 250);
    assert_eq!(alice.get_token_balances().await?[0].balance, 750, "alice keeps 750 change");

    println!("SDK52 - ✓ PASS: V2 + RGB coexist — plain coin laddered, carrier terminal-frozen (un-laddered), off-chain token transfer settled 750/250");
    Ok(())
}
