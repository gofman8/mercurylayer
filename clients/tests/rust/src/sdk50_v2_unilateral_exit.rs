//! E2E (SDK_E2E=50) — **V2DEF-3: the SDK `unilateral_exit()` routes a laddered coin through its
//! ladder**.
//!
//! sdk49 drove the tier chain by hand through `mercuryrustlib`. This proves the *public SDK* surface:
//! a wallet whose deposit auto-established a ladder calls
//! `wallet.unilateral_exit()` and the SDK — with no ladder knowledge required by the caller — walks the
//! TES-R chain (trigger → extension → state) as each relative-CSV matures, reporting `wait_blocks`
//! between tiers, until the funds land at the wallet's own seed-derived backup address. No
//! absolute-locktime backup transaction is broadcast.
//!
//! Run with SDK_E2E=50 (needs the regtest + Mercury lockbox stack, Core 28+).

use std::time::Duration;

use anyhow::{anyhow, Result};
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};
use mercuryrustlib::CoinStatus;

use crate::bitcoin_core;
use crate::sdk40_tesr_consensus::{is_outpoint_spent, tx_exists, wait_for_address};

const NETWORK: &str = "regtest";

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    let _ = std::fs::remove_dir_all("./rgb-data-sdk50_alice");
    std::env::set_var("ML_NETWORK", "regtest");

    let cc = mercuryrustlib::client_config::load().await;
    let (alice, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk50_alice"), None).await?;

    // --- Deposit: claim() auto-establishes the ladder during the confirm loop. ---------------------
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

    let coin = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk50_alice")
        .await?
        .coins
        .iter()
        .find(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0)
        .cloned()
        .ok_or(anyhow!("no confirmed coin"))?;
    let sid = coin.statechain_id.clone().ok_or(anyhow!("no statechain_id"))?;
    let f_txid = coin.utxo_txid.clone().ok_or(anyhow!("no F txid"))?;
    let f_vout = coin.utxo_vout.ok_or(anyhow!("no F vout"))?;
    let bundle = mercuryrustlib::tesr::load(&cc, "sdk50_alice", &sid)
        .await?
        .ok_or(anyhow!("claim() did not auto-establish a ladder"))?;
    let exit_addr = bundle.owner_exit_address.clone();
    let exit_value = bundle.current().state.out_value;
    assert_eq!(exit_addr, coin.backup_address, "ladder exits to the wallet's own backup_address");
    assert!(!is_outpoint_spent(&cc, &f_txid, f_vout), "F unspent before the exit");
    println!("SDK50 - laddered coin {sid}: exiting via the ladder ({} tiers) to the wallet's backup_address", bundle.exit_tiers().len());

    // --- Drive the SDK exit: each call advances the ladder as far as CSV maturity allows. ----------
    // The caller never touches a tier tx — unilateral_exit() broadcasts the trigger, then reports how
    // many blocks until the next tier; we mine that many (regtest stand-in for wall-clock) and call
    // again. On mainnet an owner (or a background loop) simply re-invokes it per block.
    let mut statuses = alice.unilateral_exit(Some(vec![sid.clone()]), None).await?;
    assert!(!statuses[0].complete, "the multi-tier exit cannot complete in one call (trigger just broadcast)");
    let mut guard = 0;
    while !statuses[0].complete {
        guard += 1;
        assert!(guard < 12, "exit did not converge — {statuses:?}");
        let wait = statuses[0].wait_blocks.max(1);
        println!("SDK50 - tier broadcast; next tier matures in {wait} blocks — mining them");
        bitcoin_core::generatetoaddress(wait, &core)?;
        statuses = alice.unilateral_exit(Some(vec![sid.clone()]), None).await?;
    }
    bitcoin_core::generatetoaddress(2, &core)?; // confirm the final exit state

    // --- The funds are the owner's, on-chain, at his own key. F is consumed; no backup tx was used. -
    assert!(tx_exists(&cc, &bundle.current().state.txid), "the exit state confirmed on-chain");
    assert!(is_outpoint_spent(&cc, &f_txid, f_vout), "F consumed by the ladder exit");
    assert!(
        wait_for_address(&cc, &exit_addr, exit_value as u32).await.is_ok(),
        "{exit_value} sat landed at the wallet's own backup_address"
    );
    // A follow-up call is a no-op (already complete) — idempotent.
    let again = alice.unilateral_exit(Some(vec![sid.clone()]), None).await?;
    assert!(again[0].complete && again[0].wait_blocks == 0, "a completed exit stays complete");

    println!("SDK50 - ✓ PASS: wallet.unilateral_exit() walked the TES-R ladder to completion ({exit_value} sat at the owner's key, F spent, no absolute-locktime backup)");
    Ok(())
}
