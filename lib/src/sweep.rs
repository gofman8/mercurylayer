//! [REQ-49…REQ-52] **§5.3 — the sweep: absorbing a leaf out of circulation, as arithmetic.**
//!
//! A leaf is a worse coin than a root in every respect: it inherits a deadline it does not control,
//! carries depth, has no one-transaction cooperative exit, and burns `BURN_SATS` of its own value if
//! it is ever walked out. The sweep replaces it, the moment it is first seen, with an ordinary root.
//!
//! # Why this module is pure, and stops at the decision
//!
//! Everything that makes the sweep right or wrong is arithmetic over four numbers and a leaf: no
//! network, no wallet, no chain. Kept pure, every boundary can be pinned exactly — including the
//! ones a live stack reaches only by luck, like a leaf one satoshi over the value ceiling or a
//! market one tenth of a sat/vB over the fee ceiling.
//!
//! **It deliberately does NOT absorb anything.** REQ-49 puts the swap in `claim()` and requires it
//! default-OFF until the cooperative exit it depends on is demonstrated end to end — and that has
//! not been demonstrated. So this module answers "may this leaf be absorbed, and at what floor
//! price", and nothing calls it yet. Shipping the decision without the mechanism is the safe half
//! first; the reverse — a mechanism that absorbs and asks afterwards — is how an operator ends up
//! holding leaves it cannot settle.
//!
//! # The one counter-intuitive fact, and what it forces
//!
//! **The surplus does not scale with the leaf's face value.** `BURN_SATS` is what a leaf's own two
//! pre-signed tiers destroy, and the marginal cost of one more combine input is a fixed vsize. So an
//! absorber earns the same ~1 057 sat at 3 sat/vB whether the leaf holds 1 560 sat or 1 BTC.
//!
//! That is why [`SweepLimits::max_leaf_value`] exists and why it is a RISK parameter rather than an
//! economic one: past some face the absorber is taking balance-sheet exposure for a return that has
//! stopped growing. It is also why batching is nearly irrelevant — 1 → 10 leaves moves the marginal
//! cost by ~150 sat against a ~1 057-sat surplus. Absorption is the business; consolidation is a
//! four-percent optimisation.

/// What a leaf's own two pre-signed tiers destroy if it is ever walked out (2 × 615 sat).
///
/// This is the whole source of the sweep's margin: an absorbed leaf is spent by a combine that never
/// broadcasts those tiers, so the burn is never realised. It does NOT scale with the leaf's value.
pub const BURN_SATS: u64 = 1_230;

/// The marginal vsize of one more input to the settling combine, in vB.
///
/// Marginal, not total: the absorber is settling a batch either way, so what one extra leaf costs is
/// one extra input, not a whole transaction.
///
/// DERIVED from the transaction module's own input model rather than restated as `57.75`. The two
/// must not be able to drift: this number is the entire economic case for the sweep, and a
/// hard-coded copy would keep quoting the old margin after the input model changed. A test below
/// checks it against `sweep_tx_vsize` by measuring an actual difference in size.
pub const COMBINE_MARGINAL_VB: f64 =
    (4 * crate::transaction::INPUT_BASE_BYTES + crate::transaction::INPUT_WITNESS_BYTES) as f64
        / 4.0;

/// Why a leaf may not be absorbed. Each variant is a distinct fact about the leaf or the market, so
/// a caller can tell "not worth it today" from "never, at any price" — the two call for opposite
/// responses, and a single boolean cannot distinguish them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SweepRefusal {
    /// The market is at or above the fee ceiling: the surplus has gone, and the payee is better off
    /// walking out on the tiers they already hold. TRANSIENT — the same leaf may qualify tomorrow.
    FeeRateTooHigh,
    /// Not enough runway left to settle the batch before the inherited deadline. Absorbing this leaf
    /// would buy a liability, not an asset: it cannot be settled, and its value voids in full.
    RunwayTooShort,
    /// Above the value ceiling. The surplus is constant in face, so this is balance-sheet risk taken
    /// for a return that has stopped growing.
    LeafTooLarge,
    /// This tree's absorbed exposure would exceed the cap. Bounds the loss if one tree's spine
    /// cannot be materialised — the failure that takes every absorbed leaf under it at once.
    TreeExposureExceeded,
}

