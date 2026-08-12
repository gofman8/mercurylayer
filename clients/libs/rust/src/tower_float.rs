//! **The tower funding rail** — keeping a funded tower able to pay, and knowing when it cannot.
//! [D31, PROTOCOL.md §5.13]
//!
//! D31 made fee-bumping owner-funded and offered the funded-tower variant as a deployment option. It
//! also named that option's duty in one sentence: *a tower that runs dry fails at exactly the moment
//! it is needed.* This module is what makes that duty checkable instead of hoped for.
//!
//! # The capacity bound is NOT sats, and that is the whole design
//!
//! §5.13 sizes a bond in sats — "~2 spike bumps, ≈15 000". That is the obvious unit and it is not
//! the binding one.
//!
//! A fee child is v3 and spends two things: the stuck tier's P2A anchor, and a funding UTXO. Under
//! TRUC (BIP-431) a v3 transaction may have at most **one** unconfirmed ancestor — and the tier is
//! already it. So a second rescue funded from the *unconfirmed change of the first* has two
//! unconfirmed ancestor chains and is refused at any price. **Measured, not reasoned:**
//! `clients/libs/rust/tests/live_tower_float.rs` gets
//!
//! ```text
//! TRUC-violation, tx <txid> would have too many ancestors
//! ```
//!
//! and the control — the same tier funded from a second CONFIRMED utxo — is accepted.
//!
//! **Therefore: simultaneous-rescue capacity = the number of CONFIRMED fee UTXOs, each large enough
//! for one bump.** A float of a million sats in ONE utxo rescues exactly one tier per confirmation
//! window, however many coins the tower watches. A tower sized only in sats reads as solvent and is
//! not, which is precisely the failure D31 warns about wearing a reassuring number.
//!
//! # What this module refuses to do
//!
//! It does not top itself up. Splitting the float into more UTXOs spends real sats and is an
//! operator's decision about their own money; [`FloatPlan`] says what shape is needed and why, and
//! stops there.

use anyhow::{anyhow, Result};
use electrum_client::ElectrumApi;

use mercurylib::wallet::p2a_fee_child::{estimate_child_vsize, FundingInput};

/// One spendable output of the fee float.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeeUtxo {
    pub outpoint: electrum_client::bitcoin::OutPoint,
    pub value: u64,
    /// **Load-bearing, not informational.** An unconfirmed fee UTXO cannot fund a rescue at all —
    /// see the TRUC measurement in this module's header — so a float that counts unconfirmed outputs
    /// toward its capacity is reporting a capacity it does not have.
    pub confirmed: bool,
}

/// The fee float as the chain currently shows it.
#[derive(Clone, Debug, Default)]
pub struct TowerFloat {
    pub utxos: Vec<FeeUtxo>,
}

/// What one rescue costs, at a given spike rate, for a tier of a given size.
///
/// The parent's committed fee is credited by the package arithmetic, so this is an UPPER bound taken
/// at the pessimistic assumption that the tier contributes nothing. Sizing a float on the optimistic
/// number would leave a tower short exactly when fees are high — which is when it is used.
pub fn bump_cost_sats(spike_rate_sat_per_vb: f64, tier_vsize: u64) -> u64 {
    let package_vsize = tier_vsize.saturating_add(estimate_child_vsize());
    (spike_rate_sat_per_vb * package_vsize as f64).ceil() as u64
}

/// A tower's ability to meet its obligations, in BOTH units — because passing one and failing the
/// other is the interesting case, and a single number cannot express it.
#[derive(Clone, Debug, PartialEq)]
pub struct Solvency {
    /// Coins this tower has undertaken to defend.
    pub obligations: usize,
    /// Confirmed UTXOs each individually large enough for one bump. **The binding limit.**
    pub capacity: usize,
    /// Confirmed sats, total.
    pub spendable_sats: u64,
    /// Sats needed to cover every obligation once.
    pub required_sats: u64,
    /// Unconfirmed sats — deliberately excluded from `spendable_sats`, reported so an operator can
    /// see that a top-up is in flight rather than concluding the float vanished.
    pub pending_sats: u64,
}

impl Solvency {
    /// Can this tower actually rescue every coin it watches, in one confirmation window?
    ///
    /// Both conditions, and the AND is the point: sats alone is the number that lies.
    pub fn is_covered(&self) -> bool {
        self.capacity >= self.obligations && self.spendable_sats >= self.required_sats
    }

