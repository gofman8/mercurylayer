//! Executable model of coin granularity (partial amounts) — the executable companion of
//! `docs/spark/GRANULARITY-SPEC.md` (sibling of `invalidation_model.rs`, the companion of
//! `docs/spark/INVALIDATION-SPEC.md` — the invalidation mechanics themselves live there and are
//! NOT restated here).
//!
//! Pure logic only: no network, no DB, no running stack. Wherever a callable pure function
//! exists these tests call the REAL one:
//!
//! - [`crate::transfer::split_amounts`] — the split executor's admission guard (fee reserve +
//!   fit + dust floor), also the arithmetic behind the COLORED split (tokens.rs:484-490
//!   duplicates its reserve/change expressions),
//! - [`crate::transfer::split_fee_reserve`] — the split miner-fee reserve clamp,
//! - [`crate::select::plan`] / [`crate::select::exact_subset`] — payment planning: exact
//!   subsets, whole-coins-then-split, typed insufficiency,
//! - [`crate::tokens::TOKEN_PIECE_SATS`] — the fixed 1500-sat packaging of a token piece,
//! - [`crate::types::ExitCostEstimate::fee_sats_at`] — ceil fee arithmetic (INV-17).
//!
//! Only quantities with no callable pure implementation are modelled, each with a citation to
//! the production code it mirrors: raw-unit token arithmetic (tokens.rs:491 `token_change =
//! carrier_amount - token_amount`; precision is contract metadata the SDK never scales by) and
//! the measured exit vsizes (backup 112 vB, split branch 155 vB — types.rs tests + sdk07;
//! width split 241 vB for 1-in-4-out — sdk26).
//!
//! Boundary rule discovered and pinned by [`split_bounds_exact_boundary`]: the EXECUTOR
//! (`split_amounts`) admits change == 330 exactly (its checks are `piece + reserve < parent`,
//! strict, and `change < DUST_LIMIT`, i.e. change ≥ 330 inclusive), so the smallest parent that
//! splits a 330-sat piece is **960** = 330 + 300 + 330. The PLANNER (`select::plan`,
//! select.rs:110) filters split candidates with the strict `parent > piece + reserve + 330`, so
//! it refuses 960 and first accepts **961** — one sat more conservative, in the safe direction
//! (audit [29]: plan() never selects a parent the executor then rejects). The planner side is
//! pinned by `invalidation_model::split_size_floor`; this module pins the executor side.

use crate::select::{exact_subset, plan, Candidate, Plan};
use crate::tokens::TOKEN_PIECE_SATS;
use crate::transfer::{split_amounts, split_fee_reserve, DUST_LIMIT};
use crate::types::ExitCostEstimate;

/// Measured backup-tx vsize: 1-in-1-out P2TR keyspend (types.rs tests + sdk07).
const BACKUP_VB: u64 = 112;
/// Measured split-branch-tx vsize: 1-in-2-out P2TR (types.rs tests + sdk07).
const BRANCH_VB: u64 = 155;
/// Measured `transfer_many` split-tx vsize: 1 input, 3 pieces + change = 4 P2TR outputs
/// (sdk26). ONE such tx is shared by every piece of a width split.
const WIDTH_SPLIT_VB: u64 = 241;
/// Per-piece share of the shared width-split tx: 241/4 ≈ 60 vB (sdk26's headline "~60 vB/piece
/// vs 155 vB/piece"). Integer division of the measured 4-output tx; wider fan-outs add ~43 vB
/// per extra P2TR output, so this share SHRINKS as k grows — using the 4-output measurement for
/// all k is conservative in the width-vs-depth comparison below.
const WIDTH_PER_PIECE_SHARE_VB: u64 = WIDTH_SPLIT_VB / 4;

fn coins(v: &[u64]) -> Vec<Candidate> {
    v.iter()
        .enumerate()
        .map(|(index, &amount_sats)| Candidate { index, amount_sats })
        .collect()
}