impl SweepRefusal {
    /// Is this refusal one that a later attempt could clear by itself?
    ///
    /// Drives what a caller says to the payee. `FeeRateTooHigh` means "try later"; the other three
    /// are facts about this leaf or this operator's book that no amount of waiting changes, and
    /// telling a payee to retry into a permanent refusal is how a coin looks stuck.
    pub fn is_transient(self) -> bool {
        matches!(self, SweepRefusal::FeeRateTooHigh)
    }
}

/// [REQ-50] The four admission limits. **Configuration, not protocol constants** — but the defaults
/// are derived rather than chosen, and each one's derivation is the reason it has that value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SweepLimits {
    /// Above this the surplus `BURN_SATS − COMBINE_MARGINAL_VB·m` has shrunk far enough that the
    /// payee does better walking out on prepaid tiers. Default 15 sat/vB; the surplus reaches ZERO
    /// at 21.3, so the default keeps a deliberate margin below the point of indifference.
    pub max_fee_rate: f64,
    /// Minimum blocks of runway. Default 903 = `e_csv + confirmations` (723) + 25 %. Below it the
    /// leaf CANNOT be settled in time, so absorbing it is buying a liability.
    pub min_runway_blocks: u32,
    /// Default 100 000 sat. A risk-appetite ceiling, not an economic one — see the module note on
    /// the surplus being constant in face.
    pub max_leaf_value: u64,
    /// Default 1 000 000 sat per tree. Bounds the loss if one tree's spine cannot be materialised.
    pub max_tree_exposure: u64,
}

impl Default for SweepLimits {
    fn default() -> Self {
        Self {
            max_fee_rate: 15.0,
            min_runway_blocks: 903,
            max_leaf_value: 100_000,
            max_tree_exposure: 1_000_000,
        }
    }
}

/// The absorber's surplus per leaf at a given market rate, in sats. `None` once the market has eaten
/// it entirely — returned rather than saturating at zero, because "no surplus" and "a surplus of
/// zero" lead to the same decision but not the same explanation.
pub fn surplus_sats(fee_rate_sats_per_vb: f64) -> Option<u64> {
    let cost = COMBINE_MARGINAL_VB * fee_rate_sats_per_vb;
    if cost >= BURN_SATS as f64 {
        return None;
    }
    Some((BURN_SATS as f64 - cost).floor() as u64)
}

/// [REQ-52] The fairness floor: what the payee MUST receive, at minimum, for a leaf of this value.
///
/// `price_paid ≥ leaf_value − BURN_SATS`. The payee is no worse off than walking the leaf out, and
/// additionally receives a coin that is strictly better in kind. A swap priced below this floor
/// takes value from a payee who would have done better alone — the one outcome that makes the sweep
/// a tax rather than a service.
///
/// Saturating, so a leaf worth less than the burn floors at zero rather than wrapping to an enormous
/// obligation. Such a leaf is below the admission floor anyway and cannot be absorbed.
pub fn fair_price_floor(leaf_value: u64) -> u64 {
    leaf_value.saturating_sub(BURN_SATS)
}

/// [REQ-52] Does this price treat the payee fairly?
pub fn is_fair_price(leaf_value: u64, price_paid: u64) -> bool {
    price_paid >= fair_price_floor(leaf_value)
}