    /// The operator-facing explanation. Says which unit failed, because the remedies differ: short
    /// on sats means "add money"; short on capacity means "split what you already have".
    pub fn explain(&self) -> String {
        if self.is_covered() {
            return format!(
                "covered: {} confirmed fee utxo(s) for {} watched coin(s), {} sat spendable against \
                 {} sat required",
                self.capacity, self.obligations, self.spendable_sats, self.required_sats
            );
        }
        let mut why = Vec::new();
        if self.capacity < self.obligations {
            why.push(format!(
                "CAPACITY: {} confirmed fee utxo(s) for {} watched coin(s) — a v3 fee child may have \
                 only one unconfirmed ancestor (the tier), so each simultaneous rescue needs its own \
                 CONFIRMED utxo. Adding sats to an existing utxo does NOT help; the float must be \
                 SPLIT into at least {} outputs",
                self.capacity, self.obligations, self.obligations
            ));
        }
        if self.spendable_sats < self.required_sats {
            why.push(format!(
                "SATS: {} confirmed against {} required ({} short){}",
                self.spendable_sats,
                self.required_sats,
                self.required_sats.saturating_sub(self.spendable_sats),
                if self.pending_sats > 0 {
                    format!("; {} sat unconfirmed and not yet usable", self.pending_sats)
                } else {
                    String::new()
                }
            ));
        }
        format!("UNDERFUNDED — {}", why.join(" | "))
    }
}

impl TowerFloat {
    /// Read the float from the chain: every UTXO paying `script_pubkey`.
    ///
    /// Electrum reports height 0 for a mempool entry, which is how `confirmed` is decided. `?` is
    /// load-bearing throughout: a backend that cannot be read must not produce an EMPTY float, which
    /// would read as "this tower is broke" and is a different, alarming claim from "I could not
    /// look".
    pub fn read(
        electrum: &electrum_client::Client,
        script_pubkey: &electrum_client::bitcoin::Script,
    ) -> Result<Self> {
        let listed = electrum.script_list_unspent(script_pubkey).map_err(|e| {
            anyhow!(
                "could not read the fee float from the chain backend: {e}. Refusing to report an \
                 empty float — 'I cannot look' and 'there is no money' are different facts and only \
                 one of them means the tower is broke."
            )
        })?;
        let utxos = listed
            .into_iter()
            .map(|u| FeeUtxo {
                outpoint: electrum_client::bitcoin::OutPoint {
                    txid: u.tx_hash,
                    vout: u.tx_pos as u32,
                },
                value: u.value,
                confirmed: u.height > 0,
            })
            .collect();
        Ok(Self { utxos })
    }

    pub fn spendable_sats(&self) -> u64 {
        self.utxos.iter().filter(|u| u.confirmed).map(|u| u.value).sum()
    }

    pub fn pending_sats(&self) -> u64 {
        self.utxos.iter().filter(|u| !u.confirmed).map(|u| u.value).sum()
    }

    /// How many rescues this float can fund SIMULTANEOUSLY: confirmed UTXOs that each, alone, cover
    /// one bump.
    ///
    /// Counted per-UTXO rather than by dividing the total, because the total is exactly the figure
    /// that misleads: two 10 000-sat UTXOs and one 20 000-sat UTXO hold the same sats and have
    /// different capacity.
    pub fn concurrent_capacity(&self, bump_cost: u64) -> usize {
        self.utxos.iter().filter(|u| u.confirmed && u.value >= bump_cost).count()
    }

    /// Assess this float against `obligations` coins at `spike_rate`.
    pub fn assess(&self, obligations: usize, spike_rate: f64, tier_vsize: u64) -> Solvency {
        let cost = bump_cost_sats(spike_rate, tier_vsize);
        Solvency {
            obligations,
            capacity: self.concurrent_capacity(cost),
            spendable_sats: self.spendable_sats(),
            required_sats: cost.saturating_mul(obligations as u64),
            pending_sats: self.pending_sats(),
        }
    }

