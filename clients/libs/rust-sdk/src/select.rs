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

/// Plan a payment of `target` sats over the candidate coins.
pub fn plan(coins: &[Candidate], target: u64) -> Plan {
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
    // Find the smallest unused coin that can cover the remainder and split it.
    let mut candidates: Vec<&Candidate> = coins
        .iter()
        .filter(|c| !whole.contains(&c.index) && c.amount_sats > remaining)
        .collect();
    candidates.sort_by_key(|c| c.amount_sats);
    match candidates.first() {
        Some(c) => Plan::WithSplit {
            whole,
            split: c.index,
            split_amount: remaining,
        },
        // Shouldn't happen when available >= target, but keep a safe fallback.
        None => Plan::Insufficient { available },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coins(v: &[u64]) -> Vec<Candidate> {
        v.iter()
            .enumerate()
            .map(|(index, &amount_sats)| Candidate { index, amount_sats })
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
        match plan(&coins(&[500, 300]), 600) {
            Plan::WithSplit { whole, split, split_amount } => {
                assert_eq!(whole, vec![0]);
                assert_eq!(split, 1);
                assert_eq!(split_amount, 100);
            }
            p => panic!("unexpected plan {p:?}"),
        }
    }

    #[test]
    fn insufficient() {
        assert_eq!(
            plan(&coins(&[100]), 200),
            Plan::Insufficient { available: 100 }
        );
    }
}
