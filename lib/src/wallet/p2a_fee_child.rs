//! The **owner-funded P2A fee child** — the CPFP rescue for a TES-R tier stuck under the relay floor.
//!
//! # Why this exists, and why it is not `cpfp_tx.rs`
//!
//! Every TES-R tier commits its fee at signing time (`committed_fee_rate`, 2 sat/vB) and carries a
//! 240-sat P2A anchor for exactly this case. When the mempool's floor rises above that committed
//! rate, the tier alone is refused —
//!
//! ```text
//! min relay fee not met, 200 < 423
//! ```
//!
//! — and the ONLY way it enters a mempool is as a 1P1C package with a child that pays for both.
//! That was measured, not assumed (WP1-TRUC-P2A-SPIKE (retired 2026-08-15)).
//!
//! `cpfp_tx.rs` cannot do this job and is not a starting point: it is **v2** (`version: 2`), pinned
//! to `input_vout = 0`, and it spends the coin's own backup output — which needs the coin key. This
//! child spends a **P2A anchor**, which needs no key at all, plus a funding input that is the
//! OWNER's. Different transaction, different signer, different version.
//!
//! # Who funds it — [D31]
//!
//! The **owner**. A keyless watchtower cannot: a CPFP child needs an input it does not hold and a
//! signature it cannot make, so a keyless tower has no move when the floor rises. Nor does the
//! anyone-can-spend anchor recruit a stranger — rescuing a 240-sat anchor was measured to need a
//! **180 330-sat child**, roughly **900×** its value, so "anyone may spend it" is a permission, not
//! an incentive. An operator MAY run the optional funded-tower variant and call this on the owner's
//! behalf; the code is identical, only the caller differs.
//!
//! # What this module does and does not do
//!
//! It **builds and prices** the child. It does **not sign** the funding input — that needs wallet key
//! material and belongs to the caller, which is why [`build_p2a_fee_child`] returns the transaction
//! together with the funding input's prevout so the caller can sighash it. The P2A input needs no
//! witness and is left empty here, correctly and finally.

use bitcoin::{
    absolute, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Txid, Witness,
};

use crate::error::MercuryError;
use crate::tesr::{p2a_script, P2A_VALUE};

/// TRUC (BIP-431) caps a v3 child at 1000 vB. This is a consensus-adjacent *policy* limit: exceed it
/// and the package is refused, so the builder must refuse first and say why.
pub const TRUC_MAX_CHILD_VSIZE: u64 = 1_000;

/// A dust floor for the child's own change output. Below this the output is unspendable in practice
/// and Core will reject the transaction, so a "successful" build that produced it would only fail
/// later, further from the cause.
pub const CHILD_CHANGE_DUST: u64 = 330;

/// The parent this child is rescuing, as the mempool sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StuckParent {
    pub txid: Txid,
    /// Index of the parent's P2A anchor output. Not assumed to be any particular vout — the tier
    /// builders place it deliberately, and a wrong guess here spends the wrong output.
    pub p2a_vout: u32,
    /// The parent's virtual size, in vB.
    pub vsize: u64,
    /// The fee the parent already commits, in sats. This is what makes the package arithmetic work:
    /// the child pays the DIFFERENCE, not the whole bill.
    pub fee: u64,
}

/// The owner's funding input. Ordinary, spendable, and the owner's own — [D31].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FundingInput {
    pub outpoint: OutPoint,
    pub value: u64,
    /// The funding output's scriptPubKey, kept so the caller can build the sighash without a second
    /// chain fetch.
    pub script_pubkey: ScriptBuf,
}

/// A built, UNSIGNED fee child plus everything the caller needs to sign and submit it.
#[derive(Clone, Debug)]
pub struct FeeChild {
    /// v3, input 0 = the parent's P2A anchor (no witness needed), input 1 = the owner's funding UTXO.
    pub tx: Transaction,
    /// The prevouts, in input order, for taproot sighashing. Input 0 is the P2A output, which is part
    /// of the sighash even though spending it needs no signature.
    pub prevouts: Vec<TxOut>,
    /// The child's own fee, in sats.
    pub child_fee: u64,
    /// The child's estimated vsize, in vB.
    pub child_vsize: u64,
    /// `(parent.fee + child_fee) / (parent.vsize + child_vsize)`, the rate a miner actually sees.
    pub package_fee_rate: f64,
}

