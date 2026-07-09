//! E2E (auto-refresh embedded in transfer): a coin nearing its backup-ladder floor is re-anchored
//! AUTOMATICALLY before it is spent, so an aging coin is transparent to the user — the transfer just
//! succeeds and the only visible effect is the re-anchor fee. This is the "embedded into the state
//! transition" refresh: `if coin is close to final it is refreshed automatically before the user's
//! action, and that is just a fee applied to the transfer`.
//!
//! The auto-refresh trigger is a headroom margin (`SdkConfig::auto_refresh_margin_blocks`, default
//! 144 ≈ 1 day). To keep the test fast and profile-independent it sets a large margin
//! (`initlock − 150`) so a coin becomes "near final" after only ~260 mined blocks instead of
//! ~initlock.
//!
//! (A) MAINTENANCE PASS — `auto_refresh_due(margin)`: a FRESH coin (headroom ≈ initlock > margin) is
//!     NOT refreshed (no churn). After mining ages it below the margin, the pass re-anchors it
//!     (user-pays, fee 112 from the coin): old coin WITHDRAWN, fresh coin = deposit − 112 with a
//!     reset full ladder (L_new > L0). Re-running the pass then refreshes nothing (fresh coin is
//!     above the margin again).
//! (B) EMBEDDED IN transfer(): alice holds an aged (near-final) coin and calls transfer() WITHOUT any
//!     manual refresh. The transfer re-anchors the coin first (a `CoinRefreshed` event fires), waits
//!     for it to confirm, then sends — bob receives the exact amount and alice paid only the fee.
//!     Because the transfer blocks on the on-chain confirmation, the test mines concurrently (as the
//!     background watcher would pre-empt in production).
//! (C) OPT-OUT: with `auto_refresh = false` a transfer of the same aged coin does NOT re-anchor it
//!     (no `CoinRefreshed` event) — the setting is respected.
//!
//! Run: SDK_E2E=33 ML_NETWORK=regtest cargo run

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

/// Pre-pay `n` deposit tokens into the wallet's pool (each deposit / split output / re-anchor
/// consumes one).
async fn prepay(cc: &ClientConfig, w: &UtexoWallet, n: usize) -> Result<()> {
    for _ in 0..n {
        let t = prepaid_token(cc).await?;
        w.add_prepaid_token(&t).await;
    }
    Ok(())
}

/// Deposit `amount` to `w`, mine, and poll claim until the wallet holds the CONFIRMED coin.
async fn deposit_confirmed_coin(
    cc: &ClientConfig,
    w: &UtexoWallet,
    wallet_name: &str,
    amount: u32,
) -> Result<mercuryrustlib::Coin> {
    let addr = w.get_deposit_address(amount as u64).await?;
    bitcoin_core::sendtoaddress(amount, &addr)?;
    let core = bitcoin_core::getnewaddress()?;
    bitcoin_core::generatetoaddress(3, &core)?;
    for _ in 0..60 {
        let _ = w.claim().await?;
        let rec = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, wallet_name).await?;
        if let Some(c) = rec.coins.iter().rev().find(|c| {
            c.status == CoinStatus::CONFIRMED
                && c.amount == Some(amount)
                && c.aggregated_address.as_deref() == Some(addr.as_str())
        }) {
            return Ok(c.clone());
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Err(anyhow!("deposit of {amount} sats did not confirm for {wallet_name}"))
}

async fn coin_by_id(cc: &ClientConfig, wallet_name: &str, id: &str) -> Result<mercuryrustlib::Coin> {
    let rec = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, wallet_name).await?;
    rec.coins
        .iter()
        .find(|c| c.statechain_id.as_deref() == Some(id) && c.duplicate_index == 0)
        .cloned()
        .ok_or_else(|| anyhow!("{wallet_name} has no coin with statechain id {id}"))
}

fn tip(cc: &ClientConfig) -> Result<u32> {
    Ok(cc.electrum_client.block_headers_subscribe_raw()?.height as u32)
}