// GRN piece/parent floors, exact boundaries, via the REAL executor guard `split_amounts`
// (transfer.rs:612-628).
//
// THE RULE (read from the code, all three probes below): with reserve floored at 300
// (`split_fee_reserve`, parents < 30_000), `split_amounts(parent, piece)` admits a split iff
//   piece ≥ 330  AND  change = parent − piece − reserve ≥ 330  AND  piece + reserve < parent,
// where BOTH dust comparisons are INCLUSIVE of 330 (the code rejects on `< DUST_LIMIT`). Hence
// the smallest parent minting a 330-sat piece is 960 = 330 + 300 + 330 EXACTLY, with change
// landing on the dust floor. 959 leaves 329-sat sub-dust change; 961 is comfortably inside.
// (The planner is one sat stricter — 961 — by the strict `>` in select.rs:110; see module docs.)
#[test]
fn split_bounds_exact_boundary() {
    // Piece floor: 329 refused, 330 admitted (parent 1_000, reserve floored at 300).
    assert_eq!(split_fee_reserve(1_000), 300);
    assert!(split_amounts(1_000, 329).is_err(), "329-sat piece is sub-dust");
    assert_eq!(
        split_amounts(1_000, DUST_LIMIT).ok(),
        Some((370, 300)),
        "330-sat piece admitted: change 370, reserve 300"
    );

    // Parent floor for a 330-sat piece: the three probes.
    assert!(
        split_amounts(959, DUST_LIMIT).is_err(),
        "959: change = 959 - 330 - 300 = 329 < 330 — sub-dust, refused"
    );
    assert_eq!(
        split_amounts(960, DUST_LIMIT).ok(),
        Some((330, 300)),
        "960 is the EXACT executor minimum: change lands ON the 330 dust floor (inclusive)"
    );
    assert_eq!(
        split_amounts(961, DUST_LIMIT).ok(),
        Some((331, 300)),
        "961: one sat of headroom (and the smallest parent the PLANNER accepts)"
    );

    // Fit bound is strict: piece + reserve == parent is refused even before the dust check.
    assert!(split_amounts(630, DUST_LIMIT).is_err(), "330 + 300 == 630: no room for change");
}

// GRN-INV-1b: the 330-sat DUST floor above (what `split_amounts` enforces) is NOT the minimum
// *mintable* piece. Each sub-coin also needs a valid backup, and `create_tx1` sweeps
// `piece − ceil(BACKUP_TX_SIZE·fee_rate)` and rejects it below the dust floor (FeeTooLow,
// lib/src/transaction.rs:122-132). So the true minimum mintable piece is `330 + ceil(112·rate)`
// — 442 sats at 1 sat/vB (measured by sdk28: backup_fee=112, min_mintable_piece=442). This test
// pins that arithmetic and documents the GUARD GAP: `split_amounts` admits the doomed [330, 442)
// band, which the executor only rejects AFTER making the parent terminal (stranding it). Encoded
// so that when the guard is fixed to reject the band up-front, the boundary here is the reference.
#[test]
fn backup_fee_floor_is_the_true_mintable_minimum() {
    // Minimum mintable piece at a given backup feerate (sat/vB): 330 + ceil(112 * rate).
    let min_mintable = |rate: f64| DUST_LIMIT + (BACKUP_VB as f64 * rate).ceil() as u64;
    assert_eq!(min_mintable(1.0), 442, "330 + ceil(112*1) = 442 (sdk28 measured)");
    assert_eq!(min_mintable(2.0), 554, "330 + 224");
    assert_eq!(min_mintable(10.0), 1450, "330 + 1120 — the floor climbs with feerate");

    // The GUARD GAP: split_amounts (the pure admission guard) accepts a 330-sat piece out of a
    // 2_000-sat parent, but that piece is NOT mintable (its 330-sat sub-coin cannot back itself at
    // 1 sat/vB: 330 - 112 = 218 < 330). The doomed band is [330, 442) at 1 sat/vB. (Parent 2_000
    // leaves ample change so the dust guard on change never confounds the piece-floor point.)
    assert!(
        split_amounts(2_000, DUST_LIMIT).is_ok(),
        "the pure guard admits a 330 piece (dust floor only) — the backup-fee shortfall is unguarded"
    );
    assert!(
        split_amounts(2_000, min_mintable(1.0)).is_ok(),
        "a 442-sat piece is both dust-valid AND backup-viable at 1 sat/vB"
    );
    // Every piece in the doomed band passes the dust guard but is not mintable.
    for piece in [DUST_LIMIT, 400, 441] {
        assert!(
            split_amounts(2_000, piece).is_ok(),
            "piece {piece} passes the dust guard (the gap: not yet rejected for the backup-fee shortfall)"
        );
        assert!(piece < min_mintable(1.0), "…yet {piece} < 442, so its backup is FeeTooLow");
    }
}

