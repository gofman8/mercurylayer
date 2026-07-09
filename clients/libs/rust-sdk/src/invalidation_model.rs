//! Executable model of the old-state invalidation mechanism — the executable companion of
//! `docs/utexo/INVALIDATION-SPEC.md`.
//!
//! Pure logic only: no network, no DB, no running stack. Wherever a callable pure function
//! exists these tests call the REAL one:
//!
//! - `mercurylib::transaction::calculate_block_height` (lib/src/transaction.rs:148-163) — the
//!   backup-tx locktime ladder,
//! - [`crate::transfer::split_fee_reserve`] — the split miner-fee reserve clamp,
//! - [`crate::transfer::split_amounts`] — the split executor's admission guard (fit + dust),
//! - [`crate::select::plan`] — payment planning / split-floor refusal,
//! - [`crate::wallet::deposit_anchored_deadline`] — the pure formula inside the async/electrum
//!   `UtexoWallet::deposit_anchored_exit_deadline`,
//! - [`crate::types::ExitCostEstimate::fee_sats_at`] and [`crate::types::is_terminal`].
//!
//! Only quantities with no callable pure implementation are modelled here, each with a citation
//! to the production code it mirrors: wall-clock arithmetic (blocks-per-day) and the measured
//! branch/backup vsizes behind the synthetic exit-cost estimates. Where a modelled value and the
//! real ladder can be cross-checked, the test computes the ladder side with the real function.
//!
//! Ladder caller convention (mirrored from production by [`ladder_locktime`]):
//! `calculate_block_height` zeroes `initlock` for `qt > 0`, so callers pass
//! - `qt = 0` (deposit backup, tx1): the CURRENT co-sign tip `h`
//!   (clients/libs/rust/src/deposit.rs, `create_tx1` → `new_transaction` with height `None`),
//!   yielding `L_0 = h + initlock`;
//! - `qt = k ≥ 1` (hop-k backup): tx1's LOCKTIME `L_0`
//!   (clients/libs/rust/src/transfer_sender.rs:314-339, `get_blockheight(bkp_tx1)`),
//!   yielding `L_k = L_0 − k·interval`.

use mercurylib::transaction::calculate_block_height;

use crate::select::{plan, Candidate, Plan};
use crate::transfer::{split_amounts, split_fee_reserve, DUST_LIMIT};
use crate::types::{is_terminal, ExitCostEstimate};
use crate::wallet::deposit_anchored_deadline;

/// Deployed profile (server/Settings.toml:2-3): `initlock = 1000`, `interval = 10`.
const DEPLOYED: (u32, u32) = (1_000, 10);
/// Config defaults (server/src/server_config.rs:69-70): `initlock = 10000`, `interval = 100`.
const DEFAULTS: (u32, u32) = (10_000, 100);
const PROFILES: [(u32, u32); 2] = [DEPLOYED, DEFAULTS];

/// Representative co-sign tip (deposit confirmation height) for the ladder tests.
const H: u32 = 850_000;
/// Expected block cadence used by all wall-clock reasoning in the docs.
const BLOCKS_PER_DAY: u32 = 144;

/// Backup locktime after `qt` hops, computed with the REAL production function under the
/// production caller convention (see module docs). `qt = 0` is the deposit backup tx1.
fn ladder_locktime(h_cosign: u32, initlock: u32, interval: u32, qt: u32) -> u32 {
    let l0 = calculate_block_height(h_cosign, initlock, interval, 0, false)
        .expect("qt=0 never errors");
    if qt == 0 {
        l0
    } else {
        calculate_block_height(l0, initlock, interval, qt, false).expect("qt>0 never errors")
    }
}

/// Number of hops until the ladder floor reaches the co-sign anchor.
fn ladder_capacity(initlock: u32, interval: u32) -> u32 {
    initlock / interval
}

