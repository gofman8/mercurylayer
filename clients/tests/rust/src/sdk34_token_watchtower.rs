//! E2E (token-carrier watchtower): the `auto_exit_due` watchtower now AUTO-MATERIALIZES a received
//! token carrier that is nearing its clawback deadline — broadcasting its exit branch settles the
//! RGB allocation on-chain and spends the shared root, so a malicious sender can no longer claw the
//! tokens back. This closes the gap `sdk32` surfaced (before, the watchtower skipped carriers, so a
//! received token had NO automatic clawback protection).
//!
//! A received token carrier cannot take the PLAIN exit (an RGB-unaware sweep destroys the
//! allocation), so the watchtower materializes it: branch only, no sats-sweeping backup.
//!
//! (A) ISSUED/flat carrier is NOT materialized: it has no exit branch (no ancestor, no clawback
//!     risk), so the watchtower skips it at any margin.
//! (B) alice sends 250 to bob → bob holds a received sub-coin carrier whose branch root is alice's
//!     carrier outpoint; its deposit-anchored exit deadline D is the sender's-backup maturity.
//! (C) Comfortably ahead of D, the watchtower does NOTHING (the branch stays off-chain).
//! (D) Near D (within margin), `auto_exit_due` MATERIALIZES: it broadcasts the branch (a
//!     `TokenCarrierMaterialized` event fires), the shared root is spent, and bob still holds 250 —
//!     preserved, no SE needed.
//! (E) The clawback is now DEFEATED: after mining past D (the sender's carrier backup matures), a
//!     broadcast of that stale backup FAILS — the root it needs was already spent by the
//!     materialization. Without the watchtower, an idle bob would have lost the tokens here.
//!
//! Run: SDK_E2E=34 ML_NETWORK=regtest cargo run

use std::str::FromStr;
use std::time::Duration;

use anyhow::{anyhow, Result};
use electrum_client::ElectrumApi;
use mercury_utexo_sdk::{SdkConfig, UtexoWallet, WalletEvent};
use mercuryrustlib::{client_config::ClientConfig, CoinStatus};

use crate::bitcoin_core;