    /// Pick a UTXO to fund one rescue.
    ///
    /// **Smallest-that-fits**, deliberately: spending the largest would collapse the float into one
    /// big change output and destroy the capacity that having several UTXOs provides. The greedy
    /// choice here is the one that preserves the property this module exists to protect.
    ///
    /// Refuses unconfirmed candidates BY NAME rather than skipping them silently, because "you have
    /// the money but not in a usable form" is the single most confusing state a tower operator can be
    /// in, and it deserves to be said out loud.
    pub fn select_funding(
        &self,
        needed: u64,
        script_pubkey: electrum_client::bitcoin::ScriptBuf,
    ) -> Result<FundingInput> {
        let mut usable: Vec<&FeeUtxo> =
            self.utxos.iter().filter(|u| u.confirmed && u.value >= needed).collect();
        usable.sort_by_key(|u| u.value);

        if let Some(u) = usable.first() {
            return Ok(FundingInput {
                outpoint: u.outpoint,
                value: u.value,
                script_pubkey,
            });
        }

        let pending_that_would_fit =
            self.utxos.iter().filter(|u| !u.confirmed && u.value >= needed).count();
        if pending_that_would_fit > 0 {
            return Err(anyhow!(
                "the fee float holds {pending_that_would_fit} UNCONFIRMED output(s) large enough for \
                 this {needed}-sat rescue, and none confirmed. They cannot be used: a v3 fee child \
                 may have only one unconfirmed ancestor and the stuck tier is already it, so a child \
                 funded from unconfirmed change is refused with `TRUC-violation … too many \
                 ancestors` (measured — `live_tower_float.rs`). Wait for a confirmation, or keep a \
                 confirmed reserve so the tower is never in this position during a spike."
            ));
        }
        Err(anyhow!(
            "the fee float has no confirmed output covering a {needed}-sat rescue ({} sat confirmed \
             across {} output(s), {} sat unconfirmed). Top up the float, or lower the target rate.",
            self.spendable_sats(),
            self.utxos.iter().filter(|u| u.confirmed).count(),
            self.pending_sats()
        ))
    }
}

/// The shape a float must have to cover `obligations`, and what it would cost to get there.
///
/// Advisory ONLY. Splitting spends the operator's sats and is their call; this says what is needed
/// and why, so the decision is informed rather than discovered during an incident.
#[derive(Clone, Debug, PartialEq)]
pub struct FloatPlan {
    pub want_utxos: usize,
    pub have_utxos: usize,
    pub per_utxo_sats: u64,
    /// Additional sats the operator must add. 0 when the float merely needs re-shaping — which is
    /// the case worth calling out, since it is fixed for free.
    pub top_up_sats: u64,
    pub reshape_only: bool,
}