// SPEC INV-5 / IVL ladder law: tx1 locks at h + initlock and every transfer hands the new owner
// a backup maturing EXACTLY one `interval` earlier, so the current owner always holds the
// strictly lowest locktime — the first broadcastable claim once the tip reaches it.
#[test]
fn ladder_decrements_exactly_interval_per_hop() {
    for &(initlock, interval) in &PROFILES {
        let ladder: Vec<u32> = (0..=10)
            .map(|qt| ladder_locktime(H, initlock, interval, qt))
            .collect();

        // First rung: the deposit backup locks initlock blocks above the co-sign tip.
        assert_eq!(ladder[0], H + initlock, "tx1 locktime = h + initlock ({initlock}/{interval})");

        // Every hop: exactly one interval lower, hence strictly monotone decreasing.
        for k in 1..ladder.len() {
            assert_eq!(
                ladder[k - 1] - ladder[k],
                interval,
                "hop {k} must decrement exactly one interval ({initlock}/{interval})"
            );
            assert!(ladder[k] < ladder[k - 1], "ladder must be strictly monotone");
        }

        // INV-5: the current owner (qt = 10) holds the strictly lowest locktime.
        let current = *ladder.last().unwrap();
        assert_eq!(current, H + initlock - 10 * interval);
        assert!(
            ladder[..ladder.len() - 1].iter().all(|&stale| current < stale),
            "current owner's locktime strictly below every stale owner's"
        );
    }
}

// Ladder capacity = initlock / interval = 100 hops in BOTH profiles; hop 100 lands exactly on
// the co-sign anchor h.
//
// TRUE behaviour beyond capacity (verified against lib/src/transaction.rs:148-163):
// `calculate_block_height` has NO floor guard. For qt = 101 it silently returns
// `h − interval` — a locktime BELOW the co-sign anchor, i.e. (once the tip passes h) an
// immediately-broadcastable backup, degrading the ladder to a plain first-seen race. Nothing
// sender-side bounds qt and the blind SE cannot see locktimes; the operative guard is
// RECEIVER-side: lib/src/transfer/receiver.rs:461 rejects any backup with
// locktime ≤ current tip (and :465 rejects locktime > tip + initlock), so an honest receiver
// refuses a beyond-capacity ladder. Latent footgun: the arithmetic itself only fails when
// interval·qt exceeds h + initlock, and then by u32 UNDERFLOW — a panic under overflow checks
// (dev/test default), a silent wrap in release; the wrapped value (≈ u32::MAX) would then be
// rejected downstream by `bitcoin::absolute::LockTime::from_height` (> 500_000_000 threshold,
// lib/src/transaction.rs:232). There is no explicit error path for it. The workspace sets no
// [profile] override, so the profile defaults apply; the test below asserts the documented
// behaviour of whichever profile it is BUILT under, so it stays green under
// `cargo test --release` too.
#[test]
fn ladder_capacity_is_initlock_over_interval() {
    for &(initlock, interval) in &PROFILES {
        assert_eq!(ladder_capacity(initlock, interval), 100, "both profiles: 100 hops");
        // Floor: hop 100 locks exactly at the co-sign anchor.
        assert_eq!(ladder_locktime(H, initlock, interval, 100), H);
        // Beyond capacity: no guard — one interval BELOW the anchor, returned Ok.
        assert_eq!(ladder_locktime(H, initlock, interval, 101), H - interval);
    }

    // Underflow boundary: with L_0 = 1000 (h_deposit = 0, deployed profile), hop 101 computes
    // 1000 − 1010 → u32 underflow. Branch on the build profile (`debug_assertions` is the proxy
    // for the default overflow-checks setting) so BOTH documented behaviours are asserted.
    if cfg!(debug_assertions) {
        let caught =
            std::panic::catch_unwind(|| calculate_block_height(1_000, 1_000, 10, 101, false));
        assert!(
            caught.is_err(),
            "interval*qt > h+initlock must underflow-panic under overflow checks (no guard exists)"
        );
    } else {
        let wrapped = calculate_block_height(1_000, 1_000, 10, 101, false)
            .expect("release: silent wrap, no error path");
        assert_eq!(wrapped, 1_000u32.wrapping_sub(1_010), "two's-complement wrap, no guard");
        assert!(
            wrapped > 500_000_000,
            "wrapped 'height' exceeds the LockTime::from_height block-height threshold and is \
             rejected downstream (lib/src/transaction.rs:232)"
        );
    }
}

// The exclusive exit window of the owner at hop k is [L_k, L_{k-1}): exactly `interval` blocks
// in which ONLY the current owner's backup is consensus-broadcastable, for every k up to the
// ladder capacity.
#[test]
fn exclusive_window_width_is_interval() {
    for &(initlock, interval) in &PROFILES {
        for k in 1..=ladder_capacity(initlock, interval) {
            let stale = ladder_locktime(H, initlock, interval, k - 1);
            let current = ladder_locktime(H, initlock, interval, k);
            assert_eq!(
                stale - current,
                interval,
                "window [L_{}, L_{}) must be exactly one interval wide ({initlock}/{interval})",
                k,
                k - 1
            );
        }
    }
}