/// Drain a broadcast receiver and count `CoinRefreshed` events, returning the `old_statechain_id`s.
fn drain_refresh_events(rx: &mut tokio::sync::broadcast::Receiver<WalletEvent>) -> Vec<String> {
    let mut refreshed = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        if let WalletEvent::CoinRefreshed { old_statechain_id, .. } = ev {
            refreshed.push(old_statechain_id);
        }
    }
    refreshed
}

fn cfg(name: &str, auto_refresh: bool, margin: u32) -> SdkConfig {
    let mut c = SdkConfig::regtest(name);
    c.auto_refresh = auto_refresh;
    c.auto_refresh_margin_blocks = margin;
    c
}

const DEP: u32 = 50_000;

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    for d in [
        "./rgb-data-sdk33_alice",
        "./rgb-data-sdk33_bob",
        "./rgb-data-sdk33_bob2",
        "./rgb-data-sdk33_carol",
        "./rgb-data-sdk33_dave",
    ] {
        let _ = std::fs::remove_dir_all(d);
    }
    let cc = mercuryrustlib::client_config::load().await;
    let core = bitcoin_core::getnewaddress()?;

    let info = mercuryrustlib::utils::info_config(&cc).await?;
    let initlock = info.initlock;
    // Large margin so a coin becomes near-final after ~260 mined blocks (fast, profile-independent);
    // the production default is 144. Aging (below) mines 260, so aged headroom ≈ initlock−263 < margin
    // while a fresh coin (headroom ≈ initlock) stays above it.
    let margin = initlock.saturating_sub(150);
    let age_blocks = 260u32;
    println!("SDK33 - server ladder: initlock={initlock} interval={}; auto_refresh margin={margin}", info.interval);

    // ===== (A) MAINTENANCE PASS ==================================================================
    let (alice, _) = UtexoWallet::initialize(cfg("sdk33_alice", true, margin), None).await?;
    prepay(&cc, &alice, 8).await?;

    let coin0 = deposit_confirmed_coin(&cc, &alice, "sdk33_alice", DEP).await?;
    let id0 = coin0.statechain_id.clone().ok_or_else(|| anyhow!("deposit has no id"))?;
    let l0 = coin0.locktime.ok_or_else(|| anyhow!("deposit has no backup locktime"))?;
    let headroom0 = l0 as i64 - tip(&cc)? as i64;
    assert!(
        headroom0 > margin as i64,
        "a fresh coin (headroom {headroom0}) must be ABOVE the margin {margin} — not near-final"
    );

    // A fresh coin is not refreshed: the pass is a no-op.
    let none = alice.auto_refresh_due(margin).await?;
    assert!(none.is_empty(), "a fresh coin must not be auto-refreshed (headroom {headroom0} > margin {margin})");
    println!("SDK33 - (A) fresh coin {id0}: headroom {headroom0} > margin {margin} → auto_refresh_due is a no-op");

    // Age it below the margin.
    bitcoin_core::generatetoaddress(age_blocks, &core)?;
    let headroom_aged = l0 as i64 - tip(&cc)? as i64;
    assert!(
        headroom_aged <= margin as i64,
        "after {age_blocks} blocks the coin's headroom {headroom_aged} must be at/under the margin {margin}"
    );
    println!("SDK33 - (A) +{age_blocks} blocks: coin headroom {headroom0} → {headroom_aged} ≤ margin {margin} (near-final)");

    // The maintenance pass re-anchors it (user pays the fee from the coin).
    let done = alice.auto_refresh_due(margin).await?;
    assert_eq!(done.len(), 1, "exactly the one near-final coin is refreshed");
    let r = &done[0];
    assert_eq!(r.old_statechain_id, id0, "the aged coin was the one refreshed");
    assert_ne!(r.new_statechain_id, id0, "refresh mints a NEW statechain id");
    assert_eq!(r.fee_sats, 112, "1-in-1-out P2TR at 1 sat/vB");
    assert_eq!(r.new_amount_sats, DEP as u64 - 112, "refreshed amount = deposit − fee");
    let new_id = r.new_statechain_id.clone();

    // Confirm the re-anchor + register the fresh coin.
    let mut fresh = None;
    for _ in 0..60 {
        bitcoin_core::generatetoaddress(1, &core)?;
        let _ = alice.claim().await?;
        if let Ok(c) = coin_by_id(&cc, "sdk33_alice", &new_id).await {
            if c.status == CoinStatus::CONFIRMED {
                fresh = Some(c);
                break;
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    let fresh = fresh.ok_or_else(|| anyhow!("refreshed coin did not confirm"))?;
    let l_new = fresh.locktime.ok_or_else(|| anyhow!("refreshed coin has no locktime"))?;
    let headroom_new = l_new as i64 - tip(&cc)? as i64;
    assert!(
        (headroom_new - initlock as i64).abs() <= 8,
        "refreshed coin headroom {headroom_new} ≈ initlock {initlock} (ladder reset)"
    );
    assert!(l_new > l0, "the refreshed backup locktime {l_new} is higher than the original {l0}");
    assert_eq!(
        coin_by_id(&cc, "sdk33_alice", &id0).await?.status,
        CoinStatus::WITHDRAWN,
        "the aged coin is spent on-chain by the re-anchor"
    );
    // Re-running the pass now refreshes nothing (fresh coin is back above the margin).
    assert!(
        alice.auto_refresh_due(margin).await?.is_empty(),
        "no churn: the just-refreshed coin (headroom {headroom_new}) is above the margin"
    );
    println!(
        "SDK33 - (A) MAINTENANCE PASS: aged coin re-anchored (old {id0} WITHDRAWN → fresh {new_id}, {DEP}→{} sats, fee 112); ladder reset (L {l0}→{l_new}, headroom {headroom_aged}→{headroom_new}); re-run is a no-op",
        r.new_amount_sats
    );

    // ===== (B) EMBEDDED IN transfer() ============================================================
    // Fresh wallets so exactly one aged coin exists.
    let (alice_b, _) = UtexoWallet::initialize(cfg("sdk33_carol", true, margin), None).await?;
    let (bob_b, _) = UtexoWallet::initialize(cfg("sdk33_bob", true, margin), None).await?;
    let bob_b_addr = bob_b.get_utexo_address().await?;
    prepay(&cc, &alice_b, 8).await?;

    let aged = deposit_confirmed_coin(&cc, &alice_b, "sdk33_carol", DEP).await?;
    let aged_id = aged.statechain_id.clone().ok_or_else(|| anyhow!("no id"))?;
    let aged_l = aged.locktime.ok_or_else(|| anyhow!("no locktime"))?;
    bitcoin_core::generatetoaddress(age_blocks, &core)?;
    let aged_headroom = aged_l as i64 - tip(&cc)? as i64;
    assert!(aged_headroom <= margin as i64, "alice_b's coin is aged near-final (headroom {aged_headroom})");
    println!("SDK33 - (B) alice holds one aged coin {aged_id} (headroom {aged_headroom} ≤ margin {margin}); calling transfer() with NO manual refresh");

    // Observe events during the transfer, and run the transfer while mining concurrently (the
    // transfer blocks waiting for its auto-refresh to confirm on-chain).
    let mut rx = alice_b.subscribe();
    const SEND: u64 = 30_000;
    let alice_b2 = alice_b.clone();
    let to = bob_b_addr.clone();
    let jh = tokio::spawn(async move { alice_b2.transfer(&to, SEND).await });
    let core_b = bitcoin_core::getnewaddress()?;
    let mut finished = false;
    for _ in 0..120 {
        if jh.is_finished() {
            finished = true;
            break;
        }
        bitcoin_core::generatetoaddress(1, &core_b)?;
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    assert!(finished, "the auto-refreshing transfer did not complete while mining");
    let tr = jh.await??;
    assert_eq!(tr.total_sats, SEND, "bob is sent the exact amount");

    // The transfer auto-refreshed the aged coin (event proof) before spending it.
    let refreshed_olds = drain_refresh_events(&mut rx);
    assert!(
        refreshed_olds.contains(&aged_id),
        "transfer() must have auto-refreshed the aged coin {aged_id} (got {refreshed_olds:?})"
    );
    assert_eq!(
        coin_by_id(&cc, "sdk33_carol", &aged_id).await?.status,
        CoinStatus::WITHDRAWN,
        "the aged coin was re-anchored (spent on-chain) inside the transfer"
    );

    // Bob claims his piece.
    let mut claimed = 0u32;
    for _ in 0..60 {
        claimed += bob_b.claim().await?.claimed_transfers;
        if claimed >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    assert_eq!(bob_b.get_balance().await?.available_sats, SEND, "bob received the exact amount");
    // Alice paid only the re-anchor fee (112) + the split reserve she keeps as her own coin's exit
    // margin: refreshed 49_888, minus SEND, minus reserve = her change.
    let reserve = (49_888u64 / 100).clamp(300, 2_000); // 498
    let expect_alice = 49_888 - SEND - reserve;
    assert_eq!(
        alice_b.get_balance().await?.available_sats,
        expect_alice,
        "alice keeps deposit − fee(112) − sent − split-reserve"
    );
    println!(
        "SDK33 - (B) EMBEDDED: transfer() auto-refreshed {aged_id} (fee 112), then sent {SEND} to bob — bob {SEND}, alice {expect_alice}; the coin's age was invisible, only the fee applied"
    );

    // ===== (C) OPT-OUT ===========================================================================
    let (alice_c, _) = UtexoWallet::initialize(cfg("sdk33_dave", false, margin), None).await?;
    let (bob_c, _) = UtexoWallet::initialize(cfg("sdk33_bob2", false, margin), None).await?;
    let bob_c_addr = bob_c.get_utexo_address().await?;
    prepay(&cc, &alice_c, 6).await?;

    let aged_c = deposit_confirmed_coin(&cc, &alice_c, "sdk33_dave", DEP).await?;
    let aged_c_id = aged_c.statechain_id.clone().ok_or_else(|| anyhow!("no id"))?;
    let aged_c_l = aged_c.locktime.ok_or_else(|| anyhow!("no locktime"))?;
    bitcoin_core::generatetoaddress(age_blocks, &core)?;
    assert!(aged_c_l as i64 - tip(&cc)? as i64 <= margin as i64, "alice_c's coin is aged near-final");

    let mut rx_c = alice_c.subscribe();
    let tr_c = alice_c.transfer(&bob_c_addr, 30_000).await?;
    assert_eq!(tr_c.total_sats, 30_000, "the transfer still succeeds (via a split)");
    let refreshed_c = drain_refresh_events(&mut rx_c);
    assert!(
        refreshed_c.is_empty(),
        "auto_refresh=false: the aged coin must NOT be re-anchored (got {refreshed_c:?})"
    );
    let mut got_c = 0u32;
    for _ in 0..60 {
        got_c += bob_c.claim().await?.claimed_transfers;
        if got_c >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    assert_eq!(bob_c.get_balance().await?.available_sats, 30_000, "bob_c received the amount");
    println!("SDK33 - (C) OPT-OUT: auto_refresh=false → transfer did NOT re-anchor {aged_c_id} (no CoinRefreshed event); opt-out respected");

    println!(
        "SDK33 - SUCCESS: auto-refresh re-anchors a coin nearing its ladder floor automatically. (A) the maintenance pass leaves fresh coins untouched and refreshes only near-final ones (fee from the coin), with no churn on re-run; (B) transfer() refreshes an aged coin transparently before spending it — bob got the exact amount and only the {}-sat fee applied; (C) auto_refresh=false opts out. The user never has to think about the ladder floor.",
        112
    );
    Ok(())
}
