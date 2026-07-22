//! E2E (SDK_E2E=57) — **FATAL-B Stage 1: authoritative sid -> aggregate binding**.
//!
//! The in-ladder split's parent census is defeated by a rogue-key decoy-counter unless the server
//! records an AUTHORITATIVE aggregate per statechain_id (owner_share + enclave_share), UNIQUE, that a
//! receiver can check a coin against — instead of trusting a sender-supplied owner key. This proves the
//! foundation is live: a fresh deposit sends the owner signing share, the server records the aggregate,
//! and /info/statechain returns it EQUAL to the coin's own aggregate x-only.
//!
//! Run: SDK_E2E=57 ML_NETWORK=regtest (regtest + lockbox stack, with the migration-0009 server).

use std::env;

use anyhow::{anyhow, Result};

use crate::sdk40_tesr_consensus::deposit_coin;

const NETWORK: &str = "regtest";

pub async fn execute() -> Result<()> {
    let _ = std::process::Command::new("rm").arg("wallet.db").arg("wallet.db-shm").arg("wallet.db-wal").output();
    let _ = std::fs::remove_dir_all("./rgb-data-sdk57");
    env::set_var("ML_NETWORK", "regtest");
    let cc = mercuryrustlib::client_config::load().await;

    // Deposit a coin — the client now sends its owner signing share (user_public_key) in DepositMsg1.
    let coin = deposit_coin(&cc, "sdk57_owner").await?;
    let sid = coin.statechain_id.clone().ok_or(anyhow!("no statechain_id"))?;

    // The coin's OWN aggregate x-only: create_aggregated_address returns the full compressed aggregate
    // pubkey; its x-only is the last 64 hex chars (drop the 1-byte 02/03 prefix) — exactly what the
    // server stores (hex of aggregate.x_only.serialize()).
    let agg = mercurylib::deposit::create_aggregated_address(&coin, NETWORK.to_string())?;
    if agg.aggregate_pubkey.len() != 66 {
        return Err(anyhow!("unexpected aggregate pubkey format: {}", agg.aggregate_pubkey));
    }
    let coin_aggregate_xonly = agg.aggregate_pubkey[2..].to_lowercase();

    // The server's AUTHORITATIVE record, via /info/statechain.
    let info = mercuryrustlib::utils::get_statechain_info(&sid, &cc)
        .await?
        .ok_or(anyhow!("no statechain_info"))?;

    let server_aggregate = info
        .aggregate_pubkey
        .clone()
        .ok_or(anyhow!("FATAL-B: server did NOT record an aggregate for a V2-native deposit — owner-share binding not active"))?
        .to_lowercase();

    println!("SDK57 - coin aggregate x-only:   {coin_aggregate_xonly}");
    println!("SDK57 - server-recorded aggregate: {server_aggregate}");

    if server_aggregate != coin_aggregate_xonly {
        return Err(anyhow!(
            "FATAL-B: server aggregate {server_aggregate} != the coin's aggregate {coin_aggregate_xonly} — the sid->aggregate binding is wrong"
        ));
    }

    println!("SDK57 - ✓ PASS: the server records an authoritative sid->aggregate binding equal to the coin's own aggregate; the receiver can now verify a coin against it (FATAL-B Stage 1 live).");
    Ok(())
}