// GRN conservation: piece + change + reserve == parent for every admitted split — no sat is
// created or lost by the off-chain split (the reserve is the split tx's pre-committed miner
// fee, not a residue). Grid spans [961, 100M] including both reserve-clamp edges
// (30_000 → floor 300; 200_000 → cap 2_000) and their ±1 neighbours; expected reserves pinned
// exactly, as returned BY `split_amounts` (checking it propagates `split_fee_reserve`).
#[test]
fn reserve_change_conservation() {
    // (parent, expected reserve from (parent/100).clamp(300, 2000))
    let grid: [(u64, u64); 15] = [
        (961, 300),
        (1_000, 300),
        (2_130, 300),
        (9_999, 300),
        (29_999, 300),         // 299 by int-div → floored to 300
        (30_000, 300),         // exactly 300: boundary ON the floor
        (30_001, 300),         // int-div still 300
        (30_100, 301),         // first parent strictly above the floor
        (50_000, 500),         // linear 1% region
        (100_000, 1_000),
        (199_999, 1_999),
        (200_000, 2_000),      // exactly 2000: boundary ON the cap
        (200_001, 2_000),
        (1_000_000, 2_000),
        (100_000_000, 2_000),  // 1 BTC (coin sats are u32 in production, SPEC §14 — well inside)
    ];

    for &(parent, expected_reserve) in &grid {
        let max_piece = parent - expected_reserve - DUST_LIMIT;
        // Probe the floor piece, the ceiling piece, and a mid piece.
        for piece in [DUST_LIMIT, (DUST_LIMIT + max_piece) / 2, max_piece] {
            let (change, reserve) = split_amounts(parent, piece)
                .unwrap_or_else(|e| panic!("parent {parent} piece {piece}: {e}"));
            assert_eq!(reserve, expected_reserve, "reserve pinned at parent {parent}");
            assert_eq!(
                piece + change + reserve,
                parent,
                "conservation at parent {parent}, piece {piece}"
            );
        }
        // One past the ceiling piece: change would be 329 — refused, so conservation is never
        // satisfied by shaving the reserve or the change below dust.
        assert!(split_amounts(parent, max_piece + 1).is_err(), "parent {parent}: ceiling+1");
    }
}

// GRN 1-sat resolution: above the dust floor, granularity is EXACTLY one sat. For a mid-sized
// parent every piece in [330, parent − reserve − 330] is admitted at 1-sat steps (the full
// range is iterated — 48_841 admissions), and both endpoints are exact: the floor piece leaves
// maximal change, the ceiling piece leaves change == 330 (the dust floor, inclusive).
#[test]
fn one_sat_resolution() {
    let parent = 50_000u64;
    let reserve = split_fee_reserve(parent);
    assert_eq!(reserve, 500, "1% of 50_000, inside the clamp");
    let max_piece = parent - reserve - DUST_LIMIT; // 49_170

    for piece in DUST_LIMIT..=max_piece {
        let (change, r) = split_amounts(parent, piece)
            .unwrap_or_else(|e| panic!("piece {piece} refused: {e}"));
        assert_eq!(r, reserve);
        assert_eq!(change, parent - piece - reserve, "exact change at piece {piece}");
    }

    // Endpoints, exact.
    assert_eq!(split_amounts(parent, DUST_LIMIT).ok(), Some((49_170, 500)));
    assert_eq!(
        split_amounts(parent, max_piece).ok(),
        Some((DUST_LIMIT, 500)),
        "ceiling piece 49_170 leaves change exactly 330"
    );
    // Just outside on both sides.
    assert!(split_amounts(parent, DUST_LIMIT - 1).is_err());
    assert!(split_amounts(parent, max_piece + 1).is_err());
}