/// Plan a float for `obligations` coins at `spike_rate`, with `reserve_multiple` bumps of headroom
/// per coin (§5.13 suggests ~2).
pub fn plan_float(
    float: &TowerFloat,
    obligations: usize,
    spike_rate: f64,
    tier_vsize: u64,
    reserve_multiple: u64,
) -> FloatPlan {
    let cost = bump_cost_sats(spike_rate, tier_vsize);
    let per_utxo = cost.saturating_mul(reserve_multiple.max(1));
    let want = obligations.max(1);
    let have = float.concurrent_capacity(per_utxo);
    let required_total = per_utxo.saturating_mul(want as u64);
    let spendable = float.spendable_sats();
    FloatPlan {
        want_utxos: want,
        have_utxos: have,
        per_utxo_sats: per_utxo,
        top_up_sats: required_total.saturating_sub(spendable),
        // Enough money, wrong shape — the case a sats-only view cannot see.
        reshape_only: spendable >= required_total && have < want,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use electrum_client::bitcoin::{OutPoint, ScriptBuf, Txid};
    use std::str::FromStr;

    fn utxo(n: u8, value: u64, confirmed: bool) -> FeeUtxo {
        FeeUtxo {
            outpoint: OutPoint {
                txid: Txid::from_str(&format!("{:064x}", n)).unwrap(),
                vout: 0,
            },
            value,
            confirmed,
        }
    }

    /// **The finding this module exists for.** Same sats, different shape, different capacity — and
    /// the one-big-utxo float is the one that reads as healthy.
    #[test]
    fn one_large_utxo_and_several_small_ones_hold_equal_sats_and_unequal_capacity() {
        let cost = 10_000;
        let one_big = TowerFloat { utxos: vec![utxo(1, 60_000, true)] };
        let several = TowerFloat {
            utxos: vec![
                utxo(2, 20_000, true),
                utxo(3, 20_000, true),
                utxo(4, 20_000, true),
            ],
        };
        assert_eq!(one_big.spendable_sats(), several.spendable_sats(), "equal sats");
        assert_eq!(one_big.concurrent_capacity(cost), 1, "one utxo = one simultaneous rescue");
        assert_eq!(several.concurrent_capacity(cost), 3);

        // And solvency must follow capacity, not sats: 3 coins, plenty of money, still not covered.
        let s = one_big.assess(3, 5.0, 141);
        assert!(!s.is_covered(), "a one-utxo float cannot defend 3 coins at once");
        assert!(s.explain().contains("CAPACITY"), "the failing unit must be named: {}", s.explain());
        assert!(
            s.explain().contains("must be SPLIT"),
            "and the remedy must be the right one — adding sats does not fix a shape problem"
        );
    }

    /// Unconfirmed outputs must never count. This is the TRUC bound, measured in
    /// `live_tower_float.rs`, expressed as arithmetic.
    #[test]
    fn unconfirmed_outputs_count_toward_nothing_spendable() {
        let f = TowerFloat {
            utxos: vec![utxo(1, 50_000, false), utxo(2, 50_000, false), utxo(3, 12_000, true)],
        };
        assert_eq!(f.spendable_sats(), 12_000);
        assert_eq!(f.pending_sats(), 100_000);
        assert_eq!(f.concurrent_capacity(10_000), 1, "only the confirmed one can fund a rescue");
    }

    /// "You have the money, just not usably" is the most confusing state an operator can be in, so
    /// it must be refused with that explanation rather than a bare not-enough-funds.
    #[test]
    fn a_float_that_is_only_unconfirmed_is_refused_with_the_truc_reason() {
        let f = TowerFloat { utxos: vec![utxo(1, 500_000, false)] };
        let err = f.select_funding(10_000, ScriptBuf::new()).unwrap_err().to_string();
        assert!(err.contains("UNCONFIRMED"), "{err}");
        assert!(err.contains("only one unconfirmed ancestor"), "explain WHY it is unusable: {err}");
        assert!(err.contains("live_tower_float.rs"), "cite the measurement: {err}");
    }

    /// Selection must not collapse the float. Spending the biggest utxo would leave one large change
    /// output and destroy the capacity that having several provides.
    #[test]
    fn selection_takes_the_smallest_that_fits_to_preserve_capacity() {
        let f = TowerFloat {
            utxos: vec![
                utxo(1, 500_000, true),
                utxo(2, 12_000, true),
                utxo(3, 80_000, true),
                utxo(4, 9_000, true), // too small
            ],
        };
        let picked = f.select_funding(10_000, ScriptBuf::new()).unwrap();
        assert_eq!(picked.value, 12_000, "smallest that fits, not first and not largest");
    }

    /// A tower short only on SHAPE is fixable for free, and the plan must say so — an operator told
    /// to "top up" when they need to split will spend money and still be uncovered.
    #[test]
    fn a_reshape_is_distinguished_from_a_top_up() {
        let cost = bump_cost_sats(5.0, 141);
        let plenty_one_utxo = TowerFloat { utxos: vec![utxo(1, cost * 50, true)] };
        let plan = plan_float(&plenty_one_utxo, 4, 5.0, 141, 2);
        assert!(plan.reshape_only, "enough sats, wrong shape");
        assert_eq!(plan.top_up_sats, 0, "no money needs adding");
        assert_eq!(plan.want_utxos, 4);
        assert_eq!(plan.have_utxos, 1);

        let broke = TowerFloat { utxos: vec![utxo(1, 100, true)] };
        let plan2 = plan_float(&broke, 4, 5.0, 141, 2);
        assert!(!plan2.reshape_only, "this one genuinely needs money");
        assert!(plan2.top_up_sats > 0);
    }

    /// The bump cost must be taken pessimistically — the parent's committed fee is credited in
    /// practice, so budgeting on the credited figure leaves a tower short precisely during a spike.
    #[test]
    fn the_bump_cost_covers_the_whole_package_not_just_the_child() {
        let tier_vsize = 141;
        let cost = bump_cost_sats(50.0, tier_vsize);
        let package_vsize = tier_vsize + estimate_child_vsize();
        assert_eq!(cost, (50.0 * package_vsize as f64).ceil() as u64);
        assert!(
            cost > 50 * estimate_child_vsize(),
            "budgeting only the child's own vsize would under-fund every rescue"
        );
    }

    #[test]
    fn a_covered_tower_says_so_with_both_numbers() {
        let f = TowerFloat {
            utxos: vec![utxo(1, 1_000_000, true), utxo(2, 1_000_000, true)],
        };
        let s = f.assess(2, 5.0, 141);
        assert!(s.is_covered());
        let e = s.explain();
        assert!(e.starts_with("covered:"), "{e}");
        assert!(e.contains("2 confirmed fee utxo(s) for 2 watched coin(s)"), "{e}");
    }
}
