//! E2E (tokens over time): what happens to statechain tokens if you ISSUE or RECEIVE them and then
//! do NOTHING for a long time (well past every backup-ladder horizon — a "year" is far beyond the
//! ~7-day deployed horizon). Answers, empirically: are they lost? can you still send them, exit
//! unilaterally, or exit cooperatively?
//!
//! (A) ISSUED tokens (flat carrier), long idle: the carrier's own backup ladder FLOORS (its locktime
//!     drops below the tip as the chain advances), yet the tokens are NOT lost and are STILL
//!     sendable — a token transfer is a colored SPLIT and the piece gets a FRESH ladder
//!     (`create_tx1`, qt=0), so a floored carrier does not block sending. Movement needs the SE
//!     (a plain unilateral exit is refused — it would destroy the allocation); issued tokens have
//!     NO ancestor, so there is NO clawback risk.
//! (B) RECEIVED tokens (sub-coin carrier), long idle: still NOT lost. UNILATERAL materialization
//!     works forever — the exit branch is locktime-0, so broadcasting it settles the allocation
//!     on-chain any time (provided the shared root is still unspent). A single 1,500-sat received
//!     piece is below the carrier floor, so it can't be re-sent alone (hold / combine / exit).
//! (C) The DANGER for received tokens: after the wait, the SENDER's carrier backup has ALSO matured,
//!     so a malicious sender could sweep the shared root and claw the tokens back — UNLESS you
//!     materialized first. The received holder must exit before the root deadline; there is no
//!     automatic protection for token carriers today (`auto_exit_due` skips carriers).
//!
//! Run: SDK_E2E=32 ML_NETWORK=regtest cargo run

use std::str::FromStr;
use std::time::Duration;