/// [REQ-50] May this leaf be absorbed? `Ok(())` only when ALL FOUR limits hold.
///
/// The checks run in a fixed order — market, runway, value, exposure — so that a refusal names the
/// most informative cause: a market that has closed the window is a different message from a leaf
/// that will never qualify, and an operator at its exposure cap needs to hear that rather than
/// something about fees.
pub fn may_absorb(
    leaf_value: u64,
    runway_blocks: u32,
    market_fee_rate: f64,
    tree_exposure: u64,
    limits: &SweepLimits,
) -> Result<(), SweepRefusal> {
    if !(market_fee_rate <= limits.max_fee_rate) {
        // Written as `!(<=)` on purpose: a NaN market rate fails this test rather than passing it.
        // A comparison that lets an unparseable rate through would admit every leaf at any price.
        return Err(SweepRefusal::FeeRateTooHigh);
    }
    if runway_blocks < limits.min_runway_blocks {
        return Err(SweepRefusal::RunwayTooShort);
    }
    if leaf_value > limits.max_leaf_value {
        return Err(SweepRefusal::LeafTooLarge);
    }
    // Checked in the leaf's own arithmetic, not by adding first: `tree_exposure + leaf_value` can
    // overflow a u64 from operator-supplied numbers, and an overflow here reads as plenty of room.
    if leaf_value > limits.max_tree_exposure.saturating_sub(tree_exposure) {
        return Err(SweepRefusal::TreeExposureExceeded);
    }
    Ok(())
}

