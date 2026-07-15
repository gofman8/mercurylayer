//! E2E (refresh / re-anchor): reset a coin's backup ladder + root deadline with ONE on-chain tx.
//!
//! A statechain coin's decrementing-locktime ladder is a finite budget (`initlock` blocks of
//! headroom, spent by both hops and wall-clock time). `refresh` re-anchors the coin on-chain into a
//! FRESH aggregate: the old outpoint is spent (so every old backup is permanently dead), and the
//! new coin gets a fresh deposit height ⇒ a fresh full ladder + fresh root deadline. This proves
//! the **user-pays** mode (fee deducted from the coin).
//!
//! (a) HORIZON RESET: alice deposits 50k (backup at L0 ≈ tip + initlock). Mine 300 blocks so the
//!     tip rises and the coin's remaining headroom (L0 − tip) shrinks by 300. `refresh` re-anchors
//!     it: the new coin's backup L_new ≈ new_tip + initlock — headroom restored to ~initlock, i.e.
//!     L_new > L0 (the horizon moved FORWARD past the original). The old coin is WITHDRAWN (its
//!     funding outpoint spent by the refresh tx, confirmed on-chain); the new statechain_id differs;
//!     the new amount is 50k − fee (fee = 112 sats at 1 sat/vB).
//! (b) STILL SPENDABLE: alice transfers the refreshed coin to bob and bob claims it — the fresh
//!     coin is an ordinary, transferable coin with a full ladder (balances 0 / new_amount).
//! (c) OPERATOR PAYS (off-chain rebate): a fresh coin is refreshed via `refresh_sponsored`, where a
//!     funded operator wallet reimburses the fee OFF-CHAIN. The user's TOTAL balance is preserved
//!     (refreshed `amount − fee` + a `fee` rebate coin = the original amount); the operator bore the
//!     on-chain cost. The blind SE holds no funds, so "operator pays" is realized as this rebate.
//!
//! Run: SDK_E2E=30 ML_NETWORK=regtest cargo run

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

