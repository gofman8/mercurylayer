//! E2E (granularity, plain sats): partial-amount payments over statechain coins. The SDK makes
//! arbitrary sat amounts exact through TWO mechanisms — an exact-subset plan (whole-coin
//! handovers, no split) and an off-chain split (one SE-co-signed, un-broadcast tx minting the
//! exact piece + change) — with client-side dust/fee guards refusing anything unrepresentable.
//!
//! (a) EXACT SUBSET: alice deposits 30k+20k+50k and pays bob exactly 50k TWICE. The first is a
//!     single-coin match (1 handover, ONE coin at bob); the second is the {30k,20k} subset
//!     (2 handovers, TWO coins at bob). NO split happens in either: `used_split == false`, alice's
//!     record still has exactly the 3 deposit coins (no sub-coin rows were minted).
//! (b) EXACT SPLIT: alice deposits 100_000 and pays 12_345 => WithSplit. bob books exactly
//!     12_345; alice's change is 100_000 − 12_345 − reserve(1_000) = 86_655; the consumed parent
//!     is PUBLICLY terminal at the SE (REQ-18, GET spend_budget). The split tx vsize is measured
//!     from the stored branch row and printed as an ECON line.
//! (c) BOUNDARY: a fresh wallet (dan) holds a single 5_000-sat coin and attempts to pay 4_800.
//!     piece(4_800) + reserve(300) + dust-floor change(330) does not fit in 5_000 (change would be
//!     −100), so the planner refuses with the TYPED `SdkError::InsufficientBalance` BEFORE touching
//!     the coin: the coin stays CONFIRMED and non-terminal (spend budget unset). Then the TRUE
//!     minimum piece: the 330-sat split-output dust floor is necessary but NOT sufficient — the
//!     piece's own backup tx (112 vB) must sweep above dust after its fee, so the minimum MINTABLE
//!     piece is 330 + ceil(112·fee_rate) (≈442 at 1 sat/vB), not 330. A 330-sat piece is refused
//!     (its backup is FeeTooLow); a piece at the true floor mints cleanly. (Known limitation: the
//!     current guard discovers the 330 shortfall only after co-signing the split, stranding the
//!     parent terminal — see the split-guard fix task; this test accepts either the current error
//!     or the post-fix clean refusal, and proves the true floor on a fresh coin.)
//! (d) RE-SPLIT DEPTH: bob re-splits his received 12_345 piece to pay carol 2_345 — carol's coin
//!     is a depth-2 sub-coin (2-tx exit branch), `estimate_exit_cost().branch_txs == 2`, and each
//!     branch level costs ~155 vB (one 1-in-2-out split tx, band 100..=250 like sdk26).
//!
//! Run: SDK_E2E=28 ML_NETWORK=regtest cargo run
//! Cross-refs: SPEC.md REQ-15/17/18, INV-9/10; docs/utexo/learn/transfers.md (amount maker);
//! sdk26 (depth/width economics baseline).

use std::time::Duration;

use anyhow::{anyhow, Result};
use mercury_utexo_sdk::{SdkConfig, SdkError, UtexoWallet};
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