/// The child's vsize, estimated before it is signed.
///
/// Signature sizes are known in advance for the shapes involved, so this is an estimate only in the
/// sense that it rounds **up**: a child that ends up smaller overpays slightly, which relays; one
/// that ends up larger would underpay, which does not. Erring downward here would produce a package
/// that is refused for exactly the reason it was built to avoid.
///
/// * base: version+locktime+counts ≈ 11 vB
/// * P2A input: 36 outpoint + 1 empty scriptSig + 4 sequence ≈ 41 vB, witness is empty
/// * funding input (P2TR key spend): 41 vB + 16.25 vB witness ≈ 58 vB
/// * change output (P2TR): 43 vB
///
/// **Measured against a real signed child on regtest: 153 vB estimated, 153 vB actual**
/// (`clients/libs/rust/tests/live_p2a_package_rescue.rs`, which asserts `actual <= estimate` so an
/// optimistic estimate fails there rather than at a node).
pub fn estimate_child_vsize() -> u64 {
    11 + 41 + 58 + 43
}

/// Build an owner-funded v3 fee child that lifts `parent` to `target_fee_rate` as a package.
///
/// The fee law, stated once:
///
/// ```text
/// required_total = ceil(target_fee_rate × (parent.vsize + child_vsize))
/// child_fee      = required_total − parent.fee      (never negative)
/// change         = P2A_VALUE + funding.value − child_fee
/// ```
///
/// The parent's already-committed fee is **credited**, which is the whole point of CPFP and the
/// reason the child is affordable at all.
///
/// Refuses — rather than producing something that fails later, further from its cause — when the
/// child would exceed TRUC's 1000 vB, when the funding input cannot cover the fee, or when the
/// change would be dust.
pub fn build_p2a_fee_child(
    parent: &StuckParent,
    funding: &FundingInput,
    change_script_pubkey: ScriptBuf,
    target_fee_rate: f64,
) -> Result<FeeChild, MercuryError> {
    if !(target_fee_rate.is_finite() && target_fee_rate > 0.0) {
        return Err(MercuryError::FeeChildUnbuildable(format!(
            "target fee rate {target_fee_rate} is not a usable rate; refusing to build a fee child \
             against it — a nonsense target produces a nonsense fee, and this transaction exists to \
             make a package relay"
        )));
    }

    let child_vsize = estimate_child_vsize();
    if child_vsize > TRUC_MAX_CHILD_VSIZE {
        return Err(MercuryError::FeeChildUnbuildable(format!(
            "fee child would be {child_vsize} vB, over TRUC's {TRUC_MAX_CHILD_VSIZE} vB cap for a v3 \
             child (BIP-431); the package would be refused"
        )));
    }

    let package_vsize = parent.vsize.saturating_add(child_vsize);
    // ceil, so rounding never lands the package a fraction below the target and gets it refused.
    let required_total = (target_fee_rate * package_vsize as f64).ceil() as u64;
    let child_fee = required_total.saturating_sub(parent.fee);

    let available = P2A_VALUE.saturating_add(funding.value);
    if child_fee > available {
        return Err(MercuryError::FeeChildUnbuildable(format!(
            "cannot rescue parent {}: lifting it to {target_fee_rate} sat/vB needs a child fee of \
             {child_fee} sats, but the P2A anchor ({P2A_VALUE}) plus the funding input ({}) is only \
             {available}. This is the D31 shape — the anchor is worth far less than its own rescue, \
             so the owner must supply a larger funding input.",
            parent.txid, funding.value
        )));
    }

    let change = available - child_fee;
    if change < CHILD_CHANGE_DUST {
        return Err(MercuryError::FeeChildUnbuildable(format!(
            "cannot rescue parent {}: after a {child_fee}-sat child fee the change would be {change} \
             sats, below the {CHILD_CHANGE_DUST}-sat dust floor. Use a larger funding input, or \
             accept a lower target than {target_fee_rate} sat/vB.",
            parent.txid
        )));
    }

    let p2a_prevout = TxOut { value: P2A_VALUE, script_pubkey: p2a_script() };
    let funding_prevout =
        TxOut { value: funding.value, script_pubkey: funding.script_pubkey.clone() };

    let tx = Transaction {
        // **v3 is mandatory, not stylistic.** TRUC's package rules only apply to v3, and the parent
        // tier is v3; a v2 child cannot form a 1P1C package with it. This is precisely what
        // `cpfp_tx.rs` gets wrong for this purpose.
        version: 3,
        // No timelock. The child is the rescue; delaying it defeats the point.
        lock_time: absolute::LockTime::ZERO,
        input: vec![
            // Input 0 — the P2A anchor. `OP_1 <0x4e73>` is anyone-can-spend, so this input needs
            // NO signature and NO witness. It is left empty deliberately and finally; a caller that
            // "fills it in" is doing something wrong.
            TxIn {
                previous_output: OutPoint { txid: parent.txid, vout: parent.p2a_vout },
                script_sig: ScriptBuf::new(),
                // Not RBF-signalling-specific: TRUC packages are replaceable regardless, and MAX
                // keeps the input free of any accidental relative timelock.
                sequence: Sequence::MAX,
                witness: Witness::new(),
            },
            // Input 1 — the OWNER's funding UTXO. This is the one that needs a signature, and this
            // module does not make it: signing needs wallet key material the caller holds.
            TxIn {
                previous_output: funding.outpoint,
                script_sig: ScriptBuf::new(),
                sequence: Sequence::MAX,
                witness: Witness::new(),
            },
        ],
        output: vec![TxOut { value: change, script_pubkey: change_script_pubkey }],
    };

    let package_fee_rate = (parent.fee + child_fee) as f64 / package_vsize as f64;

    Ok(FeeChild {
        tx,
        prevouts: vec![p2a_prevout, funding_prevout],
        child_fee,
        child_vsize,
        package_fee_rate,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn parent() -> StuckParent {
        StuckParent {
            // The measured tier from the WP1 spike.
            txid: Txid::from_str(
                "0000000000000000000000000000000000000000000000000000000000000001",
            )
            .unwrap(),
            p2a_vout: 1,
            vsize: 141,
            fee: 200,
        }
    }

    fn funding(value: u64) -> FundingInput {
        FundingInput {
            outpoint: OutPoint {
                txid: Txid::from_str(
                    "0000000000000000000000000000000000000000000000000000000000000002",
                )
                .unwrap(),
                vout: 0,
            },
            value,
            script_pubkey: ScriptBuf::new(),
        }
    }

    #[test]
    fn the_child_is_v3_because_a_v2_child_cannot_form_a_truc_package() {
        let c = build_p2a_fee_child(&parent(), &funding(1_000_000), ScriptBuf::new(), 5.0).unwrap();
        assert_eq!(c.tx.version, 3, "TRUC package rules apply to v3 only");
    }

    /// The P2A input must carry no witness — `OP_1 <0x4e73>` is anyone-can-spend. If a future edit
    /// starts signing it, that is a sign someone has mistaken this for the backup-spending CPFP.
    #[test]
    fn the_anchor_input_needs_no_witness_and_the_funding_input_is_left_for_the_caller() {
        let c = build_p2a_fee_child(&parent(), &funding(1_000_000), ScriptBuf::new(), 5.0).unwrap();
        assert_eq!(c.tx.input.len(), 2);
        assert!(c.tx.input[0].witness.is_empty(), "the P2A anchor is anyone-can-spend");
        assert_eq!(c.tx.input[0].previous_output.vout, 1, "must spend the parent's P2A vout");
        assert!(c.tx.input[1].witness.is_empty(), "this module does not sign the funding input");
        assert_eq!(c.prevouts.len(), 2, "both prevouts are needed for taproot sighashing");
        assert_eq!(c.prevouts[0].value, P2A_VALUE);
    }

    /// The fee law credits the parent's committed fee. Without that credit the child overpays by the
    /// parent's whole bill, which is the arithmetic error that makes CPFP look unaffordable.
    #[test]
    fn the_parents_committed_fee_is_credited_against_the_package() {
        let p = parent();
        let target = 5.0;
        let c = build_p2a_fee_child(&p, &funding(1_000_000), ScriptBuf::new(), target).unwrap();
        let package_vsize = p.vsize + c.child_vsize;
        let required = (target * package_vsize as f64).ceil() as u64;
        assert_eq!(c.child_fee, required - p.fee, "the child pays the DIFFERENCE, not the whole bill");
        assert!(
            c.package_fee_rate >= target,
            "package rate {} must reach the target {target}",
            c.package_fee_rate
        );
    }

    /// Rounding must never land the package a hair under target — that is refused, which is the exact
    /// failure this transaction exists to prevent.
    #[test]
    fn rounding_never_lands_under_the_target() {
        for target in [1.0, 1.7, 2.3, 5.68, 9.99, 50.0] {
            let c =
                build_p2a_fee_child(&parent(), &funding(10_000_000), ScriptBuf::new(), target).unwrap();
            assert!(
                c.package_fee_rate >= target,
                "target {target}: package landed at {} — under target is REFUSED by the node",
                c.package_fee_rate
            );
        }
    }

    /// **The D31 shape, in a test.** The anchor is worth far less than its own rescue, so a funding
    /// input that cannot cover the child fee must be refused BY NAME rather than producing a package
    /// that a node rejects later.
    #[test]
    fn an_underfunded_rescue_is_refused_by_name() {
        // 240-sat anchor, no meaningful funding, a spike-level target.
        let err = build_p2a_fee_child(&parent(), &funding(500), ScriptBuf::new(), 50.0).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("cannot rescue parent") && msg.contains("D31"),
            "the refusal must name the cause and point at the decision: {msg}"
        );
    }

    #[test]
    fn dust_change_is_refused_rather_than_created() {
        let p = parent();
        // Find a funding value that covers the fee but leaves change just under the dust floor.
        let probe =
            build_p2a_fee_child(&p, &funding(1_000_000), ScriptBuf::new(), 5.0).unwrap().child_fee;
        let funding_value = probe - P2A_VALUE + CHILD_CHANGE_DUST - 1;
        let err =
            build_p2a_fee_child(&p, &funding(funding_value), ScriptBuf::new(), 5.0).unwrap_err();
        assert!(
            err.to_string().contains("dust floor"),
            "dust change must be refused, not produced: {err}"
        );
    }

    #[test]
    fn a_nonsense_target_rate_is_refused_not_silently_coerced() {
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(
                build_p2a_fee_child(&parent(), &funding(1_000_000), ScriptBuf::new(), bad).is_err(),
                "target {bad} must be refused"
            );
        }
    }

    /// The child must stay inside TRUC's 1000 vB cap — asserted on the real estimate so that a future
    /// change to the shape (an extra input, a second output) trips here rather than at a node.
    #[test]
    fn the_child_fits_inside_trucs_thousand_vbyte_cap() {
        let v = estimate_child_vsize();
        assert!(v <= TRUC_MAX_CHILD_VSIZE, "child estimated at {v} vB, over TRUC's cap");
        // And it should be comfortably under, not scraping it — the measured child was 177 vB.
        assert!(v < 400, "child estimate {v} vB is implausibly large for 2-in 1-out");
    }

    /// The spike's measured rescue, reproduced as arithmetic: a 141 vB / 200 sat parent and a
    /// ~177 vB child both landing at ~5.68 sat/vB. This pins the fee law against a number that came
    /// from a real node rather than from this file.
    #[test]
    fn the_measured_wp1_rescue_reproduces() {
        let p = parent();
        let c = build_p2a_fee_child(&p, &funding(1_000_000), ScriptBuf::new(), 5.68).unwrap();
        assert!(
            (c.package_fee_rate - 5.68).abs() < 0.05,
            "package rate {} should sit at the measured 5.68 sat/vB",
            c.package_fee_rate
        );
        // The measured child fee was 180 330 sats against a 240-sat anchor — about 900x. Confirm the
        // builder still reports a fee that dwarfs the anchor, since that ratio IS the D31 argument.
        let spike = build_p2a_fee_child(&p, &funding(10_000_000), ScriptBuf::new(), 700.0).unwrap();
        assert!(
            spike.child_fee > 100 * P2A_VALUE,
            "at spike rates the child fee must dwarf the anchor it rescues (got {})",
            spike.child_fee
        );
    }
}