// OPEN audit item [17] (docs/utexo/AUDIT-2026-07.md), encoded as executable knowledge.
//
// For an off-chain sub-coin the exit branch is locktime-free; safety requires broadcasting it
// before the EARLIEST ancestor stale backup matures. The implemented deadline
// (`UtexoWallet::deposit_anchored_exit_deadline`, wallet.rs:634-666, the audit-[10] fix) is
// deposit-anchored: `H_deposit + initlock`. Only the electrum resolution of `H_deposit` is
// async; the formula itself is the callable `crate::wallet::deposit_anchored_deadline`, which
// this test invokes DIRECTLY — so when the deadline is made k-aware (the audit-[17] fix) this
// test fails loudly instead of silently mis-documenting the fixed implementation as still
// gapped. The TRUE earliest hostile maturity is the splitter's own retained backup: if the
// parent was transferred k times before the split, that is `(H_deposit + initlock) − k·interval`
// (computed below with the REAL ladder function).
//
// Gap = k·interval blocks: the implemented deadline is EXACT (gap 0) iff the parent was never
// transferred before the split (k = 0), and LATE by k·interval otherwise — a watchtower
// honoring it could exit after a stale ancestor backup already matured. An ONLINE receiver is
// always safe (it can broadcast the locktime-free branch immediately); a receiver offline past
// the true deadline is exposed. Audit [17] is half-closed: `auto_exit_due(margin_blocks)`
// (batch 5) acts on this deadline, but ancestor locktimes are still not conveyed, so the margin
// must absorb the k·interval gap; do not read this test as proof the deadline is safe in general.
#[test]
fn deposit_anchored_deadline_exactness_domain() {
    let (initlock, interval) = DEPLOYED;

    // The REAL implemented formula (wallet.rs::deposit_anchored_deadline): H_deposit + initlock,
    // independent of k.
    let implemented_deadline = deposit_anchored_deadline(H, initlock);
    assert_eq!(implemented_deadline, H + initlock);

    for k in 0..=25 {
        // True earliest hostile maturity: the hop-k retained backup, via the REAL ladder fn.
        let true_earliest = ladder_locktime(H, initlock, interval, k);
        assert_eq!(true_earliest, H + initlock - k * interval);

        let gap = implemented_deadline - true_earliest;
        assert_eq!(gap, k * interval, "gap grows one interval per pre-split hop");
        assert_eq!(
            gap == 0,
            k == 0,
            "implemented deadline exact iff the parent was never transferred before the split"
        );
    }

    // Audit-[17] headline number: a parent split at hop 20 under the deployed 1000/10 profile
    // leaves the implemented deadline 200 blocks (~33 h) later than the true one.
    assert_eq!(implemented_deadline - ladder_locktime(H, initlock, interval, 20), 200);
}

// INV-10 economics: split fee reserve = (parent/100).clamp(300, 2000) sats
// (crate::transfer::split_fee_reserve — the REAL function). Exact boundary behaviour, including
// integer-division flooring around the 30_000-sat floor boundary and the 200_000-sat cap.
#[test]
fn fee_reserve_clamp_edges() {
    // Floor region: parent/100 < 300 → clamped up to 300.
    assert_eq!(split_fee_reserve(1_000), 300); // tiny parent: 10 → 300
    assert_eq!(split_fee_reserve(29_999), 300); // 299 (int div) → 300
    assert_eq!(split_fee_reserve(30_000), 300); // exactly 300: boundary sits ON the floor
    assert_eq!(split_fee_reserve(30_001), 300); // still 300 (int div)
    assert_eq!(split_fee_reserve(30_100), 301); // first parent strictly above the floor

    // Linear 1% region.
    assert_eq!(split_fee_reserve(100_000), 1_000);
    assert_eq!(split_fee_reserve(199_999), 1_999);

    // Cap region: parent/100 > 2000 → clamped down to 2000.
    assert_eq!(split_fee_reserve(200_000), 2_000); // exactly 2000: boundary sits ON the cap
    assert_eq!(split_fee_reserve(200_001), 2_000);
    assert_eq!(split_fee_reserve(10_000_000), 2_000); // huge parent → 2000
}

