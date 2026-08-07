//! Coin selection: exact-subset search plus split planning.
//!
//! Mercury transfers move whole statechain coins (like Spark leaves), so paying an arbitrary
//! amount means either finding a subset of coins that sums to it exactly, or minting the exact
//! amount by splitting one coin off-chain. `plan` returns the cheapest of the two.

/// A candidate coin for selection.
#[derive(Clone, Debug)]
pub struct Candidate {
    /// Index into the caller's coin list.
    pub index: usize,
    pub amount_sats: u64,
    /// May this coin be chosen as the SPLIT coin? A received in-ladder split child is spendable only
    /// whole (a child-level split is not implemented), so it is offered for exact/whole plans but
    /// never selected to be split.
    pub splittable: bool,
    /// **[K>1 prerequisite 3] Is this coin the wallet's own INVENTORY, rather than a payee-facing
    /// piece it happens to still hold?**
    ///
    /// The planner is otherwise shape-blind: it sees amounts, sorts split candidates ascending and
    /// takes the smallest that fits. That rule picks the wrong coin as soon as a wallet holds both a
    /// spine tip and a received piece, and it picks it in a way that gets worse over time:
    ///
    /// * splitting a received PIECE pushes that payee's leaf one level deeper and mints a crumb;
    /// * the crumb is smaller than the tip, so it sorts EARLIER next time;
    /// * so the next forecast miss splits the crumb, and the one after that its crumb.
    ///
    /// Self-reinforcing, and each turn of it deepens someone else's exit chain to save the sender an
    /// arithmetic surprise. So the preference is **hard** — every inventory candidate is preferred to
    /// every non-inventory one, at any amount — not a tiebreak that a 1-sat difference can invert.
    ///
    /// **Keyed on the record kind, resolved LOCALLY, never on `amount_sats`.** The wallet reads its
    /// own `spinetip-`/`ctesr-`/`tesr-` rows to answer this. Deriving it from the amount instead
    /// would hand a counterparty the choice of which of the recipient's coins gets split next, by
    /// choosing what they send them.
    pub is_inventory: bool,
}

/// Outcome of planning a payment of `target` sats.
#[derive(Clone, Debug, PartialEq)]
pub enum Plan {
    /// These coins sum to the target exactly — transfer them as-is.
    Exact(Vec<usize>),
    /// Transfer `whole` coins as-is and split the coin at `split` into
    /// (`split_amount`, remainder) to cover the rest.
    WithSplit {
        whole: Vec<usize>,
        split: usize,
        split_amount: u64,
    },
    /// Total balance is below the target.
    Insufficient { available: u64 },
}

/// Find an exact subset summing to `target`, if any. Dynamic programming over reachable sums;
/// state is bounded by the number of distinct reachable sums ≤ target, fine for wallet-sized
/// coin counts.
pub fn exact_subset(coins: &[Candidate], target: u64) -> Option<Vec<usize>> {
    use std::collections::HashMap;
    if target == 0 {
        return Some(vec![]);
    }
    // reachable sum -> indices used (first-found wins; favours fewer coins by iterating
    // largest-first)
    let mut order: Vec<&Candidate> = coins.iter().collect();
    order.sort_by(|a, b| b.amount_sats.cmp(&a.amount_sats));
    let mut reach: HashMap<u64, Vec<usize>> = HashMap::new();
    for c in order {
        if c.amount_sats > target {
            continue;
        }
        // snapshot keys to avoid reusing the same coin twice within this round
        let sums: Vec<(u64, Vec<usize>)> = reach.iter().map(|(s, v)| (*s, v.clone())).collect();
        if c.amount_sats == target {
            return Some(vec![c.index]);
        }
        reach.entry(c.amount_sats).or_insert_with(|| vec![c.index]);
        for (s, path) in sums {
            let ns = s + c.amount_sats;
            if ns > target || path.contains(&c.index) {
                continue;
            }
            if ns == target {
                let mut p = path.clone();
                p.push(c.index);
                return Some(p);
            }
            reach.entry(ns).or_insert_with(|| {
                let mut p = path.clone();
                p.push(c.index);
                p
            });
        }
    }
    None
}

/// Plan a payment of `target` sats over the candidate coins, using the bare dust floor as the
/// per-output minimum. The live signing path uses [`plan_with_floor`] with the backup-fee floor so
/// the piece it proposes is not just non-dust but also able to fund its own backup.
pub fn plan(coins: &[Candidate], target: u64) -> Plan {
    plan_with_floor(coins, target, crate::transfer::DUST_LIMIT)
}