/// Deposit `amount` to `w`, mine, and poll claim until the wallet holds the CONFIRMED coin.
async fn deposit_confirmed_coin(
    cc: &ClientConfig,
    w: &UtexoWallet,
    wallet_name: &str,
    amount: u32,
) -> Result<mercuryrustlib::Coin> {
    let t = prepaid_token(cc).await?;
    w.add_prepaid_token(&t).await;
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

/// The wallet's coin with a given statechain id (any status).
async fn coin_by_id(
    cc: &ClientConfig,
    wallet_name: &str,
    id: &str,
) -> Result<mercuryrustlib::Coin> {
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

pub async fn execute() -> Result<()> {
    // V2DEF-5: this test exercises V1-specific mechanisms (decrementing-locktime stale-state /
    // invalidation / re-anchor) that V2 replaces for roots but that remain for V1 split sub-coins &
    // the LN lane. Pin V1 so it stays a V1-lane regression test under the V2 default.
    std::env::set_var("UTEXO_PROTOCOL_DEFAULT", "1");
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    for d in [
        "./rgb-data-sdk30_alice",
        "./rgb-data-sdk30_bob",
        "./rgb-data-sdk30_carol",
        "./rgb-data-sdk30_operator",
    ] {
        let _ = std::fs::remove_dir_all(d);
    }
    let cc = mercuryrustlib::client_config::load().await;
    let core = bitcoin_core::getnewaddress()?;

    let (alice, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk30_alice"), None).await?;
    let (bob, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk30_bob"), None).await?;
    let bob_addr = bob.get_utexo_address().await?;

    let info = mercuryrustlib::utils::info_config(&cc).await?;
    let initlock = info.initlock;
    println!("SDK30 - server ladder: initlock={initlock} interval={}", info.interval);

    // ===== (a) HORIZON RESET =====================================================================
    let coin0 = deposit_confirmed_coin(&cc, &alice, "sdk30_alice", 50_000).await?;
    let id0 = coin0.statechain_id.clone().ok_or_else(|| anyhow!("deposit has no id"))?;
    let l0 = coin0.locktime.ok_or_else(|| anyhow!("deposit coin has no backup locktime"))?;
    let tip0 = tip(&cc)?;
    let headroom0 = l0 as i64 - tip0 as i64;
    println!("SDK30 - deposited 50k: coin {id0}, backup L0={l0}, tip={tip0}, headroom={headroom0} (~initlock)");
    assert!(
        (headroom0 - initlock as i64).abs() <= 6,
        "a fresh deposit's headroom must be ~initlock (L0={l0}, tip={tip0}, headroom={headroom0})"
    );

    // Simulate time passing: mine 300 blocks. The tip rises, eating the coin's headroom from below.
    bitcoin_core::generatetoaddress(300, &core)?;
    let tip_aged = tip(&cc)?;
    let headroom_aged = l0 as i64 - tip_aged as i64;
    println!(
        "SDK30 - +300 blocks: tip={tip_aged}, the SAME coin's headroom shrank to {headroom_aged} (L0 unchanged at {l0})"
    );
    assert!(
        headroom_aged < headroom0 - 250,
        "300 blocks must consume ~300 of the coin's headroom ({headroom0} -> {headroom_aged})"
    );

    // Re-anchor it (user pays the fee from the coin).
    let res = alice.refresh(&id0, None).await?;
    assert_ne!(res.new_statechain_id, id0, "refresh mints a NEW statechain id");
    assert_eq!(res.old_amount_sats, 50_000);
    assert_eq!(res.fee_sats, 112, "1-in-1-out P2TR at 1 sat/vB = 112 sats");
    assert_eq!(res.new_amount_sats, 50_000 - 112, "refreshed amount = old − fee");
    println!(
        "SDK30 - refresh broadcast: old {id0} -> new {} (amount {} -> {}, fee {}), txid {}",
        res.new_statechain_id, res.old_amount_sats, res.new_amount_sats, res.fee_sats, res.refresh_txid
    );

    // Confirm the re-anchor tx + let the watcher register the fresh coin.
    let mut new_coin = None;
    for _ in 0..60 {
        bitcoin_core::generatetoaddress(1, &core)?;
        let _ = alice.claim().await?;
        let c = coin_by_id(&cc, "sdk30_alice", &res.new_statechain_id).await;
        if let Ok(c) = c {
            if c.status == CoinStatus::CONFIRMED {
                new_coin = Some(c);
                break;
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    let new_coin = new_coin.ok_or_else(|| anyhow!("refreshed coin did not confirm"))?;
    let l_new = new_coin.locktime.ok_or_else(|| anyhow!("refreshed coin has no backup locktime"))?;
    let tip_new = tip(&cc)?;
    let headroom_new = l_new as i64 - tip_new as i64;

    // The ladder is RESET: the new backup sits ~initlock above the (now higher) tip, and its
    // absolute locktime is HIGHER than the original — the horizon moved forward past L0.
    assert!(
        (headroom_new - initlock as i64).abs() <= 8,
        "refreshed coin's headroom must be ~initlock again (L_new={l_new}, tip={tip_new}, headroom={headroom_new})"
    );
    assert!(headroom_new > headroom_aged, "refresh restored headroom ({headroom_aged} -> {headroom_new})");
    assert!(l_new > l0, "the refreshed backup locktime is HIGHER than the original (horizon extended)");

    // The old coin is spent on-chain (WITHDRAWN); the refresh tx is confirmed and spends coin0's
    // funding outpoint — every old backup is now a double-spend of a spent input (dead).
    let old = coin_by_id(&cc, "sdk30_alice", &id0).await?;
    assert_eq!(old.status, CoinStatus::WITHDRAWN, "the old coin is spent on-chain");
    let rtxid = res.refresh_txid.parse::<electrum_client::bitcoin::Txid>()?;
    let rtx = cc.electrum_client.transaction_get(&rtxid)?;
    let spends_old = rtx.input.iter().any(|i| {
        i.previous_output.txid.to_string() == old.utxo_txid.clone().unwrap_or_default()
            && Some(i.previous_output.vout) == old.utxo_vout
    });
    assert!(spends_old, "the refresh tx must spend the OLD coin's funding outpoint");
    println!(
        "ECON refresh old_amount=50000 new_amount={} fee=112 refresh_vb={} L0={l0} L_new={l_new} headroom_aged={headroom_aged} headroom_new={headroom_new}",
        res.new_amount_sats,
        rtx.vsize()
    );
    println!(
        "SDK30 - HORIZON RESET: old coin spent (WITHDRAWN, outpoint gone → all old backups dead); fresh coin at a fresh ladder (L {l0} → {l_new}, headroom {headroom_aged} → {headroom_new} ≈ initlock)"
    );

    // ===== (b) STILL SPENDABLE ===================================================================
    let new_amount = res.new_amount_sats;
    let r = alice.transfer(&bob_addr, new_amount).await?;
    assert_eq!(r.total_sats, new_amount, "the whole refreshed coin transfers");
    let mut claimed = 0u32;
    for _ in 0..60 {
        claimed += bob.claim().await?.claimed_transfers;
        if claimed >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    assert_eq!(bob.get_balance().await?.available_sats, new_amount, "bob received the refreshed coin");
    assert_eq!(alice.get_balance().await?.available_sats, 0, "alice spent the refreshed coin");
    println!("SDK30 - STILL SPENDABLE: refreshed coin transferred to bob ({new_amount} sats), balances 0 / {new_amount}");

    // ===== (c) OPERATOR PAYS (off-chain rebate) ==================================================
    // carol is the user; a funded operator wallet reimburses the on-chain fee off-chain.
    let (carol, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk30_carol"), None).await?;
    let (operator, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk30_operator"), None).await?;
    let c0 = deposit_confirmed_coin(&cc, &carol, "sdk30_carol", 40_000).await?;
    let cid = c0.statechain_id.clone().ok_or_else(|| anyhow!("carol deposit has no id"))?;
    // Fund the operator so it can rebate off-chain.
    let _ = deposit_confirmed_coin(&cc, &operator, "sdk30_operator", 10_000).await?;

    let sp = carol.refresh_sponsored(&cid, &operator, None).await?;
    assert_eq!(sp.fee_sats, 112);
    assert_eq!(sp.new_amount_sats, 40_000 - 112);
    // The sub-dust 112-sat fee can't be rebated exactly off-chain (min off-chain piece = dust 330 +
    // backup fee 112 = 442), so the operator rebates fee + dust = 442, over-covering by 330.
    assert_eq!(sp.rebate_sats, 442, "operator rebates the smallest off-chain-payable amount ≥ fee");
    // Confirm the re-anchor + claim the fresh coin AND the incoming rebate.
    let expected_total = 40_000u64 - 112 + sp.rebate_sats; // 39_888 + 442 = 40_330
    for _ in 0..60 {
        bitcoin_core::generatetoaddress(1, &core)?;
        let _ = carol.claim().await?;
        let reanchored = coin_by_id(&cc, "sdk30_carol", &sp.new_statechain_id)
            .await
            .map(|c| c.status == CoinStatus::CONFIRMED)
            .unwrap_or(false);
        if reanchored && carol.get_balance().await?.available_sats == expected_total {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    // The user is made MORE than whole: refreshed (40_000 − 112) + rebate (442) = 40_330 ≥ 40_000.
    let carol_total = carol.get_balance().await?.available_sats;
    assert_eq!(carol_total, expected_total, "refreshed + rebate");
    assert!(carol_total >= 40_000, "operator-paid refresh leaves the user at least whole");
    // The operator bore the on-chain cost: it sent a 442-sat rebate piece (split from its 10_000
    // coin, reserve 300), so its spendable change is 10_000 − 442 − 300 = 9_258.
    assert_eq!(
        operator.get_balance().await?.available_sats,
        10_000 - 442 - 300,
        "the operator paid the rebate (442) + split reserve (300) out of its own funds"
    );
    println!(
        "ECON refresh_sponsored old_amount=40000 new_amount={} fee=112 rebate={} carol_total={carol_total} operator_paid={}",
        sp.new_amount_sats, sp.rebate_sats, 10_000 - (10_000 - 442 - 300)
    );
    println!("SDK30 - OPERATOR PAYS: carol refreshed; operator rebated the fee off-chain (442 = fee 112 + dust 330, the smallest off-chain-payable amount) — carol total {carol_total} ≥ 40_000, operator bore the cost");

    println!(
        "SDK30 - SUCCESS: refresh re-anchors a coin in ONE on-chain tx — the old outpoint is spent (all previous backups permanently invalidated), the new coin gets a fresh full ladder and root deadline (headroom restored to ~initlock, locktime moved forward from {l0} to {l_new}), and the refreshed coin is an ordinary spendable coin. TWO fee models proven: (1) USER PAYS — the fee comes from the coin (50000 → {}); (2) OPERATOR PAYS — a funded sponsor rebates the fee off-chain (442 = fee 112 + dust 330, the smallest off-chain-payable amount) so the user ends ≥ whole (40000 → {carol_total}); the operator bore the cost. The blind SE never touches funds in either.",
        res.new_amount_sats
    );
    Ok(())
}
