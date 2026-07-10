//! E2E (sponsored-refresh bounded loss): a sponsor that STIFFS the user after the on-chain
//! re-anchor costs the user only the fee — never the coin. `refresh_sponsored` re-anchors first
//! (user-pays, fee from the coin), then asks a funded sponsor to rebate the fee off-chain. If the
//! sponsor cannot pay (here: zero balance), the call returns an explicit error but the user KEEPS
//! the refreshed `amount − fee` coin. Closes the TRUST-MODEL §7 refresh-sponsor test gap (sdk30
//! covers only the happy path).
//!
//! Run: SDK_E2E=38 ML_NETWORK=regtest cargo run

use std::time::Duration;

use anyhow::{anyhow, Result};
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};
use mercuryrustlib::{client_config::ClientConfig, CoinStatus};

use crate::bitcoin_core;

async fn prepaid_token(cc: &ClientConfig) -> Result<String> {
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    crate::utils::handle_token_response(cc, &token).await
}

async fn deposit_confirmed(cc: &ClientConfig, w: &UtexoWallet, name: &str, amount: u32) -> Result<()> {
    let t = prepaid_token(cc).await?;
    w.add_prepaid_token(&t).await;
    let addr = w.get_deposit_address(amount as u64).await?;
    bitcoin_core::sendtoaddress(amount, &addr)?;
    let core = bitcoin_core::getnewaddress()?;
    bitcoin_core::generatetoaddress(3, &core)?;
    for _ in 0..60 {
        w.claim().await?;
        if w.get_balance().await?.available_sats >= amount as u64 {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Err(anyhow!("{name}: deposit of {amount} did not confirm"))
}

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    for d in ["./rgb-data-sdk38_carol", "./rgb-data-sdk38_sponsor"] {
        let _ = std::fs::remove_dir_all(d);
    }
    let cc = mercuryrustlib::client_config::load().await;
    let core = bitcoin_core::getnewaddress()?;

    let (carol, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk38_carol"), None).await?;
    // A sponsor with ZERO balance — it will be asked to rebate the fee and cannot.
    let (sponsor, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk38_sponsor"), None).await?;
    // carol needs prepaid tokens for the re-anchor's fresh deposit slot.
    for _ in 0..3 {
        let t = prepaid_token(&cc).await?;
        carol.add_prepaid_token(&t).await;
    }

    let dep = 50_000u32;
    deposit_confirmed(&cc, &carol, "sdk38_carol", dep).await?;
    let id = {
        let rec = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk38_carol").await?;
        rec.coins.iter().find(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0)
            .and_then(|c| c.statechain_id.clone()).ok_or_else(|| anyhow!("no confirmed coin"))?
    };
    assert_eq!(sponsor.get_balance().await?.available_sats, 0, "the sponsor starts broke");
    println!("SDK38 - carol has a {dep}-sat coin {id}; sponsor balance is 0 (it cannot rebate)");

    // Sponsored refresh: the re-anchor happens (user pays the fee from the coin), then the broke
    // sponsor fails to rebate -> explicit error, but the refreshed coin is already carol's.
    let res = carol.refresh_sponsored(&id, &sponsor, None).await;
    let err = res.expect_err("a broke sponsor must make refresh_sponsored return an error");
    let msg = err.to_string();
    assert!(
        msg.contains("rebate") && msg.to_lowercase().contains("re-anchor succeeded"),
        "the error must state the re-anchor succeeded but the sponsor rebate failed, got: {msg}"
    );
    println!("SDK38 - refresh_sponsored errored as documented: {msg}");

    // Confirm the re-anchor + claim: carol holds the refreshed coin = deposit − fee (112 @ 1 sat/vB),
    // and NO rebate arrived. The old coin is spent on-chain.
    let expect = dep as u64 - 112;
    let mut ok = false;
    for _ in 0..60 {
        bitcoin_core::generatetoaddress(1, &core)?;
        let _ = carol.claim().await?;
        if carol.get_balance().await?.available_sats == expect {
            ok = true;
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    assert!(ok, "carol must hold the refreshed coin ({expect} sats) after the failed rebate; got {}", carol.get_balance().await?.available_sats);
    let old_withdrawn = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk38_carol").await?
        .coins.iter().any(|c| c.statechain_id.as_deref() == Some(&id) && c.duplicate_index == 0 && c.status == CoinStatus::WITHDRAWN);
    assert!(old_withdrawn, "the old coin was spent by the re-anchor");
    assert_eq!(sponsor.get_balance().await?.available_sats, 0, "the broke sponsor spent nothing");
    println!("SDK38 - carol keeps the refreshed coin ({expect} = {dep} − 112 fee); no rebate; sponsor still 0");

    println!("SDK38 - SUCCESS: a stiffing sponsor costs the user only the on-chain fee, never the coin. refresh_sponsored re-anchors first (user-pays) then requests the off-chain rebate; a broke sponsor makes it return an explicit \"re-anchor succeeded but the sponsor rebate failed\" error while the user keeps the refreshed amount−fee coin. Bounded-loss claim (TRUST-MODEL §7) verified.");
    Ok(())
}