// GRN payment planning: one tight fixture per `plan` path (transfer.rs:35-127 routes on these).
//
// The WithSplit fixture pins the audit-[29] candidate rule (select.rs:104-113): a split
// candidate must cover remaining + its OWN fee reserve + 330 (strictly), not merely exceed the
// remaining amount — the naive rule would pick the 1000-sat coin (1000 > 500) whose split the
// executor then refuses (change 200 < 330), failing a fundable payment.
#[test]
fn plan_paths_matrix() {
    // Exact, single coin: one coin equals the target.
    assert_eq!(plan(&coins(&[4_200, 1_300]), 1_300), Plan::Exact(vec![1]));

    // Exact, multi-coin subset: 500 + 200 == 700 (no single coin matches). Same subset via the
    // REAL exact_subset that plan() delegates to.
    let cs = coins(&[500, 300, 200]);
    let Plan::Exact(mut idx) = plan(&cs, 700) else { panic!("expected Exact") };
    idx.sort_unstable();
    assert_eq!(idx, vec![0, 2], "500 + 200 == 700 exactly");
    let mut direct = exact_subset(&cs, 700).expect("subset exists");
    direct.sort_unstable();
    assert_eq!(direct, vec![0, 2]);

    // WithSplit, fee-reserve-aware (audit [29]): whole-coins-first greediness takes the LARGEST
    // coin (2000) whole, leaving remaining = 500. Of the unused coins, 1000 FAILS the candidate
    // rule (needs > 500 + 300 + 330 = 1130) even though 1000 > 500; 1200 passes and is chosen.
    let cs = coins(&[2_000, 1_000, 1_200]);
    assert_eq!(
        plan(&cs, 2_500),
        Plan::WithSplit { whole: vec![0], split: 2, split_amount: 500 },
        "whole = the largest coin; split coin = smallest candidate satisfying the [29] rule"
    );
    // The rule's safety direction, checked against the REAL executor: the refused 1000-sat coin
    // is exactly the one split_amounts would doom; the chosen 1200-sat coin passes.
    assert!(split_amounts(1_000, 500).is_err(), "1000: change 200 < 330 — doomed pick avoided");
    assert_eq!(split_amounts(1_200, 500).ok(), Some((400, 300)));

    // Insufficient: balance below target (ERR-9 at the transfer layer).
    assert_eq!(plan(&coins(&[100, 150]), 300), Plan::Insufficient { available: 250 });

    // Insufficient: sub-dust remainder — balance COVERS the target (20_000 > 10_100) but the
    // 100-sat remainder after taking one 10_000 whole cannot be minted as a piece (< 330), and
    // no exact subset exists: refused rather than minting an unbroadcastable split (audit [29]).
    // The fixture is chosen to pin the `remaining >= DUST_LIMIT` clause (select.rs:108) ALONE:
    // the unused 10_000-sat coin passes the size rule (10_000 > 100 + 300 + 330), so only that
    // clause stands between this plan and a doomed WithSplit{split_amount: 100}.
    assert_eq!(
        plan(&coins(&[10_000, 10_000]), 10_100),
        Plan::Insufficient { available: 20_000 }
    );
}

// GRN token-carrier floor, modelled with the REAL `split_amounts` at piece = TOKEN_PIECE_SATS.
//
// A colored split carves (piece = 1500 sats + exact token amount, change = rest − reserve).
// tokens.rs:484-490 computes reserve/change with the SAME expressions as
// split_fee_reserve/split_amounts, but its own early guard (:485) only checks the FIT
// (piece + reserve < carrier); the change dust floor is enforced downstream when the split
// PSBT is built (lib/src/transaction.rs:357-360, `get_unsigned_split_psbt`, audit [9]). The
// EFFECTIVE minimum carrier is therefore governed by the full split_amounts rule:
//   min carrier = TOKEN_PIECE_SATS + reserve(min 300) + change(min 330) = 2130 sats,
// with 2129 failing the sats math (change 329) and 2130 passing with change exactly 330.
#[test]
fn token_split_bounds_model() {
    assert_eq!(TOKEN_PIECE_SATS, 1_500, "fixed packaging of a token piece (tokens.rs:23)");

    // Both floors pinned: reserve at its 300 floor for carriers this small.
    assert_eq!(split_fee_reserve(2_130), 300);
    assert_eq!(TOKEN_PIECE_SATS + 300 + DUST_LIMIT, 2_130, "the 2130 identity");

    assert!(
        split_amounts(2_129, TOKEN_PIECE_SATS).is_err(),
        "2129: change = 2129 - 1500 - 300 = 329 — sub-dust, the colored split PSBT is refused"
    );
    assert_eq!(
        split_amounts(2_130, TOKEN_PIECE_SATS).ok(),
        Some((DUST_LIMIT, 300)),
        "2130 is the EXACT minimum carrier: change lands on the 330 dust floor"
    );

    // The tokens.rs:485 early guard alone ("carrier coin too small") fires at the FIT bound:
    // carrier ≤ 1800 = 1500 + 300. split_amounts subsumes it.
    assert!(split_amounts(1_800, TOKEN_PIECE_SATS).is_err(), "1500 + 300 == 1800: no fit");
}