/// Plan a payment, requiring both the minted piece and its change to be `>= min_output`. Callers
/// on the signing path pass `crate::transfer::min_split_output(fee_rate)` so a proposed split is
/// executor-admissible (piece and change each clear the dust floor AND fund their own backup),
/// never a doomed split that would strand the parent.
pub fn plan_with_floor(coins: &[Candidate], target: u64, min_output: u64) -> Plan {
    let available: u64 = coins.iter().map(|c| c.amount_sats).sum();
    if available < target {
        return Plan::Insufficient { available };
    }
    if let Some(exact) = exact_subset(coins, target) {
        return Plan::Exact(exact);
    }
    // No exact subset: greedily take the largest coins strictly below the remaining target,
    // then split one coin to cover the deficit.
    let mut order: Vec<&Candidate> = coins.iter().collect();
    order.sort_by(|a, b| b.amount_sats.cmp(&a.amount_sats));
    let mut whole: Vec<usize> = vec![];
    let mut remaining = target;
    for c in &order {
        if c.amount_sats <= remaining {
            whole.push(c.index);
            remaining -= c.amount_sats;
            if remaining == 0 {
                // exact_subset above would normally have caught this
                return Plan::Exact(whole);
            }
        }
    }
    // Find the smallest unused coin that can cover the remainder AND leave room for the split's fee
    // reserve plus a change that clears `min_output` (audit [29] + backup-fee floor). Filtering only
    // on `amount > remaining` would pick a coin the split path then rejects (piece + fee_reserve >=
    // parent, or a sub-viable change/piece), failing an otherwise-fundable payment; requiring the
    // piece itself to clear `min_output` avoids proposing a split whose piece cannot back itself.
    let mut candidates: Vec<&Candidate> = coins
        .iter()
        .filter(|c| {
            c.splittable
                && !whole.contains(&c.index)
                && remaining >= min_output
                && c.amount_sats
                    > remaining + crate::transfer::split_fee_reserve(c.amount_sats) + min_output
        })
        .collect();
    // [K>1 prerequisite 3] INVENTORY FIRST, and only then smallest-that-fits. `false < true`, so
    // `!is_inventory` puts every inventory candidate ahead of every non-inventory one regardless of
    // amount — a HARD preference, which is the point: as a tiebreak it would be inverted by a
    // one-sat difference, and the coin it would then pick is a payee's piece. See `Candidate`.
    candidates.sort_by_key(|c| (!c.is_inventory, c.amount_sats));
    match candidates.first() {
        Some(c) => Plan::WithSplit {
            whole,
            split: c.index,
            split_amount: remaining,
        },
        // No coin can mint the remainder as a viable piece with non-dust change.
        None => Plan::Insufficient { available },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coins(v: &[u64]) -> Vec<Candidate> {
        v.iter()
            .enumerate()
            .map(|(index, &amount_sats)| Candidate {
                index,
                amount_sats,
                splittable: true,
                // The pre-existing tests describe a wallet of ordinary own coins; the inventory
                // preference is exercised by the tests that mix the two kinds, below.
                is_inventory: true,
            })
            .collect()
    }

    #[test]
    fn exact_single() {
        assert_eq!(exact_subset(&coins(&[500, 300]), 300), Some(vec![1]));
    }

    #[test]
    fn exact_pair() {
        let got = exact_subset(&coins(&[500, 300, 200]), 700).unwrap();
        let sum: u64 = got.iter().map(|&i| [500u64, 300, 200][i]).sum();
        assert_eq!(sum, 700);
    }

    #[test]
    fn no_exact_plans_split() {
        match plan(&coins(&[5000, 3000]), 6000) {
            Plan::WithSplit { whole, split, split_amount } => {
                assert_eq!(whole, vec![0]);
                assert_eq!(split, 1);
                assert_eq!(split_amount, 1000);
            }
            p => panic!("unexpected plan {p:?}"),
        }
    }

    // Audit [29]: a remainder below the dust floor cannot be minted by a split (the piece would be
    // un-broadcastable); plan() must refuse rather than hand the split path a doomed piece.
    #[test]
    fn sub_dust_remainder_is_refused() {
        assert_eq!(
            plan(&coins(&[500, 300]), 600),
            Plan::Insufficient { available: 800 }
        );
    }

    #[test]
    fn insufficient() {
        assert_eq!(
            plan(&coins(&[100]), 200),
            Plan::Insufficient { available: 100 }
        );
    }

    // INV-9: WithSplit structural invariants (post-audit-[29]: the split coin must also cover the
    // fee reserve and leave non-dust change).
    #[test]
    fn with_split_invariants() {
        let cs = coins(&[5000, 3000]); // no exact subset for 6000
        match plan(&cs, 6000) {
            Plan::WithSplit { whole, split, split_amount } => {
                let whole_sum: u64 = whole.iter().map(|&i| cs[i].amount_sats).sum();
                assert!(whole_sum < 6000, "whole < target");
                assert_eq!(split_amount, 6000 - whole_sum, "split covers the deficit");
                let parent = cs[split].amount_sats;
                assert!(
                    parent > split_amount + crate::transfer::split_fee_reserve(parent) + 330,
                    "split coin covers piece + fee reserve + non-dust change"
                );
            }
            p => panic!("expected split, got {p:?}"),
        }
    }

    // ---- [K>1 prerequisite 3] THE INVENTORY PREFERENCE ------------------------------------------

    /// `(amount, is_inventory)` — the only two facts this preference is allowed to read.
    fn mixed(v: &[(u64, bool)]) -> Vec<Candidate> {
        v.iter()
            .enumerate()
            .map(|(index, &(amount_sats, is_inventory))| Candidate {
                index,
                amount_sats,
                splittable: true,
                is_inventory,
            })
            .collect()
    }

    /// THE DEFECT, executed. A wallet holding a small received PIECE and a large spine TIP must
    /// split the tip: splitting the piece deepens a payee's exit chain and mints a crumb that sorts
    /// even earlier next time.
    ///
    /// Both coins are viable here — this is not a fallback, it is a preference, and it has to hold
    /// when the *wrong* coin is the one the amount rule would pick.
    #[test]
    fn a_forecast_miss_splits_inventory_not_a_payees_piece() {
        //            index 0: a received piece (small)   index 1: the spine tip (large)
        let cs = mixed(&[(20_000, false), (900_000, true)]);
        match plan(&cs, 5_000) {
            Plan::WithSplit { split, split_amount, whole } => {
                assert!(whole.is_empty(), "nothing sums below the target here");
                assert_eq!(split_amount, 5_000);
                assert_eq!(
                    split, 1,
                    "the SPINE TIP (index 1) must be split even though the received piece at \
                     index 0 is smaller and also viable"
                );
            }
            p => panic!("expected a split, got {p:?}"),
        }
    }

    /// The preference is HARD, not a tiebreak: no amount gap reverses it. 40x here, and the
    /// direction must be unchanged.
    #[test]
    fn the_inventory_preference_is_not_a_tiebreak() {
        let cs = mixed(&[(25_000, false), (1_000_000, true)]);
        let picked = match plan(&cs, 5_000) {
            Plan::WithSplit { split, .. } => split,
            p => panic!("expected a split, got {p:?}"),
        };
        assert_eq!(picked, 1, "a 40x larger inventory coin still wins");
    }

    /// …and within a class the old rule is untouched: smallest that fits.
    #[test]
    fn among_inventory_the_smallest_that_fits_still_wins() {
        let cs = mixed(&[(900_000, true), (40_000, true), (20_000, false)]);
        match plan(&cs, 5_000) {
            Plan::WithSplit { split, .. } => assert_eq!(split, 1, "the 40 000-sat inventory coin"),
            p => panic!("expected a split, got {p:?}"),
        }
    }

    /// A wallet with NO inventory still pays. The preference orders candidates; it never removes
    /// them, because a payment refused for want of a tip is a payment the chain would have carried.
    #[test]
    fn with_no_inventory_a_piece_is_still_split() {
        let cs = mixed(&[(20_000, false), (900_000, false)]);
        match plan(&cs, 5_000) {
            Plan::WithSplit { split, .. } => assert_eq!(split, 0, "smallest that fits, as before"),
            p => panic!("expected a split, got {p:?}"),
        }
    }

    /// The flag is read, the amount is not. Same amounts, flags swapped, and the choice follows the
    /// FLAG — which is what stops a counterparty from steering the recipient's next split by
    /// choosing what they send.
    #[test]
    fn the_choice_follows_the_flag_and_not_the_amount() {
        let a = mixed(&[(20_000, false), (900_000, true)]);
        let b = mixed(&[(20_000, true), (900_000, false)]);
        let pick = |cs: &[Candidate]| match plan(cs, 5_000) {
            Plan::WithSplit { split, .. } => split,
            p => panic!("expected a split, got {p:?}"),
        };
        assert_eq!(pick(&a), 1);
        assert_eq!(pick(&b), 0);
    }

    // INV-9: exact subset sums to target exactly.
    #[test]
    fn exact_sums_to_target() {
        let cs = coins(&[500, 300, 200]);
        if let Some(idx) = exact_subset(&cs, 700) {
            let sum: u64 = idx.iter().map(|&i| cs[i].amount_sats).sum();
            assert_eq!(sum, 700);
        } else {
            panic!("expected exact subset");
        }
    }
}
