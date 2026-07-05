//! E2E (security, honest about the trust floor): the **fresh double-sign** case — a *malicious SE*
//! that co-signs a second, conflicting spend. Unlike a stale backup (sdk13/sdk14), a freshly
//! SE-co-signed tx carries a current locktime, so the decrementing-locktime ladder gives NO
//! advantage: it degrades to a plain on-chain race (first-seen / highest-fee wins).
//!
//! We model the malicious SE with a NORMAL (non-single-use, no-budget) coin, for which the honest SE
//! already permits re-signing — so obtaining two conflicting co-signatures of one coin exercises
//! exactly the code path a single-use-ignoring SE would take. The point: two valid conflicting spends
//! exist, and only the one that confirms first wins. This is the irreducible single-SE trust floor
//! that neither the ladder, single-use, nor the terminal query can close (only threshold signing can).
//!
//! Run: SDK_E2E=15 ML_NETWORK=regtest cargo run

use anyhow::{anyhow, Result};
use electrum_client::ElectrumApi;
use mercury_spark_sdk::{SdkConfig, SparkWallet};
use mercuryrustlib::{client_config::ClientConfig, CoinStatus};

use crate::bitcoin_core;

async fn prepaid_token(cc: &ClientConfig) -> Result<String> {
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    crate::utils::handle_token_response(cc, &token).await
}

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    let cc = mercuryrustlib::client_config::load().await;
    let core = bitcoin_core::getnewaddress()?;

    let (alice, _) = SparkWallet::initialize(SdkConfig::regtest("sdk15_alice"), None).await?;

    // Fund a NORMAL statechain coin (single_use=false, no budget) -> the SE permits re-signing, which
    // is exactly what a malicious SE ignoring single-use would do for an off-chain node.
    let t = prepaid_token(&cc).await?;
    alice.add_prepaid_token(&t).await;
    let addr = alice.get_deposit_address(40_000).await?;
    bitcoin_core::sendtoaddress(40_000, &addr)?;
    bitcoin_core::generatetoaddress(3, &core)?;
    let mut waited = 0;
    while alice.get_balance().await?.available_sats != 40_000 {
        alice.claim().await?;
        waited += 1;
        if waited > 60 { return Err(anyhow!("deposit did not confirm")); }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    let coin = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk15_alice")
        .await?
        .coins
        .iter()
        .find(|c| c.status == CoinStatus::CONFIRMED && c.amount == Some(40_000))
        .ok_or_else(|| anyhow!("no confirmed coin"))?
        .clone();
    println!("SDK15 - funded a normal coin {} (SE permits re-signing)", coin.statechain_id.clone().unwrap_or_default());

    let si = mercuryrustlib::utils::info_config(&cc).await?;
    let addr_x = bitcoin_core::getnewaddress()?;
    let addr_y = bitcoin_core::getnewaddress()?;

    // --- The malicious SE co-signs TWO conflicting spends of the SAME coin ------------------------
    let mut c1 = coin.clone();
    let tx_x = mercuryrustlib::transaction::new_transaction(
        &cc, &mut c1, &addr_x, 0, true, None, "regtest", 2.0, si.initlock, si.interval,
    ).await?;
    let mut c2 = coin.clone(); // fresh nonce state
    let tx_y = mercuryrustlib::transaction::new_transaction(
        &cc, &mut c2, &addr_y, 0, true, None, "regtest", 2.0, si.initlock, si.interval,
    ).await?;
    let txx: electrum_client::bitcoin::Transaction =
        electrum_client::bitcoin::consensus::deserialize(&hex::decode(&tx_x)?)?;
    let txy: electrum_client::bitcoin::Transaction =
        electrum_client::bitcoin::consensus::deserialize(&hex::decode(&tx_y)?)?;
    // Both spend the SAME coin outpoint -> they conflict.
    assert_eq!(txx.input[0].previous_output, txy.input[0].previous_output,
        "both fresh co-signs must spend the same coin outpoint (they conflict)");
    println!("SDK15 - the SE produced TWO conflicting co-signs: tx_X {} and tx_Y {} (same input {})",
        txx.txid(), txy.txid(), txx.input[0].previous_output);

    // --- On-chain it is a plain race: only the FIRST-seen confirms; the other is rejected ---------
    let bx = cc.electrum_client.transaction_broadcast_raw(&hex::decode(&tx_x)?);
    assert!(bx.is_ok(), "first-broadcast conflicting spend is accepted: {bx:?}");
    bitcoin_core::generatetoaddress(1, &core)?;
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    let by = cc.electrum_client.transaction_broadcast_raw(&hex::decode(&tx_y)?);
    assert!(by.is_err(), "the conflicting spend loses the race (already-spent input): {by:?}");
    println!("SDK15 - tx_X won the race (confirmed); tx_Y REJECTED (its input is already spent). Only ONE of the two co-signed conflicts can settle.");

    println!("SDK15 - SUCCESS (documents the trust floor): a malicious SE CAN produce two conflicting fresh co-signatures; unlike a stale backup they carry no locktime handicap, so the decrementing-locktime ladder gives no advantage and it becomes a first-seen/highest-fee RACE. The honest party's only defence is speed + fee (be first / RBF). This is the irreducible single-SE trust — closed only by threshold-signing the SE, not by any client-side layer.");
    Ok(())
}
