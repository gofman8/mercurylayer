//! E2E (SDK_E2E=56) — **KEYSTONE: the signing round is IDEMPOTENT under retry**.
//!
//! A co-sign is two calls: sign/first seals a secnonce; sign/second produces the partial sig AND
//! increments the SE's `sig_count`. If the sign/second RESPONSE is lost in flight, the count has
//! advanced but the client holds no tier — and the naive recovery (a fresh sign/first) mints a NEW
//! secnonce and a NEW co-sign, so `sig_count` runs ahead of the client's disclosed tier set forever.
//! The receiver census `num_sigs == v1_backups + tiers + superseded` can then never rebalance ⟹ the
//! coin BRICKS. No attacker needed. This is the last thing gating V2-as-default.
//!
//! The lockbox now caches the produced partial sig keyed on the session and increments the count
//! ATOMICALLY with storing it. This test proves the property that makes the count census retry-safe:
//! re-sending the EXACT SAME sign/second (what a client retry does)
//!   1. returns the IDENTICAL partial signature (served from cache, not re-signed), and
//!   2. does NOT advance `sig_count` a second time.
//!
//! Before the cache, step 2 would fail: the 2nd sign/second either re-incremented the count (desync ⟹
//! brick) or hit the consumed secnonce and 400'd (unrecoverable). Run with SDK_E2E=56 (regtest +
//! lockbox stack).

use std::env;

use anyhow::{anyhow, Result};

use crate::sdk40_tesr_consensus::deposit_coin;

const NETWORK: &str = "regtest";
const FEE_RATE: f64 = 2.0;

async fn num_sigs(cc: &mercuryrustlib::client_config::ClientConfig, sid: &str) -> Result<u32> {
    Ok(mercuryrustlib::utils::get_statechain_info(sid, cc)
        .await?
        .ok_or(anyhow!("no statechain_info"))?
        .num_sigs)
}

pub async fn execute() -> Result<()> {
    let _ = std::process::Command::new("rm").arg("wallet.db").arg("wallet.db-shm").arg("wallet.db-wal").output();
    let _ = std::fs::remove_dir_all("./rgb-data-sdk56");
    env::set_var("ML_NETWORK", "regtest");
    let cc = mercuryrustlib::client_config::load().await;

    // A real coin co-signable by the live SE.
    let mut coin = deposit_coin(&cc, "sdk56_owner").await?;
    let sid = coin.statechain_id.clone().ok_or(anyhow!("no statechain_id"))?;
    let f_txid = coin.utxo_txid.clone().ok_or(anyhow!("no utxo_txid"))?;
    let f_vout = coin.utxo_vout.ok_or(anyhow!("no utxo_vout"))?;
    let f_value = coin.amount.ok_or(anyhow!("no amount"))? as u64;
    let agg = coin.aggregated_address.clone().ok_or(anyhow!("no aggregated_address"))?;

    // Capture the count BEFORE opening a session. (Reading /info/statechain BETWEEN sign/first and
    // sign/second hits a dangling NULL-challenge row — harmless once the server NULL-challenge fix is
    // deployed, but we read here so this test does not depend on that deploy.)
    let before = num_sigs(&cc, &sid).await?;
    println!("SDK56 - num_sigs before the co-sign: {before}");

    // Build ONE tier tx and drive the co-sign flow BY HAND (mirrors tesr::cosign_tier) so we can hold
    // the exact sign/second payload and replay it — the thing a client retry does.
    let t = mercurylib::tesr::build_trigger(&f_txid, f_vout, f_value, &agg, NETWORK, FEE_RATE)?;

    let coin_nonce = mercurylib::transaction::create_and_commit_nonces(&mut coin)?;
    coin.secret_nonce = Some(coin_nonce.secret_nonce);
    coin.public_nonce = Some(coin_nonce.public_nonce);
    coin.blinding_factor = Some(coin_nonce.blinding_factor);
    let server_public_nonce =
        mercuryrustlib::transaction::sign_first(&cc, &coin_nonce.sign_first_request_payload).await?;
    coin.server_public_nonce = Some(server_public_nonce);

    let partial = mercurylib::tesr::cosign_tier_request(&coin, t.tx_hex.clone(), f_value, NETWORK.to_string())?;

    // First sign/second — the real one. Count advances by exactly 1.
    let sig1 = mercuryrustlib::transaction::sign_second(&cc, &partial.partial_signature_request_payload).await?;
    let after1 = num_sigs(&cc, &sid).await?;
    println!("SDK56 - after 1st sign/second: num_sigs={after1}");
    if after1 != before + 1 {
        return Err(anyhow!("expected the first co-sign to advance num_sigs by 1: {before} -> {after1}"));
    }

    // RETRY: re-send the IDENTICAL sign/second payload (same session). This is exactly what the client
    // does when the first response is lost. It MUST be served from the cache: same sig, no new count.
    let sig2 = mercuryrustlib::transaction::sign_second(&cc, &partial.partial_signature_request_payload).await
        .map_err(|e| anyhow!("KEYSTONE: retrying the SAME sign/second failed (should be cache-served): {e}"))?;
    let after2 = num_sigs(&cc, &sid).await?;
    println!("SDK56 - after REPLAYED sign/second: num_sigs={after2}");

    if sig1.serialize() != sig2.serialize() {
        return Err(anyhow!("KEYSTONE: replay returned a DIFFERENT partial sig — cache is not serving the produced sig"));
    }
    if after2 != after1 {
        return Err(anyhow!(
            "KEYSTONE: replaying sign/second advanced num_sigs {after1} -> {after2} — a retry double-counts, which desyncs the census and bricks the coin"
        ));
    }

    // A THIRD replay, to be sure the cache is stable and never counts again.
    let sig3 = mercuryrustlib::transaction::sign_second(&cc, &partial.partial_signature_request_payload).await?;
    let after3 = num_sigs(&cc, &sid).await?;
    if sig3.serialize() != sig1.serialize() || after3 != after1 {
        return Err(anyhow!("KEYSTONE: a 3rd replay was not idempotent (sig or count changed)"));
    }

    println!("SDK56 - ✓ PASS: the signing round is idempotent under retry — identical sig served from cache, num_sigs advanced exactly once across 3 replays. The count census is retry-safe.");
    Ok(())
}