// Minimum splittable parent (extends select.rs::tests, does not duplicate them).
//
// The split EXECUTOR (`crate::transfer::split_amounts`, the REAL pure guard called by
// split_coin) requires every split output non-dust: piece ≥ 330, change = parent − piece −
// reserve ≥ 330, reserve = split_fee_reserve(parent) (floored at 300 for parents below 30_000).
// Smallest parent minting a 330-sat piece: 330 + 300 + 330 = 960 sats.
//
// The PLANNER (select.rs:104-113, the REAL `plan`) filters with a strict inequality
// `parent > piece + reserve + 330`, i.e. it demands change ≥ 331 — one sat stricter than the
// executor. The gap is one-directional safety (audit [29]): plan() never selects a parent the
// executor would then reject; the price is refusing the exactly-960 boundary parent.
#[test]
fn split_size_floor() {
    // Executor guard: the REAL split_amounts (fee reserve + fit + dust floor).
    let executor_accepts = |parent: u64, piece: u64| split_amounts(parent, piece).is_ok();
    assert_eq!(split_fee_reserve(960), 300, "floor reserve applies at this size");
    assert_eq!(
        split_amounts(960, DUST_LIMIT).ok(),
        Some((330, 300)),
        "960 = 330 piece + 300 reserve + 330 change"
    );
    assert!(!executor_accepts(959, DUST_LIMIT), "959 leaves 329-sat sub-dust change");

    // Planner boundary (REAL plan()): accepts 961 (change 331), refuses 960 (change 330 — the
    // strict `>` in select.rs:110 makes plan() 1 sat more conservative than the executor).
    let one_coin = |sats: u64| vec![Candidate { index: 0, amount_sats: sats }];
    assert_eq!(
        plan(&one_coin(961), 330),
        Plan::WithSplit { whole: vec![], split: 0, split_amount: 330 }
    );
    assert_eq!(plan(&one_coin(960), 330), Plan::Insufficient { available: 960 });

    // Agreement in the safe direction: any plan()-accepted boundary parent also passes the
    // executor guard.
    assert!(executor_accepts(961, 330));

    // Sub-dust remainder after whole-coin selection is refused (audit [29]) even though the
    // balance covers the target: 2000 taken whole, remainder 200 < 330 cannot be minted.
    let cs = vec![
        Candidate { index: 0, amount_sats: 2_000 },
        Candidate { index: 1, amount_sats: 5_000 },
    ];
    assert_eq!(plan(&cs, 2_200), Plan::Insufficient { available: 7_000 });
}

/// Synthetic depth-N exit-cost estimate: N split-branch txs at the measured 155 vB each
/// (1-in-2-out P2TR, sdk07/types.rs measurements; combine hops are larger — ~57.5 vB per extra
/// input — and are NOT modelled here) plus the measured 112 vB backup (1-in-1-out P2TR
/// keyspend). Uses the REAL `ExitCostEstimate::fee_sats_at` for all fee arithmetic.
fn synthetic_exit_estimate(depth: u32) -> ExitCostEstimate {
    let branch_vbytes = 155u64 * depth as u64;
    let backup_vbytes = 112u64;
    ExitCostEstimate {
        statechain_id: format!("model-depth-{depth}"),
        branch_txs: depth,
        branch_vbytes,
        backup_vbytes,
        total_vbytes: branch_vbytes + backup_vbytes,
        wait_blocks: 0,
        exit_deadline_block: None,
    }
}

// INV-17 exit-cost scaling: fee_sats_at is ceil(total·rate); cost is LINEAR in tree depth (each
// extra split level adds exactly one 155-vB branch tx). Note the production invariant
// `total_vbytes = branch_vbytes + backup_vbytes` is a property of `estimate_exit_cost`
// (wallet.rs:628) pinned by the sdk07 E2E against real pre-signed txs; the synthetic helper
// above mirrors it by construction, so asserting it here would only re-check the helper — the
// expected-totals table pins the numbers instead.
#[test]
fn exit_cost_scaling_model() {
    // (depth, expected total vbytes = 155·depth + 112)
    let expected = [(0u32, 112u64), (1, 267), (2, 422), (4, 732), (8, 1352)];
    let rates = [1u64, 2, 10, 30, 100];

    for &(depth, total) in &expected {
        let e = synthetic_exit_estimate(depth);
        assert_eq!(e.total_vbytes, total, "depth {depth}");
        for &r in &rates {
            // Integer rates: ceil(total·r) = total·r exactly.
            assert_eq!(e.fee_sats_at(r as f64), total * r, "depth {depth} @ {r} sat/vB");
        }
    }

    // Fractional rates round UP (ceil), never down.
    assert_eq!(synthetic_exit_estimate(2).fee_sats_at(1.1), 465); // ceil(422·1.1 = 464.2)

    // Linearity in depth: fee(d8) − fee(d4) == fee(d4) − fee(d0) == 4·155·rate.
    for &r in &rates {
        let fee = |d: u32| synthetic_exit_estimate(d).fee_sats_at(r as f64);
        assert_eq!(fee(8) - fee(4), fee(4) - fee(0), "equal per-4-level increments @ {r}");
        assert_eq!(fee(8) - fee(4), 620 * r);
    }
}