use anyhow::{anyhow, Result};
use electrum_client::ElectrumApi;
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};
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
/// Mine `n` blocks in batches and wait until electrs's tip catches up (so later reads are current).
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
fn parse_tx(h: &str) -> Result<electrum_client::bitcoin::Transaction> {
    Ok(electrum_client::bitcoin::consensus::deserialize(&hex::decode(h)?)?)
}
fn is_outpoint_spent(cc: &ClientConfig, txid: &str, vout: u32) -> Result<bool> {
    use electrum_client::bitcoin::Txid;
    let tx = cc.electrum_client.transaction_get(&Txid::from_str(txid)?)?;
    let spk = &tx.output[vout as usize].script_pubkey;
    Ok(!cc.electrum_client.script_list_unspent(spk)?.iter().any(|u| u.tx_hash.to_string() == txid && u.tx_pos as u32 == vout))
}
async fn coin_of(cc: &ClientConfig, name: &str, id: &str) -> Result<mercuryrustlib::Coin> {
    mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, name).await?
        .coins.into_iter().find(|c| c.statechain_id.as_deref() == Some(id) && c.duplicate_index == 0)
        .ok_or_else(|| anyhow!("{name} has no coin {id}"))
}

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    for d in ["./rgb-data-sdk32_alice", "./rgb-data-sdk32_bob", "./rgb-data-sdk32_carol"] {
        let _ = std::fs::remove_dir_all(d);
    }
    let cc = mercuryrustlib::client_config::load().await;
    let core = bitcoin_core::getnewaddress()?;
    let initlock = mercuryrustlib::utils::info_config(&cc).await?.initlock;

    let (alice, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk32_alice"), None).await?;
    let (bob, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk32_bob"), None).await?;
    let (carol, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk32_carol"), None).await?;
    let bob_addr = bob.get_utexo_address().await?;
    let carol_addr = carol.get_utexo_address().await?;

    let rgb_fund = alice.get_token_funding_address().await?;
    bitcoin_core::sendtoaddress(600_000, &rgb_fund)?;
    bitcoin_core::generatetoaddress(3, &core)?;
    tokio::time::sleep(Duration::from_secs(4)).await;

    // ===== (A) ISSUED tokens (flat carrier) idle well past the ladder horizon ====================
    add_tokens(&cc, &alice, 1).await?;
    let asset = alice.issue_token("YR", "Year Token", 0, 1000).await?;
    let carrier = wait_carrier(&cc, &alice, "sdk32_alice", &core, &asset, 1000).await?;
    let carrier_id = carrier.statechain_id.clone().ok_or_else(|| anyhow!("carrier has no id"))?;
    let l0 = carrier.locktime.ok_or_else(|| anyhow!("carrier has no backup locktime"))?;
    println!("SDK32 - alice issued 1000 {asset} on a flat carrier; backup ladder L0={l0}, initlock={initlock}");

    // Do nothing for a "year": advance the chain far past L0 (the 1000-block ladder floors ~7 days in).
    let tip_yr = mine_and_sync(&cc, &core, initlock + 500)?;
    assert!(l0 < tip_yr, "the carrier's own backup ladder has FLOORED (L0={l0} < tip={tip_yr})");
    alice.claim().await?;
    assert_eq!(token_balance(&alice, &asset).await?, 1000, "the tokens are NOT lost after long inactivity");
    println!("SDK32 - (A) after ~{} idle blocks the carrier ladder is FLOORED (L0={l0} < tip={tip_yr}), yet alice still holds all 1000 {asset}", initlock + 500);

    // A PLAIN unilateral exit of the (still-current) issued carrier is REFUSED — it would sweep the
    // sats and destroy the allocation. Issued tokens are anchored on-chain and move only via the SE
    // (colored) path; they are never lost and have NO clawback risk (no ancestor above them).
    let plain_exit = alice.unilateral_exit(Some(vec![carrier_id.clone()]), None).await;
    assert!(plain_exit.is_err(), "a plain unilateral exit of a token carrier must be refused (carrier guard)");
    println!("SDK32 - (A) a plain unilateral exit of the issued carrier is refused (would destroy tokens); issued tokens move only via the SE, are never lost, and have NO clawback risk");

    // COOPERATIVE SEND still works despite the floored ladder — the piece gets a FRESH ladder.
    add_tokens(&cc, &alice, 3).await?;
    let r = alice.transfer_tokens(&asset, &bob_addr, 250).await?;
    let bob_piece = r.coins[0].statechain_id.clone();
    wait_token_balance(&bob, &asset, 250).await?;
    assert_eq!(token_balance(&alice, &asset).await?, 750, "alice sent 250 from a floored carrier");
    let bob_coin = coin_of(&cc, "sdk32_bob", &bob_piece).await?;
    let bob_l = bob_coin.locktime.ok_or_else(|| anyhow!("bob piece no locktime"))?;
    let tip_now = tip(&cc)?;
    assert!(
        bob_l as i64 - tip_now as i64 > (initlock as i64) - 20,
        "bob's received piece has a FRESH ladder (headroom ~initlock: L={bob_l}, tip={tip_now})"
    );
    println!("SDK32 - (A) COOPERATIVE SEND works after a year: alice→bob 250 (balances 750/250); bob's piece has a FRESH ladder (headroom {} ≈ initlock) — a floored carrier does NOT block sending", bob_l as i64 - tip_now as i64);

    // ===== (B) RECEIVED tokens (sub-coin carrier) idle well past the horizon =====================
    // bob's 250-piece branch = the colored split tx; its root is alice's carrier outpoint.
    let branch = mercuryrustlib::sqlite_manager::get_backup_txs(&cc.pool, "sdk32_bob", &format!("branch-{bob_piece}")).await?;
    assert_eq!(branch.len(), 1, "bob's piece is a depth-1 sub-coin (branch = the colored split)");
    let split_tx = parse_tx(&branch[0].tx)?;
    let root = split_tx.input[0].previous_output;

    let tip_yr2 = mine_and_sync(&cc, &core, initlock + 500)?;
    bob.claim().await?;
    assert_eq!(token_balance(&bob, &asset).await?, 250, "bob's received tokens are NOT lost after long inactivity");
    let bob_l2 = coin_of(&cc, "sdk32_bob", &bob_piece).await?.locktime.unwrap_or(0);
    assert!(bob_l2 < tip_yr2, "bob's own piece backup ladder has also floored (L={bob_l2} < tip={tip_yr2})");
    println!("SDK32 - (B) after another ~{} idle blocks bob still holds 250 {asset}; his piece ladder floored too (L={bob_l2} < tip={tip_yr2})", initlock + 500);

    // A single 1,500-sat received piece is below the carrier floor → cannot be re-sent alone.
    let resend = bob.transfer_tokens(&asset, &carol_addr, 100).await;
    assert!(resend.is_err(), "a lone 1,500-sat received piece is too small to re-send (hold / combine / exit)");
    println!("SDK32 - (B) a lone received piece can't be re-sent (below the carrier floor): {:?}", resend.err().map(|e| e.to_string()));

    // UNILATERAL materialization works forever: the branch is locktime-0. Broadcast it → the tokens
    // settle on-chain (the root is still unspent because alice — the sender — did not claw back).
    assert!(!is_outpoint_spent(&cc, &root.txid.to_string(), root.vout)?, "the shared root is still unspent (honest sender)");
    for row in &branch {
        cc.electrum_client.transaction_broadcast_raw(&hex::decode(&row.tx)?)?;
    }
    bitcoin_core::generatetoaddress(3, &core)?;
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(is_outpoint_spent(&cc, &root.txid.to_string(), root.vout)?, "materializing spends the shared root");
    for _ in 0..20 {
        let _ = bob.claim().await?;
        if token_balance(&bob, &asset).await? == 250 {
            break;
        }
        bitcoin_core::generatetoaddress(1, &core)?;
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    assert_eq!(token_balance(&bob, &asset).await?, 250, "250 units materialize on-chain (unilateral exit preserves the tokens)");
    println!("SDK32 - (B) UNILATERAL materialization works after a year: bob broadcast the locktime-0 branch, spent the root, and settled 250 {asset} on-chain — tokens preserved without the SE");

    // ===== (C) the clawback WINDOW for received tokens ==========================================
    // Alice's carrier backup (L0) matured long ago (L0 < tip), so a MALICIOUS sender could have swept
    // the shared root before bob materialized — clawing the tokens back. bob was safe only because he
    // exited. This is why a received holder must materialize before the root deadline.
    let alice_backup = mercuryrustlib::sqlite_manager::get_backup_txs(&cc.pool, "sdk32_alice", &carrier_id).await?;
    let alice_bk_lock = alice_backup.first().map(|b| mercurylib::utils::get_blockheight(b).unwrap_or(0)).unwrap_or(0);
    let tip_c = tip(&cc)?;
    assert!(alice_bk_lock <= tip_c, "the SENDER's carrier backup matured (L0={alice_bk_lock} <= tip={tip_c}) — clawback was possible");
    println!("SDK32 - (C) CLAWBACK WINDOW: the sender's carrier backup matured at {alice_bk_lock} (tip {tip_c}); a malicious sender could have swept the root had bob stayed idle — received tokens MUST be materialized before the root deadline (auto_exit_due does not cover carriers today)");

    println!("SDK32 - SUCCESS: tokens are NEVER LOST by inactivity. ISSUED tokens (flat carrier): the ladder floors but they stay sendable (piece gets a fresh ladder) and move via the SE — no clawback risk, but a plain unilateral exit would destroy them. RECEIVED tokens (sub-coin): unilateral materialization works forever (locktime-0 branch) and preserves them IF the shared root is still unspent — but a lone piece can't be re-sent, and a malicious sender can claw back past the root deadline, so a received holder should exit promptly. Cooperative operations (with the SE) work throughout.");
    Ok(())
}