/// [REQ-51] Should the holder of absorbed leaves settle NOW?
///
/// Either the batch has reached its target with the market at or under the ceiling, OR the earliest
/// inherited deadline is inside the runway — and **the deadline path ignores the fee ceiling
/// entirely.** The risk is asymmetric: settling early forfeits a few hundred sat of batching,
/// settling late voids the leaf for its full face. An expensive settlement beats a voided one at
/// every fee rate, so a fee check on this path would be a way to lose the whole position to save a
/// few hundred sat.
pub fn should_settle(
    batch_size: usize,
    target_batch: usize,
    earliest_runway_blocks: u32,
    market_fee_rate: f64,
    limits: &SweepLimits,
) -> bool {
    if batch_size == 0 {
        return false;
    }
    if earliest_runway_blocks <= limits.min_runway_blocks {
        return true;
    }
    batch_size >= target_batch && market_fee_rate <= limits.max_fee_rate
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_surplus_is_constant_in_face_and_vanishes_at_the_indifference_point() {
        // THE fact the whole section rests on: same surplus for a tiny leaf and a huge one.
        let at_three = surplus_sats(3.0).expect("a surplus exists at 3 sat/vB");
        assert_eq!(at_three, 1_056, "1 230 − 57.75×3 = 1 056.75, floored");
        // It reaches zero at 21.3 sat/vB — above that the payee does better walking out.
        assert!(surplus_sats(21.2).is_some(), "still a surplus just below the crossover");
        assert!(surplus_sats(21.3).is_none(), "the surplus is gone at the crossover");
        assert!(surplus_sats(50.0).is_none());
    }

    /// The constant is derived, so this pins it against the SIZE FUNCTION rather than against the
    /// number it was derived from — the only check that catches the input model moving underneath.
    /// Four steps, because 57.75 is not an integer and a single step hides in the rounding.
    #[test]
    fn the_marginal_agrees_with_the_transaction_modules_own_size_function() {
        use crate::transaction::sweep_tx_vsize;
        let base = sweep_tx_vsize(10, 1).expect("10 inputs");
        let four_more = sweep_tx_vsize(14, 1).expect("14 inputs");
        assert_eq!(
            (four_more - base) as f64,
            4.0 * COMBINE_MARGINAL_VB,
            "four more inputs must cost exactly 4 x the marginal this module bills for"
        );
        assert_eq!(COMBINE_MARGINAL_VB, 57.75, "and the derived value is still the documented one");
    }

    #[test]
    fn the_fairness_floor_never_takes_value_from_a_payee() {
        // Exactly the walk-out value: fair, by definition of the floor.
        assert_eq!(fair_price_floor(10_000), 8_770);
        assert!(is_fair_price(10_000, 8_770));
        // ONE satoshi below is not. This is the boundary the requirement exists to defend.
        assert!(!is_fair_price(10_000, 8_769));
        assert!(is_fair_price(10_000, 8_771));
        // A leaf worth less than the burn floors at zero rather than wrapping to a huge obligation.
        assert_eq!(fair_price_floor(500), 0);
        assert!(is_fair_price(500, 0));
    }

    #[test]
    fn all_four_limits_admit_only_together() {
        let l = SweepLimits::default();
        assert_eq!(may_absorb(50_000, 1_000, 3.0, 0, &l), Ok(()));

        // Each limit, at its boundary, in both directions.
        assert_eq!(may_absorb(50_000, 1_000, 15.0, 0, &l), Ok(()), "AT the fee ceiling is allowed");
        assert_eq!(
            may_absorb(50_000, 1_000, 15.1, 0, &l),
            Err(SweepRefusal::FeeRateTooHigh)
        );
        assert_eq!(may_absorb(50_000, 903, 3.0, 0, &l), Ok(()), "AT the runway floor is allowed");
        assert_eq!(
            may_absorb(50_000, 902, 3.0, 0, &l),
            Err(SweepRefusal::RunwayTooShort)
        );
        assert_eq!(may_absorb(100_000, 1_000, 3.0, 0, &l), Ok(()), "AT the value ceiling");
        assert_eq!(
            may_absorb(100_001, 1_000, 3.0, 0, &l),
            Err(SweepRefusal::LeafTooLarge)
        );
        assert_eq!(
            may_absorb(100_000, 1_000, 3.0, 900_000, &l),
            Ok(()),
            "exactly filling the exposure cap is allowed"
        );
        assert_eq!(
            may_absorb(100_000, 1_000, 3.0, 900_001, &l),
            Err(SweepRefusal::TreeExposureExceeded)
        );
    }

    #[test]
    fn a_nan_market_rate_is_refused_rather_than_admitted() {
        // `rate <= ceiling` is FALSE for NaN, so a naive `if rate > ceiling { refuse }` would admit
        // every leaf at an unparseable rate. This asserts the polarity that makes that impossible.
        let l = SweepLimits::default();
        assert_eq!(
            may_absorb(50_000, 1_000, f64::NAN, 0, &l),
            Err(SweepRefusal::FeeRateTooHigh)
        );
        assert_eq!(
            may_absorb(50_000, 1_000, f64::INFINITY, 0, &l),
            Err(SweepRefusal::FeeRateTooHigh)
        );
    }

    #[test]
    fn an_exposure_cap_cannot_be_overflowed_into_looking_empty() {
        let l = SweepLimits::default();
        assert_eq!(
            may_absorb(1, 1_000, 3.0, u64::MAX, &l),
            Err(SweepRefusal::TreeExposureExceeded),
            "an absurd running exposure must refuse, not wrap into room"
        );
    }

    #[test]
    fn only_the_fee_refusal_is_transient() {
        assert!(SweepRefusal::FeeRateTooHigh.is_transient());
        for permanent in [
            SweepRefusal::RunwayTooShort,
            SweepRefusal::LeafTooLarge,
            SweepRefusal::TreeExposureExceeded,
        ] {
            assert!(!permanent.is_transient(), "{permanent:?} cannot clear by waiting");
        }
    }

    #[test]
    fn the_deadline_path_settles_regardless_of_the_fee_ceiling() {
        let l = SweepLimits::default();
        // Batch target reached and the market is cheap: settle.
        assert!(should_settle(10, 10, 5_000, 3.0, &l));
        // Batch target reached but the market is expensive: WAIT, batching is worth a few hundred.
        assert!(!should_settle(10, 10, 5_000, 99.0, &l));
        // Deadline inside the runway and the market is absurd: settle ANYWAY. An expensive
        // settlement beats a voided leaf at every fee rate.
        assert!(should_settle(1, 10, 903, 10_000.0, &l));
        assert!(should_settle(1, 10, 100, f64::INFINITY, &l));
        // Nothing absorbed: nothing to settle, whatever the market does.
        assert!(!should_settle(0, 10, 1, 1.0, &l));
    }
}