// Wall-clock model (no callable production function — the chain tip is external): the co-sign
// anchor's tip advances ~144 blocks/day, so the current owner's backup matures
// horizon_days(k) = (L_k − h)/144 = (initlock − k·interval)/144 days after deposit. The ladder
// side (L_k) is computed with the REAL calculate_block_height; only blocks→days is modelled.
#[test]
fn wall_clock_vs_hop_budget() {
    let horizon_days = |initlock: u32, interval: u32, k: u32| -> f64 {
        let l_k = ladder_locktime(H, initlock, interval, k);
        (l_k - H) as f64 / BLOCKS_PER_DAY as f64
    };

    // Day-0 horizons: deployed 1000/144 ≈ 6.94 days; defaults 10000/144 ≈ 69.4 days.
    let deployed_day0 = horizon_days(DEPLOYED.0, DEPLOYED.1, 0);
    assert!((deployed_day0 - 1_000.0 / 144.0).abs() < 1e-12);
    assert_eq!((deployed_day0 * 100.0).round() / 100.0, 6.94);

    let defaults_day0 = horizon_days(DEFAULTS.0, DEFAULTS.1, 0);
    assert!((defaults_day0 - 10_000.0 / 144.0).abs() < 1e-12);
    assert_eq!((defaults_day0 * 10.0).round() / 10.0, 69.4);

    for &(initlock, interval) in &PROFILES {
        // Each hop consumes exactly interval/144 days of the current owner's maturity horizon
        // (deployed: 10/144 ≈ 100 minutes per hop; defaults: 100/144 ≈ 16.7 h per hop).
        let per_hop = interval as f64 / BLOCKS_PER_DAY as f64;
        for k in 0..10 {
            let step =
                horizon_days(initlock, interval, k) - horizon_days(initlock, interval, k + 1);
            assert!(
                (step - per_hop).abs() < 1e-9,
                "hop {k} must consume interval/144 days ({initlock}/{interval})"
            );
        }
        // The full 100-hop budget consumes the entire horizon: the floor backup is mature the
        // moment it is co-signed.
        assert_eq!(horizon_days(initlock, interval, ladder_capacity(initlock, interval)), 0.0);
    }
}

// SPEC §3.6 / REQ-13 terminal predicate over the (sig_budget, finalized) domain, via the REAL
// `is_terminal` (the pure mirror of GET /statechain/spend_budget's `terminal` field).
//
// The (Some(0), 0) row encodes OPEN audit item [15] as executable knowledge: an attacker who
// replays the M1 owner-auth can call set_spend_budget with remaining = 0; the monotonic rule
// new = min(existing, finalized + remaining) (server/src/database/deposit.rs:170-188) then pins
// budget = finalized, making the coin terminal IMMEDIATELY — the SE refuses every further
// co-sign. The coin stays unilaterally exitable (griefing/brick, not theft).
#[test]
fn terminal_predicate_matrix() {
    let matrix: [(Option<i64>, i64, bool); 5] = [
        (None, 0, false),    // no budget set → never terminal (plain transferable coin)
        (Some(1), 0, false), // budget granted, not yet consumed
        (Some(1), 1, true),  // budget consumed exactly → terminal (ERR-3 on next sign)
        (Some(1), 2, true),  // over-consumed → still terminal (predicate is >=, not ==)
        (Some(0), 0, true),  // budget 0 → terminal with ZERO further co-signs: the audit-[15] brick
    ];
    for (budget, finalized, expected) in matrix {
        assert_eq!(
            is_terminal(budget, finalized),
            expected,
            "is_terminal({budget:?}, {finalized})"
        );
    }
}