// GRN raw-units model (pure — no callable SDK function scales token amounts, BY DESIGN).
//
// Token amounts are raw u64 units end-to-end; `precision` (u8) is contract metadata only
// (rgb14 pins the metadata; the SDK never multiplies or divides by it). So "0.1 of an asset"
// is representable iff precision ≥ 1, and equals 10^(precision−1) raw units. Conservation is
// the u64 identity mirrored from tokens.rs:491 (`token_change = carrier_amount − token_amount`;
// rgb-lib enforces Σin = Σout per contract at coloring time, INV-13).
#[test]
fn raw_units_precision_model() {
    /// Raw units in "0.1" of a precision-`p` asset, if representable.
    fn tenth_in_raw_units(precision: u8) -> Option<u64> {
        if precision >= 1 {
            Some(10u64.pow(precision as u32 - 1))
        } else {
            None // precision 0: 1 raw unit is the whole token — no tenth exists
        }
    }

    assert_eq!(tenth_in_raw_units(0), None, "indivisible asset: 0.1 not representable");
    assert_eq!(tenth_in_raw_units(1), Some(1));
    assert_eq!(tenth_in_raw_units(2), Some(10));
    assert_eq!(tenth_in_raw_units(8), Some(10_000_000), "BTC-like precision");
    assert_eq!(tenth_in_raw_units(18), Some(100_000_000_000_000_000), "10^17: max tested (rgb14)");

    // The tokens.rs:491 change formula (`carrier_amount − token_amount`) against INDEPENDENTLY
    // pinned literals — not re-derived, so a regression in the mirrored expression would fail.
    // Includes the two degenerate sends: the 1-unit send (minimum possible: 1 raw unit) and the
    // full spend (change 0 — the carrier's change output is left UNCOLORED, a plain BTC
    // sub-coin).
    let grid: [(u64, u64, u64); 6] = [
        // (total, send, expected change)
        (1, 1, 0),                       // 1-unit allocation, full spend
        (1_000, 1, 999),                 // 1-unit send
        (1_000, 1_000, 0),               // full spend
        (1_000, 250, 750),               // sdk02's 250/750 split shape
        (1_000_000_000_000_000_000, 1, 999_999_999_999_999_999), // 1-unit send at 10^18 scale
        (u64::MAX, u64::MAX, 0),         // no headroom needed: change 0, no overflow
    ];
    for &(total, send, expected_change) in &grid {
        assert_eq!(
            total - send, // tokens.rs:491
            expected_change,
            "change at total {total}, send {send}"
        );
    }
}

// GRN packaging-dust threshold (exit economics of the 1500-sat piece), via the REAL
// `ExitCostEstimate::fee_sats_at` (types.rs:138-140, INV-17 ceil arithmetic).
//
// A token piece's backup tx (112 vB measured) sweeps its 1500-sat packaging minus the fee. The
// sats zero out exactly when ceil(112·rate) ≥ 1500, i.e. at integer feerates the threshold is
// 14 sat/vB: 112·13 = 1456 < 1500 < 1568 = 112·14. Above it the sweep is economically dust —
// but ONLY the packaging: the token allocation is untouched (the colored split txs are the RGB
// witnesses; the allocation settles on-chain when they confirm, INV-16). Branch txs carry
// pre-committed fees from the parent's reserve and do not draw on the 1500.
#[test]
fn token_packaging_exit_economics() {
    let piece_backup = ExitCostEstimate {
        statechain_id: "model-token-piece".into(),
        branch_txs: 0, // branch fees are pre-committed (split fee reserve), not paid by the piece
        branch_vbytes: 0,
        backup_vbytes: BACKUP_VB,
        total_vbytes: BACKUP_VB,
        wait_blocks: 0,
        exit_deadline_block: None,
    };

    // Exact ceil arithmetic on both sides of the threshold.
    assert_eq!(piece_backup.fee_sats_at(13.0), 1_456);
    assert_eq!(piece_backup.fee_sats_at(14.0), 1_568);

    // The packaging-dust threshold: net sats at 13 sat/vB = 44; negative (zeroed out) at 14.
    assert!(TOKEN_PIECE_SATS > piece_backup.fee_sats_at(13.0), "13 sat/vB: 44 sats net remain");
    assert!(TOKEN_PIECE_SATS < piece_backup.fee_sats_at(14.0), "14 sat/vB: sweep zeroes out");
    assert_eq!(TOKEN_PIECE_SATS - piece_backup.fee_sats_at(13.0), 44);
}

