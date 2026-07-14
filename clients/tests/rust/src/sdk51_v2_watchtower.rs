//! E2E (SDK_E2E=51) — **V2DEF-3: the SDK watchtower defends a *triggered* V2 coin**.
//!
//! sdk50 was the owner walking away on his own terms. This is the adversarial case the TES-R clock
//! exists for: someone ELSE spends the coin's funding `F` (a contested exit — a prior owner racing a
//! stale state, or a griefer), starting the relative-CSV clock. The owner does nothing but run the
//! background watchtower pass `wallet.defend_ladders()`; because the adopted current state carries the
//! strictly-lowest CSV, it matures first and the funds land at the OWNER's own key. Also proves the
//! pass is a no-op while the coin is idle (an un-broadcast V2 coin never ages — nothing to defend).
//!
//! Run with SDK_E2E=51 (needs the regtest + Mercury lockbox stack, Core 28+).

use std::time::Duration;

use anyhow::{anyhow, Result};
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};
use mercuryrustlib::CoinStatus;

use crate::bitcoin_core;
use crate::sdk40_tesr_consensus::{broadcast, is_outpoint_spent, tx_exists, wait_for_address};

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    let _ = std::fs::remove_dir_all("./rgb-data-sdk51_alice");
    std::env::set_var("ML_NETWORK", "regtest");
    std::env::set_var("UTEXO_PROTOCOL_DEFAULT", "2");

    let cc = mercuryrustlib::client_config::load().await;
    let (alice, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk51_alice"), None).await?;

    // --- V2-native deposit (claim() auto-establishes the ladder). ----------------------------------
    let amount = 100_000u32;
    let token = mercuryrustlib::deposit::get_token(&cc).await?;
    let t = crate::utils::handle_token_response(&cc, &token).await?;
    alice.add_prepaid_token(&t).await;
    let addr = alice.get_deposit_address(amount as u64).await?;
    bitcoin_core::sendtoaddress(amount, &addr)?;
    let core = bitcoin_core::getnewaddress()?;
    bitcoin_core::generatetoaddress(3, &core)?;
    let mut confirmed = false;
    for _ in 0..60 {
        alice.claim().await?;
        if alice.get_balance().await?.available_sats >= amount as u64 {
            confirmed = true;
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    assert!(confirmed, "deposit did not confirm");

    let coin = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk51_alice")
        .await?
        .coins
        .iter()
        .find(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0)
        .cloned()
        .ok_or(anyhow!("no confirmed coin"))?;
    let sid = coin.statechain_id.clone().ok_or(anyhow!("no statechain_id"))?;
    let f_txid = coin.utxo_txid.clone().ok_or(anyhow!("no F txid"))?;
    let f_vout = coin.utxo_vout.ok_or(anyhow!("no F vout"))?;
    let bundle = mercuryrustlib::tesr::load(&cc, "sdk51_alice", &sid)
        .await?
        .ok_or(anyhow!("no ladder"))?;
    let exit_addr = bundle.owner_exit_address.clone();
    let exit_value = bundle.current().state.out_value;

    // --- 1. IDLE coin: the watchtower is a no-op (an un-broadcast V2 coin never ages). -------------
    assert!(!is_outpoint_spent(&cc, &f_txid, f_vout), "F unspent while idle");
    assert!(alice.defend_ladders().await?.is_empty(), "an idle (un-triggered) coin is never defended");
    println!("SDK51 - idle coin: defend_ladders() is a no-op (nothing broadcast) — un-broadcast coins don't age");

    // --- 2. ADVERSARY triggers: spend F to start the CSV clock (contested exit). --------------------
    broadcast(&cc, &bundle.trigger.signed_tx)?;
    bitcoin_core::generatetoaddress(1, &core)?; // confirm the hostile trigger
    assert!(is_outpoint_spent(&cc, &f_txid, f_vout), "the adversary's trigger spent F");
    println!("SDK51 - adversary broadcast the trigger; the clock is running — owner must now defend");

    // --- 3. Owner does NOTHING but run the watchtower pass each block; the ladder races to Alice. ---
    let mut guard = 0;
    loop {
        guard += 1;
        assert!(guard < 12, "the watchtower did not converge");
        let acted = alice.defend_ladders().await?;
        if !acted.is_empty() {
            println!("SDK51 - defend_ladders broadcast a tier for {acted:?}");
        }
        match mercuryrustlib::tesr::next_exit_tier(&cc, &bundle) {
            None => break, // every tier is on-chain / in the mempool
            Some(csv) => {
                bitcoin_core::generatetoaddress(csv.max(1) as u32, &core)?;
            }
        }
    }
    bitcoin_core::generatetoaddress(2, &core)?; // confirm the final exit state

    // --- The funds are the OWNER's, purely from the watchtower — no manual exit call was made. ------
    assert!(tx_exists(&cc, &bundle.current().state.txid), "the owner's exit state confirmed");
    assert!(
        wait_for_address(&cc, &exit_addr, exit_value as u32).await.is_ok(),
        "{exit_value} sat landed at the owner's own key despite a hostile trigger"
    );
    println!("SDK51 - ✓ PASS: watchtower defended a hostile trigger — {exit_value} sat raced to the owner's key with no manual exit");
    Ok(())
}