/// Poll claim until `n` incoming transfers have been claimed in total.
async fn claim_n(w: &UtexoWallet, n: u32) -> Result<()> {
    let mut got = 0u32;
    for _ in 0..60 {
        got += w.claim().await?.claimed_transfers;
        if got >= n {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Err(anyhow!("expected {n} claimed transfers, got {got}"))
}

/// The wallet's CONFIRMED coin of exactly `sats` (most recent), with its statechain id.
async fn confirmed_coin(
    cc: &ClientConfig,
    wallet_name: &str,
    sats: u32,
) -> Result<mercuryrustlib::Coin> {
    let rec = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, wallet_name).await?;
    rec.coins
        .iter()
        .rev()
        .find(|c| c.status == CoinStatus::CONFIRMED && c.amount == Some(sats) && c.duplicate_index == 0)
        .cloned()
        .ok_or_else(|| anyhow!("{wallet_name} has no CONFIRMED coin of {sats} sats"))
}

/// vsizes of the stored exit-branch txs of `statechain_id` (root-first).
async fn branch_vsizes(
    cc: &ClientConfig,
    wallet_name: &str,
    statechain_id: &str,
) -> Result<Vec<u64>> {
    let rows = mercuryrustlib::sqlite_manager::get_backup_txs(
        &cc.pool,
        wallet_name,
        &format!("branch-{statechain_id}"),
    )
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let tx: electrum_client::bitcoin::Transaction =
            electrum_client::bitcoin::consensus::deserialize(&hex::decode(&row.tx)?)?;
        out.push(tx.vsize() as u64);
    }
    Ok(out)
}

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    // Plain-BTC test, but SdkConfig::regtest defaults rgb_data_dir, so the engines open lazily
    // for the carrier-exclusion checks — start them from clean dirs.
    for d in [
        "./rgb-data-sdk28_alice",
        "./rgb-data-sdk28_bob",
        "./rgb-data-sdk28_dan",
        "./rgb-data-sdk28_carol",
    ] {
        let _ = std::fs::remove_dir_all(d);
    }
    // V1-LANE TEST: exercises split-based sats granularity (V1 branch sub-coins). V2 splits only
    // IN-LADDER (exit-only children with different tier-fee amounts). Pin V1 until V1 is deleted.
    std::env::set_var("UTEXO_PROTOCOL_DEFAULT", "1");
    let cc = mercuryrustlib::client_config::load().await;

    let (alice, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk28_alice"), None).await?;
    let (bob, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk28_bob"), None).await?;
    let (dan, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk28_dan"), None).await?;
    let (carol, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk28_carol"), None).await?;
    let bob_addr = bob.get_utexo_address().await?;
    let carol_addr = carol.get_utexo_address().await?;

    // ===== (a) EXACT SUBSET: 30k + 20k + 50k, pay 50k twice — no split either time ==============
    deposit_confirmed_coin(&cc, &alice, "sdk28_alice", 30_000).await?;
    deposit_confirmed_coin(&cc, &alice, "sdk28_alice", 20_000).await?;
    deposit_confirmed_coin(&cc, &alice, "sdk28_alice", 50_000).await?;
    println!("SDK28 - alice deposited 30k + 20k + 50k (three separate coins)");

    // First 50k: single-coin exact match — ONE handover, bob gets ONE coin.
    let r1 = alice.transfer(&bob_addr, 50_000).await?;
    assert!(!r1.used_split, "single-coin exact match must not split");
    assert_eq!(r1.coins.len(), 1, "exact single-coin match = ONE handover");
    assert_eq!(r1.coins[0].amount_sats, 50_000);
    claim_n(&bob, 1).await?;

    // Second 50k: only {30k, 20k} remain — exact SUBSET, TWO handovers, bob gets TWO coins.
    let r2 = alice.transfer(&bob_addr, 50_000).await?;
    assert!(!r2.used_split, "exact-subset match must not split");
    assert_eq!(r2.coins.len(), 2, "30k+20k subset = TWO handovers");
    let mut amounts: Vec<u64> = r2.coins.iter().map(|c| c.amount_sats).collect();
    amounts.sort();
    assert_eq!(amounts, vec![20_000, 30_000], "subset coins are exactly {{20k, 30k}}");
    claim_n(&bob, 2).await?;

    assert_eq!(
        bob.get_balance().await?.available_sats,
        100_000,
        "bob holds both 50k payments"
    );
    let bob_rec = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk28_bob").await?;
    let mut bob_amounts: Vec<u32> = bob_rec
        .coins
        .iter()
        .filter(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0)
        .filter_map(|c| c.amount)
        .collect();
    bob_amounts.sort();
    assert_eq!(bob_amounts, vec![20_000, 30_000, 50_000], "bob's coins are the ORIGINAL coins");

    // NO split happened: alice's record still holds exactly the 3 deposit coin rows (a split
    // would have ADDED piece+change sub-coin rows), none of them still spendable.
    let _ = alice.claim().await?; // refresh statuses after bob's claims
    let alice_rec = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk28_alice").await?;
    let alice_coins: Vec<_> =
        alice_rec.coins.iter().filter(|c| c.duplicate_index == 0).collect();
    assert_eq!(alice_coins.len(), 3, "no new sub-coins were minted at alice");
    assert!(
        !alice_coins.iter().any(|c| c.status == CoinStatus::CONFIRMED),
        "all three deposit coins were handed over whole"
    );
    assert_eq!(alice.get_balance().await?.available_sats, 0);
    println!(
        "SDK28 - EXACT SUBSET: 50k paid twice with ZERO splits (1-coin match, then 30k+20k subset); bob holds 3 original coins"
    );

    // ===== (b) EXACT SPLIT: 100_000 pays 12_345 → piece 12_345 + change 86_655 ==================
    let dep = deposit_confirmed_coin(&cc, &alice, "sdk28_alice", 100_000).await?;
    let parent_id = dep
        .statechain_id
        .clone()
        .ok_or_else(|| anyhow!("100k deposit has no statechain id"))?;
    add_tokens(&cc, &alice, 2).await?; // two fresh statechain slots for {piece, change}
    let r3 = alice.transfer(&bob_addr, 12_345).await?;
    assert!(r3.used_split, "12_345 from a 100k coin requires an off-chain split");
    assert_eq!(r3.coins.len(), 1);
    assert_eq!(r3.coins[0].amount_sats, 12_345, "the minted piece is the EXACT amount");
    claim_n(&bob, 1).await?;

    // bob books exactly 12_345 (a distinct coin, not an approximation).
    let bob_piece = confirmed_coin(&cc, "sdk28_bob", 12_345).await?;
    let bob_piece_id = bob_piece.statechain_id.clone().unwrap_or_default();
    assert_eq!(
        bob_piece_id, r3.coins[0].statechain_id,
        "bob's booked coin is the transferred piece"
    );
    assert_eq!(bob.get_balance().await?.available_sats, 112_345);

    // alice's change = parent − piece − reserve = 100_000 − 12_345 − 1_000 = 86_655 (INV-10:
    // reserve = (100_000/100).clamp(300, 2000) = 1_000).
    let change = confirmed_coin(&cc, "sdk28_alice", 86_655).await?;
    let change_id = change.statechain_id.clone().unwrap_or_default();

    // The consumed parent is PUBLICLY terminal at the SE (REQ-18/INV-24).
    let (budget, finalized, terminal) =
        mercuryrustlib::lightning_latch::get_spend_budget(&cc, &parent_id).await?;
    assert!(
        terminal,
        "split parent must be terminal at the SE (budget {budget:?}, finalized {finalized})"
    );

    // Measure the split tx from the stored branch row (change side; the piece went to bob).
    let vbs = branch_vsizes(&cc, "sdk28_alice", &change_id).await?;
    assert_eq!(vbs.len(), 1, "depth-1 change carries a 1-tx exit branch");
    assert!(
        (100..=250).contains(&vbs[0]),
        "plain split tx ~155 vB (got {} vB, band 100..=250)",
        vbs[0]
    );
    println!(
        "ECON split parent=100000 piece=12345 change=86655 reserve=1000 split_vb={}",
        vbs[0]
    );
    println!("SDK28 - EXACT SPLIT: bob booked exactly 12_345; alice keeps 86_655 change; parent terminal at the SE");

    // ===== (c) BOUNDARY: 5_000-sat coin refuses 4_800, then pays the 330 minimum ================
    let dan_dep = deposit_confirmed_coin(&cc, &dan, "sdk28_dan", 5_000).await?;
    let dan_coin_id = dan_dep
        .statechain_id
        .clone()
        .ok_or_else(|| anyhow!("dan's deposit has no statechain id"))?;

    // 4_800 + reserve(300) + dust-floor change(330) = 5_430 > 5_000 (change would be −100):
    // the planner refuses with the TYPED error before any split state is created (audit [29]).
    let err = dan
        .transfer(&bob_addr, 4_800)
        .await
        .expect_err("paying 4_800 from a single 5_000 coin must be refused (sub-dust change)");
    match err.downcast_ref::<SdkError>() {
        Some(SdkError::InsufficientBalance { requested_sats, available_sats }) => {
            assert_eq!(*requested_sats, 4_800);
            assert_eq!(*available_sats, 5_000);
        }
        _ => return Err(anyhow!("expected typed SdkError::InsufficientBalance, got: {err:#}")),
    }
    println!("SDK28 - BOUNDARY refusal (typed): {err:#}");

    // The coin is UNTOUCHED: still CONFIRMED/spendable, and the SE never saw a budget guard —
    // GET spend_budget (public) shows budget unset and non-terminal.
    let untouched = confirmed_coin(&cc, "sdk28_dan", 5_000).await?;
    assert_eq!(untouched.statechain_id.as_deref(), Some(dan_coin_id.as_str()));
    let (budget, _fin, terminal) =
        mercuryrustlib::lightning_latch::get_spend_budget(&cc, &dan_coin_id).await?;
    assert!(budget.is_none(), "refused split must not have set a spend budget");
    assert!(!terminal, "refused split must leave the coin non-terminal at the SE");

    // The 330-sat split-output dust floor is NOT the minimum mintable piece: a 330-sat sub-coin's
    // own backup (112 vB) sweeps 330 − ceil(112·fee_rate) < 330 = dust, so create_tx1 rejects it
    // (FeeTooLow). Probe on a THROWAWAY coin (the current guard checks this only AFTER co-signing
    // the split, so the probe consumes that coin — hence a fresh one). Accept any error: today it
    // is FeeTooLow post-split; after the split-guard fix it becomes a clean pre-terminal refusal.
    let (eve, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk28_eve"), None).await?;
    let _ = deposit_confirmed_coin(&cc, &eve, "sdk28_eve", 5_000).await?;
    add_tokens(&cc, &eve, 2).await?;
    let sub_dust = eve.transfer(&carol_addr, 330).await;
    assert!(
        sub_dust.is_err(),
        "a 330-sat piece is NOT mintable — its backup would be dust after fee (FeeTooLow), so the split must fail"
    );
    println!("SDK28 - a 330-sat piece is refused (backup would be sub-dust after fee): {sub_dust:?}");

    // The TRUE minimum mintable piece = 330 + backup fee. Compute the backup fee the way create_tx1
    // does (min(SE-quoted rate, client max_fee_rate); BACKUP_TX_SIZE = 112 vB) and mint exactly at
    // the floor from dan's untouched 5_000 coin.
    let info = mercuryrustlib::utils::info_config(&cc).await?;
    let fee_rate = info.fee_rate_sats_per_byte.min(cc.max_fee_rate);
    let backup_fee = (112.0f64 * fee_rate).ceil() as u64;
    let min_piece = 330 + backup_fee; // ≈ 442 at 1 sat/vB
    add_tokens(&cc, &dan, 2).await?;
    let r4 = dan.transfer(&carol_addr, min_piece).await?;
    assert!(r4.used_split);
    assert_eq!(
        r4.coins[0].amount_sats, min_piece,
        "the true minimum piece (330 + backup fee) mints exactly"
    );
    claim_n(&carol, 1).await?;
    assert_eq!(carol.get_balance().await?.available_sats, min_piece);
    let dan_change_sats: u32 = (5_000 - min_piece - 300) as u32; // reserve at 5_000 = 300
    let dan_change = confirmed_coin(&cc, "sdk28_dan", dan_change_sats).await?;
    let (_, _, dan_terminal) =
        mercuryrustlib::lightning_latch::get_spend_budget(&cc, &dan_coin_id).await?;
    assert!(dan_terminal, "the min-piece split consumed the 5_000 parent (now terminal)");
    println!(
        "ECON boundary parent=5000 refused_piece=4800 dust_floor=330 backup_fee={backup_fee} min_mintable_piece={min_piece} change={dan_change_sats}"
    );
    println!(
        "SDK28 - BOUNDARY: 4_800 refused (coin untouched); 330 refused (backup sub-dust); true min piece {min_piece} sats paid, dan keeps {} change",
        dan_change.amount.unwrap_or_default()
    );

    // ===== (d) RE-SPLIT DEPTH: bob re-splits his 12_345 piece → carol at depth 2 ================
    add_tokens(&cc, &bob, 2).await?;
    let r5 = bob.transfer(&carol_addr, 2_345).await?;
    assert!(r5.used_split, "2_345 requires splitting bob's smallest sufficient coin (12_345)");
    assert_eq!(r5.coins.len(), 1);
    assert_eq!(r5.coins[0].amount_sats, 2_345);
    let carol_piece_id = r5.coins[0].statechain_id.clone();
    claim_n(&carol, 1).await?;

    let carol_piece = confirmed_coin(&cc, "sdk28_carol", 2_345).await?;
    assert_eq!(carol_piece.statechain_id.as_deref(), Some(carol_piece_id.as_str()));

    // bob's received piece was the re-split parent: consumed + publicly terminal; his change is
    // 12_345 − 2_345 − reserve(300) = 9_700.
    let (_, _, piece_terminal) =
        mercuryrustlib::lightning_latch::get_spend_budget(&cc, &bob_piece_id).await?;
    assert!(piece_terminal, "bob's 12_345 piece is terminal after the re-split");
    confirmed_coin(&cc, "sdk28_bob", 9_700).await?;

    // carol's coin is a DEPTH-2 sub-coin: 2-tx branch, each level one ~155 vB split tx.
    let est = carol.estimate_exit_cost(&carol_piece_id).await?;
    assert_eq!(est.branch_txs, 2, "depth-2 piece exits through 2 branch txs");
    let level_vbs = branch_vsizes(&cc, "sdk28_carol", &carol_piece_id).await?;
    assert_eq!(level_vbs.len(), 2);
    for (i, vb) in level_vbs.iter().enumerate() {
        assert!(
            (100..=250).contains(vb),
            "branch level {} out of band: {} vB (expected ~155, band 100..=250)",
            i + 1,
            vb
        );
    }
    assert_eq!(
        level_vbs.iter().sum::<u64>(),
        est.branch_vbytes,
        "estimate_exit_cost measures exactly the stored branch txs (INV-17)"
    );
    println!(
        "ECON resplit parent=12345 piece=2345 change=9700 reserve=300 depth=2 branch_txs={} branch_vb={} per_level_vb={:?} backup_vb={} total_vb={}",
        est.branch_txs, est.branch_vbytes, level_vbs, est.backup_vbytes, est.total_vbytes
    );
    println!("SDK28 - RE-SPLIT DEPTH: carol's 2_345 piece sits at depth 2 (~155 vB per level)");

    println!(
        "SDK28 - SUCCESS: partial sats amounts are exact at 1-sat resolution — exact subsets hand over whole coins (zero splits), an off-chain split mints 12_345 exactly (change 86_655, parent publicly terminal), the 330 dust floor + fee reserve boundary is refused with a TYPED error leaving the coin untouched, and a received piece re-splits to depth 2 at ~155 vB per level."
    );
    Ok(())
}