async fn prepaid_token(cc: &ClientConfig) -> Result<String> {
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    crate::utils::handle_token_response(cc, &token).await
}
async fn add_tokens(cc: &ClientConfig, w: &UtexoWallet, n: usize) -> Result<()> {
    for _ in 0..n {
        let t = prepaid_token(cc).await?;
        w.add_prepaid_token(&t).await;
    }
    Ok(())
}
async fn token_balance(w: &UtexoWallet, asset: &str) -> Result<u64> {
    Ok(w.get_token_balances().await?.into_iter().find(|t| t.asset_id == asset).map(|t| t.balance).unwrap_or(0))
}
async fn wait_token_balance(w: &UtexoWallet, asset: &str, want: u64) -> Result<()> {
    for _ in 0..60 {
        w.claim().await?;
        if token_balance(w, asset).await? == want {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Err(anyhow!("settled balance of {asset} did not reach {want}"))
}
async fn wait_carrier(cc: &ClientConfig, w: &UtexoWallet, name: &str, core: &str, asset: &str, units: u64) -> Result<mercuryrustlib::Coin> {
    for _ in 0..60 {
        bitcoin_core::generatetoaddress(1, core)?;
        w.claim().await?;
        if token_balance(w, asset).await? >= units {
            let rec = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, name).await?;
            if let Some(c) = rec.coins.iter().rev().find(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0 && c.amount == Some(10_000)) {
                return Ok(c.clone());
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Err(anyhow!("{name}: {units} of {asset} carrier did not confirm"))
}
fn tip(cc: &ClientConfig) -> Result<u32> {
    Ok(cc.electrum_client.block_headers_subscribe_raw()?.height as u32)
}
fn mine_and_sync(cc: &ClientConfig, core: &str, n: u32) -> Result<u32> {
    let start = tip(cc)?;
    let target = start + n;
    let mut mined = 0;
    while mined < n {
        let batch = (n - mined).min(200);
        bitcoin_core::generatetoaddress(batch, core)?;
        mined += batch;
    }
    for _ in 0..120 {
        if tip(cc)? >= target {
            return Ok(tip(cc)?);
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    Err(anyhow!("electrs did not catch up to height {target}"))
}
fn is_outpoint_spent(cc: &ClientConfig, txid: &str, vout: u32) -> Result<bool> {
    use electrum_client::bitcoin::Txid;
    let tx = cc.electrum_client.transaction_get(&Txid::from_str(txid)?)?;
    let spk = &tx.output[vout as usize].script_pubkey;
    Ok(!cc.electrum_client.script_list_unspent(spk)?.iter().any(|u| u.tx_hash.to_string() == txid && u.tx_pos as u32 == vout))
}
fn drain_materialized(rx: &mut tokio::sync::broadcast::Receiver<WalletEvent>) -> Vec<String> {
    let mut out = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        if let WalletEvent::TokenCarrierMaterialized { statechain_id, .. } = ev {
            out.push(statechain_id);
        }
    }
    out
}

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    for d in ["./rgb-data-sdk34_alice", "./rgb-data-sdk34_bob"] {
        let _ = std::fs::remove_dir_all(d);
    }
    let cc = mercuryrustlib::client_config::load().await;
    let core = bitcoin_core::getnewaddress()?;
    let initlock = mercuryrustlib::utils::info_config(&cc).await?.initlock;

    let (alice, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk34_alice"), None).await?;
    let (bob, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk34_bob"), None).await?;
    let bob_addr = bob.get_utexo_address().await?;

    let rgb_fund = alice.get_token_funding_address().await?;
    bitcoin_core::sendtoaddress(600_000, &rgb_fund)?;
    bitcoin_core::generatetoaddress(3, &core)?;
    tokio::time::sleep(Duration::from_secs(4)).await;

    add_tokens(&cc, &alice, 1).await?;
    let asset = alice.issue_token("WT", "Watch Token", 0, 1000).await?;
    let carrier = wait_carrier(&cc, &alice, "sdk34_alice", &core, &asset, 1000).await?;
    let carrier_id = carrier.statechain_id.clone().ok_or_else(|| anyhow!("carrier has no id"))?;
    println!("SDK34 - alice issued 1000 {asset} on a flat carrier {carrier_id}; initlock={initlock}");

    // ===== (A) ISSUED/flat carrier is not materialized ==========================================
    // A flat carrier has no exit branch (no ancestor → no clawback risk), so the watchtower skips it
    // even at an enormous margin: there is nothing to materialize.
    let a = alice.auto_exit_due(1_000_000).await?;
    assert!(
        !a.contains(&carrier_id),
        "the issued flat carrier has no branch and must NOT be materialized (got {a:?})"
    );
    println!("SDK34 - (A) issued flat carrier {carrier_id} is NOT materialized (no branch, no clawback risk) — watchtower skips it");

    // ===== (B) set up a RECEIVED carrier ========================================================
    add_tokens(&cc, &alice, 3).await?;
    let r = alice.transfer_tokens(&asset, &bob_addr, 250).await?;
    let bob_piece = r.coins[0].statechain_id.clone();
    wait_token_balance(&bob, &asset, 250).await?;
    // bob's exit branch root = alice's carrier outpoint (the colored split's input).
    let branch = mercuryrustlib::sqlite_manager::get_backup_txs(&cc.pool, "sdk34_bob", &format!("branch-{bob_piece}")).await?;
    let split_tx: electrum_client::bitcoin::Transaction =
        electrum_client::bitcoin::consensus::deserialize(&hex::decode(&branch[0].tx)?)?;
    let root = split_tx.input[0].previous_output;
    let est0 = bob.estimate_exit_cost(&bob_piece).await?;
    let d = est0.exit_deadline_block.ok_or_else(|| anyhow!("received carrier has no exit deadline"))?;
    let headroom0 = d as i64 - tip(&cc)? as i64;
    assert!(headroom0 > 100, "bob's received carrier should start well ahead of its deadline (headroom {headroom0})");
    println!("SDK34 - (B) alice→bob 250 {asset}: bob holds received carrier {bob_piece}; exit deadline D={d} (headroom {headroom0}), root {}:{}", root.txid, root.vout);

    // ===== (C) watchtower does NOTHING while comfortably ahead of D ==============================
    let none = bob.auto_exit_due(20).await?;
    assert!(!none.contains(&bob_piece), "far from the deadline, the carrier must not be materialized yet");
    assert!(!is_outpoint_spent(&cc, &root.txid.to_string(), root.vout)?, "the branch is still off-chain (root unspent)");
    println!("SDK34 - (C) comfortably ahead of D (headroom {headroom0} > margin 20): watchtower is a no-op, branch stays off-chain");

    // ===== (D) near D → AUTO-MATERIALIZE =========================================================
    mine_and_sync(&cc, &core, 100)?;
    let headroom = d as i64 - tip(&cc)? as i64;
    assert!(headroom > 0, "still before the deadline when the watchtower fires (headroom {headroom})");
    let margin = (headroom + 30) as u32; // guarantees tip + margin >= D → due
    let mut rx = bob.subscribe();
    let acted = bob.auto_exit_due(margin).await?;
    assert!(acted.contains(&bob_piece), "the watchtower must materialize the near-deadline carrier (got {acted:?})");
    let mat = drain_materialized(&mut rx);
    assert!(mat.contains(&bob_piece), "a TokenCarrierMaterialized event must fire for {bob_piece} (got {mat:?})");
    // The branch is broadcast; confirm it and verify the shared root is now spent by bob's branch.
    bitcoin_core::generatetoaddress(3, &core)?;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(is_outpoint_spent(&cc, &root.txid.to_string(), root.vout)?, "materialization spent the shared root");
    for _ in 0..20 {
        let _ = bob.claim().await?;
        if token_balance(&bob, &asset).await? == 250 {
            break;
        }
        bitcoin_core::generatetoaddress(1, &core)?;
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    assert_eq!(token_balance(&bob, &asset).await?, 250, "bob's 250 tokens are preserved on-chain (materialized, no SE)");
    println!("SDK34 - (D) near D (headroom {headroom} ≤ margin {margin}): auto_exit_due MATERIALIZED {bob_piece} (TokenCarrierMaterialized event); root spent, bob still holds 250 {asset} — settled on-chain, no SE");

    // ===== (E) the clawback is now DEFEATED =====================================================
    // The sender's stale carrier backup (its input = the shared root) matures at ~D. Mine past it and
    // try to broadcast it: it FAILS because the root was already spent by bob's materialization.
    let alice_backup = mercuryrustlib::sqlite_manager::get_backup_txs(&cc.pool, "sdk34_alice", &carrier_id).await?;
    let alice_bk = alice_backup.first().ok_or_else(|| anyhow!("alice has no carrier backup"))?;
    let l0_alice = mercurylib::utils::get_blockheight(alice_bk)?;
    mine_and_sync(&cc, &core, d.max(l0_alice).saturating_sub(tip(&cc)?) + 20)?;
    assert!(tip(&cc)? > l0_alice, "the sender's carrier backup is now mature (tip > L0={l0_alice})");
    let claw = cc.electrum_client.transaction_broadcast_raw(&hex::decode(&alice_bk.tx)?);
    assert!(claw.is_err(), "the sender's stale backup must FAIL to broadcast — its root was already spent by the materialization");
    assert!(is_outpoint_spent(&cc, &root.txid.to_string(), root.vout)?, "the root remains spent by bob's branch (not the sender's backup)");
    assert_eq!(token_balance(&bob, &asset).await?, 250, "bob still holds all 250 after the failed clawback attempt");
    println!("SDK34 - (E) CLAWBACK DEFEATED: even with its backup mature (tip {} > L0 {l0_alice}), the sender cannot sweep the root — {} — bob keeps 250; without the watchtower an idle bob would have lost them here", tip(&cc)?, claw.err().map(|e| e.to_string().chars().take(80).collect::<String>()).unwrap_or_default());

    println!("SDK34 - SUCCESS: the auto_exit_due watchtower now AUTO-MATERIALIZES received token carriers before their clawback deadline (branch-only, allocation preserved), while leaving issued/flat carriers (no branch, no clawback risk) untouched. The received-token gap sdk32 found is closed: an idle receiver is automatically protected — the shared root is spent in time, so the sender's stale backup can never claw the tokens back.");
    Ok(())
}
