//! E2E (SDK_E2E=48) — **V2DEF-2 Stage 2a: laddered deposit** on the live SE + real bitcoind.
//!
//! The SDK `claim()` pass
//! auto-establishes + persists a TES-R exit ladder for each fresh CONFIRMED deposit — so the coin
//! is laddered without any manual `establish` call. Proven here against the live SE:
//!   - a ladder is auto-persisted (`tesr-<id>`), exiting to the wallet's seed-derived backup_address
//!     (recoverable — NOT an out-of-wallet key);
//!   - the SE's num_sigs == 4 (the deposit's signed-once backup tx1 + T + X + S), and R′
//!     `verify_bundle` ACCEPTS it (sound);
//!   - it is idempotent: a second `claim()` does not double-establish (num_sigs stays 4).
//!
//! Run with SDK_E2E=48 (needs the regtest + Mercury lockbox stack, Core 28+).

use std::time::Duration;

use anyhow::{anyhow, Result};
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};
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
    let _ = std::fs::remove_dir_all("./rgb-data-sdk48_alice");
    std::env::set_var("ML_NETWORK", "regtest");
    // Every deposit is laddered: the SDK claim() hook auto-establishes the TES-R ladder.

    let cc = mercuryrustlib::client_config::load().await;
    let (alice, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk48_alice"), None).await?;
    // (there is no protocol switch any more — every deposit is laddered)

    // Deposit + confirm; claim() auto-establishes the ladder during the confirm loop.
    let amount = 100_000u32;
    let t = prepaid_token(&cc).await?;
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
    // A second claim proves idempotency (must NOT double-establish).
    alice.claim().await?;

    // Verify via the shared wallet.db.
    let coin = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk48_alice")
        .await?
        .coins
        .iter()
        .find(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0)
        .cloned()
        .ok_or(anyhow!("no confirmed coin"))?;
    let sid = coin.statechain_id.clone().ok_or(anyhow!("no statechain_id"))?;

    let bundle = mercuryrustlib::tesr::load(&cc, "sdk48_alice", &sid)
        .await?
        .ok_or(anyhow!("claim() did not auto-establish a ladder"))?;
    println!("SDK48 - deposit auto-established a ladder ({} tiers) for {sid}", bundle.exit_tiers().len());

    // Exit payee is the wallet's own seed-derived backup_address (recoverable from the mnemonic).
    assert_eq!(bundle.owner_exit_address, coin.backup_address, "ladder exits to the wallet's backup_address, not an external key");

    // Sound sig-count vs the LIVE SE: the deposit's signed-once backup tx1 (1) + T + X + S (3) = 4;
    // still 4 after the idempotent claim.
    let num_sigs = mercuryrustlib::utils::get_statechain_info(&sid, &cc)
        .await?
        .ok_or(anyhow!("no statechain_info"))?
        .num_sigs;
    assert_eq!(num_sigs, 4, "1 backup tx1 + 3 tiers; idempotent (a 2nd claim did not re-establish)");
    mercuryrustlib::tesr::verify_bundle(&bundle, num_sigs, 1)
        .map_err(|e| anyhow!("R′ rejected the auto-established ladder: {e}"))?;

    println!("SDK48 - ✓ PASS: deposit is laddered natively — ladder auto-established, exits to backup_address, num_sigs=4, R′ verified, idempotent");
    Ok(())
}