// GRN width-vs-depth: paying k parties by ONE transfer_many width split beats k chained
// depth splits on exit weight, for every k ≥ 2.
//
// Depth model: k successive splits chain the change; the coin at depth k (each piece i sits at
// depth i; the LAST piece and the change tail sit at depth k) exits with k branch txs + its
// backup = 112 + 155·k vB (measured per-hop growth, sdk26; same linearity the invalidation
// model pins in exit_cost_scaling_model). Width model: ONE shared split tx (241 vB measured
// for 4 outputs, sdk26) amortizes to ~60 vB/piece; every piece sits at depth 1 and exits with
// 112 + 60 vB attributed. Both models use sdk26's measured constants — see the module-level
// constants for the conservatism note on the 60-vB share. The per-piece comparison is routed
// through the REAL `ExitCostEstimate::fee_sats_at` (mirroring
// invalidation_model::exit_cost_scaling_model), so the vB models are compared in actual
// ceil-fee sats, not bare arithmetic.
#[test]
fn width_vs_depth_weight_model() {
    // Depth-k last coin as a real ExitCostEstimate: k branch txs + the backup.
    let depth_estimate = |k: u64| ExitCostEstimate {
        statechain_id: format!("model-depth-{k}"),
        branch_txs: k as u32,
        branch_vbytes: BRANCH_VB * k,
        backup_vbytes: BACKUP_VB,
        total_vbytes: BACKUP_VB + BRANCH_VB * k,
        wait_blocks: 0,
        exit_deadline_block: None,
    };
    // A width piece: depth 1, its backup plus its attributed share of the ONE shared split tx.
    let width_piece = ExitCostEstimate {
        statechain_id: "model-width-piece".into(),
        branch_txs: 1,
        branch_vbytes: WIDTH_PER_PIECE_SHARE_VB,
        backup_vbytes: BACKUP_VB,
        total_vbytes: BACKUP_VB + WIDTH_PER_PIECE_SHARE_VB,
        wait_blocks: 0,
        exit_deadline_block: None,
    };
    let depth_last_coin = |k: u64| depth_estimate(k).total_vbytes;
    // Aggregate over k pieces from a depth chain: piece i at depth i.
    let depth_total = |k: u64| (1..=k).map(depth_last_coin).sum::<u64>();
    // Aggregate width: k backups + the ONE shared split tx (broadcast once by whoever exits first).
    let width_total = |k: u64| k * BACKUP_VB + WIDTH_SPLIT_VB;

    // Pinned constants and exact anchors — these are the test's teeth: they fail if the
    // measured 112/155/241 constants are edited inconsistently, and 267 cross-pins the depth-1
    // exit to invalidation_model::exit_cost_scaling_model and the types.rs measurements.
    assert_eq!(WIDTH_PER_PIECE_SHARE_VB, 60, "241/4 (sdk26)");
    assert_eq!(width_piece.total_vbytes, 172);
    assert_eq!(depth_last_coin(1), 267, "depth-1 exit (matches invalidation model)");
    assert_eq!(depth_last_coin(2), 422);
    assert_eq!(depth_last_coin(4), 732);
    assert_eq!(width_total(2), 465); // 2·112 + 241
    assert_eq!(depth_total(2), 689); // 267 + 422
    assert_eq!(width_total(3), 577); // 3·112 + 241
    assert_eq!(depth_total(3), 1_266); // 267 + 422 + 577

    // Exact fee anchors through the real ceil arithmetic (INV-17), fractional rate included:
    // ceil(172·1.1) = 190 vs ceil(732·1.1) = 806.
    assert_eq!(width_piece.fee_sats_at(1.1), 190);
    assert_eq!(depth_estimate(4).fee_sats_at(1.1), 806);

    for k in 2..=10u64 {
        // Per-piece, in FEE SATS via the real fee_sats_at: even the amortized-worst width piece
        // beats the depth chain's LAST coin at every probed feerate.
        for rate in [1.0, 1.1, 30.0, 100.0] {
            assert!(
                width_piece.fee_sats_at(rate) < depth_estimate(k).fee_sats_at(rate),
                "k={k} @ {rate} sat/vB: width piece {} < depth-{k} last coin {}",
                width_piece.fee_sats_at(rate),
                depth_estimate(k).fee_sats_at(rate)
            );
        }
        // Aggregate: all k width pieces together beat the k chained pieces together.
        assert!(
            width_total(k) < depth_total(k),
            "k={k}: width total {} vB < depth total {} vB",
            width_total(k),
            depth_total(k)
        );
        // The gap grows with k — each extra chained piece adds a 112 + 155·k coin, each extra
        // width piece a flat 112 vB backup (its branch share is already sunk). This is a
        // telescoping consequence of the closure definitions, so it is stated, not asserted
        // (asserting it could never fail and would only re-check the closures).
    }
}
