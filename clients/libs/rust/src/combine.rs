//! # THE LEAF COMBINE — one transaction, N confirmed leaves, one consolidated on-chain UTXO.
//!
//! An in-ladder split makes the final tier a SPINE tier `SP` with one payload output per leaf. The
//! leaves share the prefix `T -> X_m -> SP` and each carries its own tail (`ext_child` +
//! `state_child`). Walking those tails is the only way a leaf has ever reached chain, and it is
//! expensive in every dimension at once — for 60 leaves: prefix 3 084 vB + 60 x 250 vB of tails =
//! 18 084 vB across 129 transactions and 2 160 blocks of accumulated CSV, ending as 60 dust UTXOs.
//!
//! **THE STRUCTURAL FACT THIS MODULE IS BUILT ON.** Once `SP` CONFIRMS, every `SP.out[j]` is an
//! ordinary on-chain P2TR paying that leaf's aggregate key. The owner can spend it with a FRESH
//! co-signature carrying NO timelock — it does not have to be the pre-signed extension tier. N such
//! outputs go in ONE transaction: prefix 3 084 vB + a single ~3 519 vB sweep, no CSV at all, ending
//! as ONE UTXO. Roughly 2.7x cheaper and ~15 days faster.
//!
//! ## What this is NOT
//!
//! * **It does not replace the exit.** The pre-signed tiers remain valid and remain the guarantee.
//!   The combine is a cheaper HAPPY path that exists only while the SE co-signs; the tail is the
//!   path that works when it does not. See `an_abandoned_combine_leaves_the_leaf_exitable`.
//! * **It is not a re-entry into a fresh statechain coin.** V1 sweeps to one caller-controlled
//!   address; depositing that UTXO back through the normal flow is the caller's business.
//! * **It never touches coloured leaves.** An RGB allocation is sealed to `SP.out[j]`; spending it
//!   in a plain (un-coloured) transaction destroys the allocation irrecoverably. That refusal lives
//!   on the BUILD path here, not in a selection helper a lower-level caller could skip, and it is
//!   FAIL-CLOSED: [`ColourEvidence`] is a per-leaf VERDICT, so a caller that established nothing is
//!   refused rather than told "nothing is coloured".
//!
//! ## Where the pieces are
//!
//! The multi-input co-signing stack already existed and is colour-agnostic — this module adds no
//! signing machinery and reuses the sighash/prevout assembly verbatim:
//! [`mercurylib::transaction::get_unsigned_combine_psbt`],
//! [`mercurylib::transaction::get_partial_sig_request_for_colored_tx_multi`] (prevouts matched to
//! coins BY OUTPOINT, every input committing to the whole set via `Prevouts::All`), and
//! [`mercurylib::transaction::new_backup_transaction_multi`]. The SE sees N independent, ordinary
//! `sign_first`/`sign_second` round-trips — one per `statechain_id` — and never learns they belong
//! to one transaction. Nothing in `server/`, `lockbox/` or `enclave/` changes.
//!
//! P0-1 (secnonce single-use) is satisfied structurally and must stay that way: each input is a
//! distinct `statechain_id`, so each gets its own fresh secnonce, spent exactly once. Sessions are
//! never batched or reused.

use anyhow::Result;
use mercurylib::transaction::DUST_LIMIT;
use mercurylib::wallet::Coin;

/// Journal key prefix, alongside `tesr-` / `ctesr-` / `spinetip-` / the split journal's own.
pub const COMBINE_JOURNAL_PREFIX: &str = "combine-";

/// `"txid:vout"` for a coin, or `None` if either field is absent.
pub fn coin_outpoint(c: &Coin) -> Option<String> {
    match (c.utxo_txid.as_deref(), c.utxo_vout) {
        (Some(t), Some(v)) => Some(format!("{t}:{v}")),
        _ => None,
    }
}

/// **Every way a leaf combine can be refused.** Typed, following the
/// `SdkError::CoinBelowMaintenanceCost` precedent: the caller must be able to tell "this batch is
/// uneconomic at this fee rate" from "one of these leaves carries tokens" without parsing prose.
///
/// Every variant names WHAT is wrong and WHICH leaf, because a refusal a user cannot act on is a
/// refusal they will work around.
#[derive(Debug, Clone, PartialEq)]
pub enum CombineRefusal {
    /// No leaves selected.
    NoInputs,
    /// A leaf is missing one of the four fields the PSBT builder reads per input.
    LeafMissingField { statechain_id: String, field: &'static str },
    /// A leaf is not in this wallet's own CONFIRMED liveness allowlist — it is not provably ours to
    /// spend. Carries the status by name so the caller can see WHICH lane already has it.
    LeafNotLive { statechain_id: String, status: String },
    /// A leaf is a duplicate-deposit clone (`duplicate_index != 0`), not the live coin.
    LeafIsDuplicate { statechain_id: String, duplicate_index: u32 },
    /// A selected leaf carries an RGB allocation. Sweeping it in a plain transaction destroys the
    /// allocation with no recovery.
    ColouredLeaf { statechain_id: String, outpoint: String },
    /// **The fail-CLOSED half of the coloured refusal.** Nobody positively established whether this
    /// leaf carries an RGB allocation, so the only safe reading is that it might.
    ColourUnknown { statechain_id: String, outpoint: String },
    /// Some selected leaves are coloured and some are not. Named separately from
    /// [`Self::ColouredLeaf`] because the caller's mistake is different: they have mixed two
    /// inventories, and dropping the coloured ones would silently change what they asked for.
    MixedSelection { coloured: Vec<String>, plain_count: usize },
    /// The same outpoint appears twice. Bitcoin rejects the transaction outright, but only after N
    /// SE co-signatures have been spent on it.
    DuplicateInput { outpoint: String },
    /// A leaf's funding output (`SP.out[j]`) is not confirmed to the wallet's confirmation target.
    LeafNotConfirmed { outpoint: String, confirmations: u32, required: u32 },
    /// A leaf's funding output has already been spent.
    LeafAlreadySpent { outpoint: String },
    /// The batch cannot pay for its own sweep: `total_in < fee + DUST_LIMIT`.
    BelowSweepCost {
        n_inputs: usize,
        total_in_sats: u64,
        fee_sats: u64,
        dust_sats: u64,
        fee_rate_sats_per_vb: f64,
    },
    /// The fee model itself refused (nonsensical rate, zero inputs).
    Unpriceable { reason: String },
}

impl std::fmt::Display for CombineRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoInputs => write!(f, "leaf combine refused: no leaves were selected"),
            Self::LeafMissingField { statechain_id, field } => write!(
                f,
                "leaf combine refused: leaf {statechain_id} has no `{field}` — the combine PSBT \
                 builder reads utxo_txid, utxo_vout, amount and aggregated_address per input, and \
                 a leaf missing any of them is not an adopted leaf"
            ),
            Self::LeafNotLive { statechain_id, status } => write!(
                f,
                "leaf combine refused: leaf {statechain_id} is {status}, not CONFIRMED. A combine \
                 SPENDS the leaf's funding output, so it may only ever run on a coin this wallet \
                 can still prove is its own, unspent, and held by nobody else. CONFIRMED is that \
                 proof; every lane that hands value away moves the coin out of it first \
                 (IN_TRANSFER/TRANSFERRED for a conveyance, WITHDRAWING/WITHDRAWN for a spend, \
                 INVALIDATED for a superseded duplicate). Sweeping one of those would be spending \
                 a coin a counterparty already holds material over"
            ),
            Self::LeafIsDuplicate { statechain_id, duplicate_index } => write!(
                f,
                "leaf combine refused: leaf {statechain_id} is duplicate-deposit clone \
                 #{duplicate_index}, not the live coin. A clone shares the statechain id but not \
                 the funding output the ladder was built over, so co-signing it would spend N \
                 secnonces on an input the SE's key share does not govern"
            ),
            Self::ColouredLeaf { statechain_id, outpoint } => write!(
                f,
                "leaf combine refused: leaf {statechain_id} ({outpoint}) carries an RGB allocation. \
                 The allocation is sealed to that outpoint; spending it in a PLAIN transaction \
                 destroys it irrecoverably, and no consignment can recover it afterwards. Move the \
                 asset off this leaf first, or exit it via its coloured pre-signed tiers"
            ),
            Self::ColourUnknown { statechain_id, outpoint } => write!(
                f,
                "leaf combine refused: nothing established whether leaf {statechain_id} \
                 ({outpoint}) carries an RGB allocation. A plain sweep of a coloured leaf destroys \
                 the allocation irrecoverably, so 'nobody looked' cannot be read as 'it is plain' — \
                 the two are indistinguishable to this function and only one of them is safe. \
                 Supply a ColourVerdict for EVERY selected leaf, produced by something that \
                 actually queried the RGB engine (`UtexoWallet::unspendable_as_btc_outpoints`, \
                 which itself fails closed on an unreadable engine)"
            ),
            Self::MixedSelection { coloured, plain_count } => write!(
                f,
                "leaf combine refused: the selection MIXES {} coloured leaf/leaves ({}) with \
                 {plain_count} plain one(s). A plain sweep would destroy every coloured \
                 allocation in it; silently dropping the coloured leaves would sweep something \
                 other than what was asked for. Split the selection and combine the plain leaves \
                 on their own",
                coloured.len(),
                coloured.join(", ")
            ),
            Self::DuplicateInput { outpoint } => write!(
                f,
                "leaf combine refused: outpoint {outpoint} appears more than once in the \
                 selection — Bitcoin rejects a transaction that spends the same output twice, and \
                 the rejection would arrive only after N SE co-signatures had been spent on it"
            ),
            Self::LeafNotConfirmed { outpoint, confirmations, required } => write!(
                f,
                "leaf combine refused: leaf funding output {outpoint} has {confirmations} \
                 confirmation(s), below the required {required}. A combine spends SP.out[j] as an \
                 ORDINARY on-chain UTXO, which it only becomes once SP is confirmed; an SP still in \
                 the mempool can be replaced, taking every co-signature in this batch with it"
            ),
            Self::LeafAlreadySpent { outpoint } => write!(
                f,
                "leaf combine refused: leaf funding output {outpoint} is already spent — its \
                 extension tier, a previous combine, or a rival has taken it"
            ),
            Self::BelowSweepCost { n_inputs, total_in_sats, fee_sats, dust_sats, fee_rate_sats_per_vb } => write!(
                f,
                "leaf combine refused: {n_inputs} leaves totalling {total_in_sats} sats cannot \
                 cover the {fee_sats}-sat sweep fee at {fee_rate_sats_per_vb} sat/vB plus the \
                 {dust_sats}-sat dust floor — the batch costs more than it is worth. Add more \
                 leaves (each additional leaf costs only 57.5 vB) or wait for a lower fee rate"
            ),
            Self::Unpriceable { reason } => {
                write!(f, "leaf combine refused: the sweep could not be priced ({reason})")
            }
        }
    }
}

impl std::error::Error for CombineRefusal {}

/// What the chain says about ONE leaf funding output.
///
/// Split out from the electrum call deliberately: the precondition RULE is then a pure function
/// that can be tested at every boundary without a chain backend, and the only thing left in the
/// I/O layer is producing this struct. `confirmations == 0` covers both "in the mempool" and "not
/// found at all" — electrum reports height 0 for a mempool UTXO, and both answers mean the same
/// thing here (do not build).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeafObservation {
    pub outpoint: String,
    pub confirmations: u32,
    pub unspent: bool,
}

// =================================================================================================
// THE GATE, AS A MODULE BOUNDARY
// =================================================================================================
//
// `CombinePlan`'s docstring used to claim it was "Produced ONLY by `plan_leaf_combine`, so there is
// no way to reach the builder with an unvalidated selection" — while being a `pub struct` with
// every field `pub`. Anyone could write `CombinePlan { outpoints: mine, fee_sats: 0, .. }` and hand
// it to `build_unsigned_sweep`, walking around every refusal in this file. The claim was false, and
// a claim that is only true if nobody writes the obvious literal is not a claim.
//
// `mod linked` (tesr.rs:83, and the comment above it) is this repo's own precedent for the fix, and
// its reasoning applies here verbatim: "Ordering that only a comment defends is not defended."
// There, an accessor is private to a module that exports nothing but constructors which perform a
// structural pin first. Here, two types are private-field members of a module that exports nothing
// but the constructors which validate first:
//
//   * [`CombinePlan`]    — evidence that `plan_leaf_combine` ran and every refusal passed.
//   * [`ColourEvidence`] — evidence that SOMETHING LOOKED at each leaf's colour.
//
// Because their fields are private, neither can be named in a struct literal outside this module —
// a language rule, not a convention — so `plan_leaf_combine` is the only way to obtain a
// `CombinePlan` and a probe is the only way to obtain a `ColourEvidence`. The test-side census
// `a_combine_plan_cannot_be_built_outside_its_validating_constructor` pins the encapsulation
// itself, since a future edit could always widen a visibility again.
mod gated {
    use std::collections::{HashMap, HashSet};

    use mercurylib::transaction::{combine_is_economic, sweep_fee_sats, sweep_tx_vsize, DUST_LIMIT};
    use mercurylib::wallet::Coin;

    use super::{coin_outpoint, CombineRefusal};

    /// What was POSITIVELY ESTABLISHED about ONE leaf's colour. There is deliberately no `Unknown`
    /// variant: "unknown" is the ABSENCE of a verdict, and absence is what the planner refuses on.
    /// A third variant would let a caller record not-knowing and then be tempted to treat it as
    /// benign.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum ColourVerdict {
        /// Looked at, and it carries no RGB allocation and no pending consignment.
        Plain,
        /// Looked at, and it does. A plain sweep would destroy the allocation irrecoverably.
        Coloured,
    }

    /// **FAIL-CLOSED COLOUR EVIDENCE: one verdict per leaf, not a set of positives.**
    ///
    /// The parameter this replaces was `&HashSet<String>` of known-coloured outpoints. A set states
    /// POSITIVES ONLY, so the empty set means "nothing is coloured" — and the empty set is also
    /// precisely what a caller that never gathered any evidence supplies. The two are
    /// indistinguishable to the planner, and only one of them is safe: an RGB allocation is sealed
    /// to `SP.out[j]`, and a plain sweep of it destroys the allocation with no consignment able to
    /// recover it. Value destruction on the default path is fail-open in its purest form.
    ///
    /// **Why an absent-evidence caller cannot satisfy this.** There is no value of this type that
    /// means "nothing is coloured". The planner iterates the SELECTION and demands a verdict for
    /// every outpoint in it (the same rule, for the same reason, as `check_leaf_observations`
    /// iterating the plan rather than the observations). An empty `ColourEvidence` therefore
    /// refuses the FIRST leaf rather than passing all of them; partial evidence refuses the first
    /// leaf nobody looked at. Doing nothing produces a refusal, which is what fail-closed means.
    ///
    /// **And it cannot be assembled out of failures.** The only constructor is [`Self::probe`],
    /// which runs a FALLIBLE lookup per outpoint and propagates the first error — so an RGB engine
    /// that cannot be read yields no evidence object at all, exactly as an unreadable chain backend
    /// yields no `LeafObservation`. Producing the lookup from
    /// `UtexoWallet::unspendable_as_btc_outpoints` (itself `?`-propagating, tokens.rs:1625-1631)
    /// keeps that property end to end.
    ///
    /// **What it does NOT protect against**, stated plainly: a caller that fabricates
    /// `ColourVerdict::Plain` without looking. That is a deliberate false statement rather than an
    /// omission, and no signature can prevent it — the achievable guarantee is that SILENCE is
    /// never read as safety, and that is the guarantee the old parameter did not have.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ColourEvidence {
        verdicts: HashMap<String, ColourVerdict>,
    }

    impl ColourEvidence {
        /// The only constructor. Runs `probe` once per outpoint and keeps the verdicts; the first
        /// probe failure aborts and no evidence is produced.
        pub fn probe<E, F>(
            outpoints: &[String],
            mut probe: F,
        ) -> std::result::Result<Self, E>
        where
            F: FnMut(&str) -> std::result::Result<ColourVerdict, E>,
        {
            let mut verdicts = HashMap::with_capacity(outpoints.len());
            for op in outpoints {
                let v = probe(op)?;
                verdicts.insert(op.clone(), v);
            }
            Ok(Self { verdicts })
        }

        /// The verdict for one outpoint, or `None` — which means NOBODY LOOKED, and which every
        /// reader must treat as a refusal.
        pub fn verdict(&self, outpoint: &str) -> Option<ColourVerdict> {
            self.verdicts.get(outpoint).copied()
        }

        /// How many leaves were looked at. For callers reporting what they gathered.
        pub fn len(&self) -> usize {
            self.verdicts.len()
        }

        pub fn is_empty(&self) -> bool {
            self.verdicts.is_empty()
        }
    }

    /// A validated, priced leaf combine, ready to build.
    ///
    /// Produced ONLY by [`plan_leaf_combine`] — and now that is a fact about the language rather
    /// than about this sentence: every field is private and the type is declared inside a private
    /// module whose only export is the validating constructor, so the struct literal cannot be
    /// written anywhere else. There is no way to reach [`super::build_unsigned_sweep`] with an
    /// unvalidated selection.
    #[derive(Debug, Clone, PartialEq)]
    pub struct CombinePlan {
        outpoints: Vec<String>,
        statechain_ids: Vec<String>,
        total_in_sats: u64,
        vsize: u64,
        fee_sats: u64,
        output_sats: u64,
        fee_rate_sats_per_vb: f64,
    }

    impl CombinePlan {
        /// `"txid:vout"` per input, in selection order. Order is load-bearing: the signing stack
        /// matches inputs back to coins BY OUTPOINT.
        pub fn outpoints(&self) -> &[String] {
            &self.outpoints
        }
        pub fn statechain_ids(&self) -> &[String] {
            &self.statechain_ids
        }
        pub fn total_in_sats(&self) -> u64 {
            self.total_in_sats
        }
        pub fn vsize(&self) -> u64 {
            self.vsize
        }
        pub fn fee_sats(&self) -> u64 {
            self.fee_sats
        }
        /// `total_in_sats - fee_sats` — what the single sweep output pays.
        pub fn output_sats(&self) -> u64 {
            self.output_sats
        }
        pub fn fee_rate_sats_per_vb(&self) -> f64 {
            self.fee_rate_sats_per_vb
        }
    }

    /// **THE BUILD-PATH GATE.** Validate and price a leaf combine.
    ///
    /// `colour` is REQUIRED and is a per-input VERDICT, not a set of positives: that is what makes
    /// the coloured refusal both un-bypassable and fail-closed (audit P0-5's shape — "exclude
    /// token-carrier coins from BTC selection" — moved from a selection helper onto the build
    /// path, and then made to refuse on absence instead of on membership). See [`ColourEvidence`]
    /// for why an absent-evidence caller cannot satisfy it.
    pub fn plan_leaf_combine(
        coins: &[Coin],
        colour: &ColourEvidence,
        fee_rate_sats_per_vb: f64,
    ) -> std::result::Result<CombinePlan, CombineRefusal> {
        if coins.is_empty() {
            return Err(CombineRefusal::NoInputs);
        }

        // ---- 1. STRUCTURE. Every field the PSBT builder `.unwrap()`s, checked by name first ---------
        //
        // `get_unsigned_combine_psbt` unwraps `amount`, `utxo_txid`, `utxo_vout`,
        // `aggregated_pubkey` and `aggregated_address`; `create_and_commit_nonces` unwraps
        // `statechain_id` and `signed_statechain_id`. Reaching either with a `None` is a PANIC in the
        // client, and the second one panics AFTER a nonce has been committed. Refuse by name here,
        // before anything is built.
        let mut outpoints = Vec::with_capacity(coins.len());
        let mut statechain_ids = Vec::with_capacity(coins.len());
        let mut total_in_sats: u64 = 0;
        for c in coins {
            let sid = c.statechain_id.clone().ok_or(CombineRefusal::LeafMissingField {
                statechain_id: "<unidentified leaf>".to_string(),
                field: "statechain_id",
            })?;
            let missing = |field: &'static str| CombineRefusal::LeafMissingField {
                statechain_id: sid.clone(),
                field,
            };
            if c.utxo_txid.is_none() {
                return Err(missing("utxo_txid"));
            }
            if c.utxo_vout.is_none() {
                return Err(missing("utxo_vout"));
            }
            if c.aggregated_address.is_none() {
                return Err(missing("aggregated_address"));
            }
            if c.aggregated_pubkey.is_none() {
                return Err(missing("aggregated_pubkey"));
            }
            let amount = c.amount.ok_or_else(|| missing("amount"))? as u64;
            let op = coin_outpoint(c).ok_or_else(|| missing("utxo_txid"))?;
            outpoints.push(op);
            statechain_ids.push(sid);
            total_in_sats += amount;
        }

        // ---- 2. LIVENESS, AS AN ALLOWLIST -----------------------------------------------------------
        //
        // **THIS LANE SHARES THE WATCHTOWER'S L1 RULE** (`wallet.rs:1856-1872`), verbatim in form and
        // for the same reason. A combine SPENDS the leaf's funding output, so it is exactly as
        // dangerous as a broadcast: run it on a leaf this wallet has already handed to a counterparty
        // and the sender spends the very output the recipient's co-signed material hangs off.
        //
        // ALLOWLIST, NOT DENYLIST, and that is the whole point rather than a style preference. The
        // conveyance lane already made the denylist mistake once — it enumerated WITHDRAWN/WITHDRAWING
        // and was walked straight through by IN_TRANSFER/TRANSFERRED (wallet.rs:1846-1852). Naming the
        // ONE admissible status instead means "a lane added tomorrow parks its coin in SOME
        // non-CONFIRMED status and is refused BY DEFAULT" (wallet.rs:1863-1866). The `match` below is
        // written with an explicit catch-all so the property is structural, not a list to maintain.
        //
        // Placed AFTER the structural pass and BEFORE anything that reads a value: `sid` has to exist
        // to name the leaf in the refusal, and the check-ordering rule this file lives under puts every
        // value check last.
        for (c, sid) in coins.iter().zip(statechain_ids.iter()) {
            // A duplicate-deposit clone shares the statechain id but not the funding output the ladder
            // was built over. `defend_ladders` skips these for the same reason (wallet.rs:1954).
            if c.duplicate_index != 0 {
                return Err(CombineRefusal::LeafIsDuplicate {
                    statechain_id: sid.clone(),
                    duplicate_index: c.duplicate_index,
                });
            }
            match c.status {
                mercurylib::wallet::CoinStatus::CONFIRMED => {}
                ref other => {
                    return Err(CombineRefusal::LeafNotLive {
                        statechain_id: sid.clone(),
                        status: other.to_string(),
                    })
                }
            }
        }

        // ---- 3. NO DUPLICATE INPUT ------------------------------------------------------------------
        let mut seen: HashSet<&str> = HashSet::with_capacity(outpoints.len());
        for op in &outpoints {
            if !seen.insert(op.as_str()) {
                return Err(CombineRefusal::DuplicateInput { outpoint: op.clone() });
            }
        }

        // ---- 4. THE COLOURED REFUSAL — the one that destroys value ----------------------------------
        //
        // ON THE BUILD PATH, keyed on evidence the caller had to supply. Audit P0-5 put the equivalent
        // rule ("exclude token-carrier coins from BTC selection") in a selection helper; a helper is
        // exactly what a caller assembling its own `Vec<Coin>` skips. Here the verdict set is a
        // REQUIRED parameter of the only function that can produce a `CombinePlan`, and the builder
        // takes nothing else.
        //
        // **ITERATE THE SELECTION, NOT THE EVIDENCE** — the same rule `check_leaf_observations` holds
        // for chain observations, and for the same reason. Filtering the selection by membership of a
        // set of positives means every leaf nobody looked at is waved through, and the empty set (what
        // a caller that gathered nothing supplies) waves through ALL of them. Here a missing verdict is
        // `ColourUnknown`, so "nobody looked" and "it is plain" are no longer the same input.
        let mut coloured: Vec<String> = Vec::new();
        let mut first_coloured: Option<usize> = None;
        for (i, (op, sid)) in outpoints.iter().zip(statechain_ids.iter()).enumerate() {
            match colour.verdict(op) {
                None => {
                    return Err(CombineRefusal::ColourUnknown {
                        statechain_id: sid.clone(),
                        outpoint: op.clone(),
                    })
                }
                Some(ColourVerdict::Plain) => {}
                Some(ColourVerdict::Coloured) => {
                    coloured.push(sid.clone());
                    first_coloured.get_or_insert(i);
                }
            }
        }
        if let Some(idx) = first_coloured {
            let plain_count = outpoints.len() - coloured.len();
            // A MIXED selection is a different mistake from an all-coloured one and gets a different
            // answer: the caller has merged two inventories, and neither sweeping (destroys tokens) nor
            // quietly dropping the coloured leaves (sweeps something other than what was asked for) is
            // an acceptable outcome.
            if plain_count > 0 {
                return Err(CombineRefusal::MixedSelection { coloured, plain_count });
            }
            return Err(CombineRefusal::ColouredLeaf {
                statechain_id: statechain_ids[idx].clone(),
                outpoint: outpoints[idx].clone(),
            });
        }

        // ---- 5. THE PRICE, AND WHETHER IT IS WORTH PAYING -------------------------------------------
        let vsize = sweep_tx_vsize(coins.len(), 1)
            .map_err(|e| CombineRefusal::Unpriceable { reason: format!("{e:?}") })?;
        let fee_sats = sweep_fee_sats(coins.len(), 1, fee_rate_sats_per_vb)
            .map_err(|e| CombineRefusal::Unpriceable { reason: format!("{e:?}") })?;
        let economic = combine_is_economic(total_in_sats, coins.len(), fee_rate_sats_per_vb)
            .map_err(|e| CombineRefusal::Unpriceable { reason: format!("{e:?}") })?;
        if !economic {
            return Err(CombineRefusal::BelowSweepCost {
                n_inputs: coins.len(),
                total_in_sats,
                fee_sats,
                dust_sats: DUST_LIMIT,
                fee_rate_sats_per_vb,
            });
        }

        Ok(CombinePlan {
            outpoints,
            statechain_ids,
            total_in_sats,
            vsize,
            fee_sats,
            output_sats: total_in_sats.saturating_sub(fee_sats),
            fee_rate_sats_per_vb,
        })
    }
}

pub use gated::{plan_leaf_combine, ColourEvidence, ColourVerdict, CombinePlan};

/// The chain-side preconditions, as a pure rule over observations.
pub fn check_leaf_observations(
    plan: &CombinePlan,
    observations: &[LeafObservation],
    confirmation_target: u32,
) -> std::result::Result<(), CombineRefusal> {
    // ITERATE THE PLAN, NOT THE OBSERVATIONS. Iterating what the chain returned means every input
    // nobody managed to look at is waved through — the exact silent-degradation shape this repo
    // keeps rediscovering ("failure looks like idle"). An input with no observation is an input
    // this code knows NOTHING about, and the only safe reading of that is a refusal.
    for outpoint in plan.outpoints() {
        let Some(o) = observations.iter().find(|o| &o.outpoint == outpoint) else {
            return Err(CombineRefusal::LeafNotConfirmed {
                outpoint: outpoint.clone(),
                confirmations: 0,
                required: confirmation_target,
            });
        };
        // Spent first: "spent" is the more specific and more alarming answer, and a spent output
        // that also happens to be deeply confirmed must not be reported as merely fine.
        if !o.unspent {
            return Err(CombineRefusal::LeafAlreadySpent { outpoint: outpoint.clone() });
        }
        // A combine spends SP.out[j] as an ORDINARY UTXO, which it only becomes once SP confirms.
        // An SP in the mempool is still replaceable, and replacing it invalidates every
        // co-signature in this batch at once. Electrum reports height 0 for a mempool UTXO, so
        // "0 confirmations" covers both "unconfirmed" and "not found".
        if o.confirmations < confirmation_target {
            return Err(CombineRefusal::LeafNotConfirmed {
                outpoint: outpoint.clone(),
                confirmations: o.confirmations,
                required: confirmation_target,
            });
        }
    }
    Ok(())
}

/// Ask the chain about one leaf funding output. `Err` = **blind** — the backend could not be read,
/// which is never to be turned into a passing [`LeafObservation`].
pub fn observe_leaf(
    electrum: &electrum_client::Client,
    outpoint: &str,
    tip_height: u32,
) -> Result<LeafObservation> {
    use electrum_client::ElectrumApi;
    use std::str::FromStr;

    let (txid_s, vout_s) = outpoint
        .split_once(':')
        .ok_or_else(|| anyhow::anyhow!("malformed leaf outpoint {outpoint:?}"))?;
    let vout: u32 = vout_s
        .parse()
        .map_err(|e| anyhow::anyhow!("malformed leaf outpoint {outpoint:?}: {e}"))?;
    let txid = electrum_client::bitcoin::Txid::from_str(txid_s)
        .map_err(|e| anyhow::anyhow!("malformed leaf txid in {outpoint:?}: {e}"))?;

    let raw = electrum.transaction_get_raw(&txid).map_err(|e| {
        // NOT folded into "0 confirmations, unspent". A backend that cannot answer has told this
        // code nothing, and a combine must not be built on nothing.
        anyhow::anyhow!(
            "chain backend unreadable while fetching leaf funding {outpoint} — cannot tell whether \
             SP is confirmed: {e}"
        )
    })?;
    let tx: electrum_client::bitcoin::Transaction =
        electrum_client::bitcoin::consensus::deserialize(&raw)
            .map_err(|e| anyhow::anyhow!("leaf funding tx {txid_s} did not deserialize: {e}"))?;
    let out = tx.output.get(vout as usize).ok_or_else(|| {
        anyhow::anyhow!("leaf funding {txid_s} has no output {vout} ({} outputs)", tx.output.len())
    })?;

    let listed = electrum.script_list_unspent(&out.script_pubkey).map_err(|e| {
        anyhow::anyhow!(
            "chain backend unreadable while listing unspent outputs of {outpoint} — cannot tell \
             whether the leaf is still spendable: {e}"
        )
    })?;
    let hit = listed
        .iter()
        .find(|u| u.tx_hash.to_string() == txid_s && u.tx_pos as u32 == vout);
    let (unspent, confirmations) = match hit {
        // `height > 0` is load-bearing, exactly as in
        // `transfer_receiver::verify_tx0_output_is_unspent_and_confirmed`: electrum reports height
        // 0 for a MEMPOOL utxo, and `tip - 0 + 1` is a huge number that trivially clears any
        // target, mis-booking an RBF-able mempool SP as confirmed. A combine multiplies that
        // mistake by N.
        Some(u) if u.height > 0 => {
            (true, tip_height.saturating_sub(u.height as u32).saturating_add(1))
        }
        Some(_) => (true, 0),
        None => (false, 0),
    };
    Ok(LeafObservation { outpoint: outpoint.to_string(), confirmations, unspent })
}

// =================================================================================================
// THE BUILDER
// =================================================================================================

/// The unsigned sweep, in the three forms the signing stack needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsignedSweep {
    /// Base64 PSBT, N inputs / 1 output.
    pub psbt_b64: String,
    /// Consensus-serialised unsigned transaction, hex — what
    /// `get_partial_sig_request_for_colored_tx_multi` takes.
    pub tx_hex: String,
    pub txid: String,
}

/// Build the unsigned N-in / 1-out sweep for a validated [`CombinePlan`].
///
/// This is the port of `mercuryrustlib::rgb::create_colored_combine_tx` (rgb.rs:351-449) with the
/// colouring step DELETED — no `rgb.color(...)`, and therefore no opret output and no
/// `colored_payload_vouts` cardinality guard. Everything else, including the multi-input PSBT
/// assembly, is the existing machinery unchanged.
///
/// It takes a `CombinePlan`, which only [`plan_leaf_combine`] can mint, and then checks the coins
/// against it: a builder that accepted any `&[Coin]` alongside any plan would be a second door
/// around every refusal the planner enforces.
pub fn build_unsigned_sweep(
    plan: &CombinePlan,
    coins: &[Coin],
    destination_address: &str,
    network: &str,
) -> Result<UnsignedSweep> {
    use std::str::FromStr;

    // The plan and the coins must describe the SAME transaction. Checked by outpoint AND by total,
    // in plan order, because the signing stack matches inputs back to coins by outpoint and every
    // sighash commits to the whole prevout set — a mismatch would produce N valid signatures over a
    // transaction nobody validated.
    if coins.len() != plan.outpoints().len() {
        return Err(anyhow::anyhow!(
            "leaf combine build refused: the plan covers {} leaf/leaves but {} coin(s) were handed \
             to the builder — build exactly what was validated",
            plan.outpoints().len(),
            coins.len()
        ));
    }
    let mut total: u64 = 0;
    for (i, c) in coins.iter().enumerate() {
        let op = coin_outpoint(c).ok_or_else(|| {
            anyhow::anyhow!("leaf combine build refused: coin {i} has no funding outpoint")
        })?;
        if op != plan.outpoints()[i] {
            return Err(anyhow::anyhow!(
                "leaf combine build refused: coin {i} is {op} but the validated plan names {} at \
                 that position — the selection changed between validation and build",
                plan.outpoints()[i]
            ));
        }
        total += c.amount.ok_or_else(|| {
            anyhow::anyhow!("leaf combine build refused: coin {i} ({op}) has no amount")
        })? as u64;
    }
    if total != plan.total_in_sats() {
        return Err(anyhow::anyhow!(
            "leaf combine build refused: the coins total {total} sats but the validated plan was \
             priced against {} — the fee and the sweep output would both be wrong",
            plan.total_in_sats()
        ));
    }

    // ONE output, paying the plan's arithmetic. `output_sats = total_in - fee`, and the PSBT builder
    // treats `inputs - outputs` as the fee, so this is the fee actually paid.
    //
    // `is_withdrawal = false` is deliberate and is the whole structural point: it selects
    // `LockTime::from_height(0)` (transaction.rs:937-942). A combine is not a ladder tier — it is an
    // ordinary spend of an ordinary confirmed P2TR, and it must be broadcastable NOW. The
    // `is_withdrawal = true` branch would stamp a deposit-anchored decrementing locktime on it, i.e.
    // re-introduce exactly the CSV/locktime delay the feature exists to avoid. Input `nSequence` is
    // 0 in the builder, so there is no relative timelock either.
    let psbt_b64 = mercurylib::transaction::get_unsigned_combine_psbt(
        coins,
        0, // block_height — unused when is_withdrawal is false
        0, // initlock    — unused when is_withdrawal is false
        0, // interval    — unused when is_withdrawal is false
        0, // qt_backup_tx— unused when is_withdrawal is false
        vec![(destination_address.to_string(), plan.output_sats())],
        network.to_string(),
        false,
    )
    .map_err(|e| anyhow::anyhow!("leaf combine build refused: {e:?}"))?;

    let psbt = bitcoin::psbt::Psbt::from_str(&psbt_b64)
        .map_err(|e| anyhow::anyhow!("leaf combine build produced an unparseable psbt: {e}"))?;
    let unsigned_tx = psbt.unsigned_tx.clone();
    let tx_hex = hex::encode(bitcoin::consensus::encode::serialize(&unsigned_tx));
    let txid = unsigned_tx.txid().to_string();

    Ok(UnsignedSweep { psbt_b64, tx_hex, txid })
}

// =================================================================================================
// THE JOURNAL, AND THE WATCHTOWER INTERLOCK
// =================================================================================================

/// **THE INTERLOCK (step 5).** The status a leaf is parked in for the duration of a combine.
///
/// After `SP` confirms, `SP.out[j]` can be spent by EITHER the pre-signed extension tier OR the
/// combine. Both are the owner's own, so the risk is not theft — it is that `defend_ladders`
/// (through `watch_child_pass`, which broadcasts `ext_child`) or `auto_exit_due` (through
/// `unilateral_exit`) evicts ONE leaf and thereby invalidates the WHOLE batch, after every input has
/// been co-signed.
///
/// The interlock adds NOTHING to those passes — step 6 pins that it must not. It reuses their
/// existing L1 LIVENESS ALLOWLIST: both broadcast only for coins whose status is `CONFIRMED`
/// (wallet.rs:1494, 1962, 1919), and that allowlist was written precisely so that "a lane added
/// tomorrow parks its coin in SOME non-CONFIRMED status and is refused BY DEFAULT"
/// (wallet.rs:1857-1866). `WITHDRAWING` is the honest name for what a combine is doing: a signed,
/// not-yet-confirmed spend to an on-chain address.
pub const COMBINE_PARK_STATUS: mercurylib::wallet::CoinStatus =
    mercurylib::wallet::CoinStatus::WITHDRAWING;

/// How far a leaf combine got before it stopped. Mirrors `SplitStage`.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum CombineStage {
    /// Plan written and leaves parked; NOTHING irreversible has happened. Recovery = unpark and
    /// simply run again.
    Planned,
    /// The sweep is co-signed. From here the material is unregenerable (N secnonces are spent) and
    /// recovery must re-broadcast the stored transaction, never re-sign.
    Signed,
    /// The sweep is in the mempool / on chain. Terminal.
    Broadcast,
    /// Deliberately given up before signing; the leaves were unparked. Terminal.
    Abandoned,
}

/// The durable record of ONE leaf combine.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub struct CombineJournalRecord {
    pub op_id: String,
    pub lane: String,
    pub stage: CombineStage,
    pub outpoints: Vec<String>,
    pub statechain_ids: Vec<String>,
    /// Per leaf, the status it held before parking — the only way an abandoned combine can put the
    /// wallet back the way it found it.
    pub status_before_park: Vec<(String, String)>,
    pub destination_address: String,
    pub total_in_sats: u64,
    pub fee_sats: u64,
    pub output_sats: u64,
    pub fee_rate_sats_per_vb: f64,
    pub vsize: u64,
    pub network: String,
    /// The unregenerable material, recorded the moment it exists.
    pub signed_tx: Option<String>,
    pub txid: Option<String>,
}

impl CombineJournalRecord {
    /// The plan is complete, and now it is CHECKED to be — while a refusal still costs nothing.
    /// Past the first `sign_first` this record is the only description of N spent secnonces.
    pub fn validate_plan(&self) -> Result<()> {
        if self.outpoints.is_empty() {
            return Err(anyhow::anyhow!("combine journal: no inputs"));
        }
        if self.outpoints.len() != self.statechain_ids.len() {
            return Err(anyhow::anyhow!(
                "combine journal: {} outpoints but {} statechain ids — the record could not identify \
                 which leaf a spent nonce belonged to",
                self.outpoints.len(),
                self.statechain_ids.len()
            ));
        }
        if self.output_sats.saturating_add(self.fee_sats) != self.total_in_sats {
            return Err(anyhow::anyhow!(
                "combine journal: {} in, {} out, {} fee — the arithmetic does not close",
                self.total_in_sats,
                self.output_sats,
                self.fee_sats
            ));
        }
        if self.output_sats < DUST_LIMIT {
            return Err(anyhow::anyhow!(
                "combine journal: sweep output {} is below the {DUST_LIMIT}-sat dust floor",
                self.output_sats
            ));
        }
        Ok(())
    }
}

/// Write the combine journal. A failed write is FATAL by design, exactly as in `journal_write`
/// (tesr.rs:3604-3624): the next step spends secnonces that can never be re-issued.
pub async fn combine_journal_write(
    cc: &crate::client_config::ClientConfig,
    wallet_name: &str,
    rec: &CombineJournalRecord,
) -> Result<()> {
    let json = serde_json::to_string(rec)?;
    crate::sqlite_manager::insert_raw_backup_txs(
        &cc.pool,
        wallet_name,
        &format!("{COMBINE_JOURNAL_PREFIX}{}", rec.op_id),
        &json,
    )
    .await
    .map_err(|e| {
        anyhow::anyhow!(
            "leaf combine journal write failed at stage {:?} ({e}) — refusing to continue, because \
             the next step spends secnonces that could never be re-issued",
            rec.stage
        )
    })
}

/// Put every parked leaf back the way it was found. Called on any failure BEFORE the point of no
/// return; a leaf left parked is a leaf the watchtower will not defend for a sweep that never
/// happened, so a failure to restore is reported by name rather than swallowed.
async fn restore_parked_statuses(
    cc: &crate::client_config::ClientConfig,
    wallet_name: &str,
    parked: &[(String, mercurylib::wallet::CoinStatus)],
) -> Vec<String> {
    let mut failures = Vec::new();
    for (sid, previous) in parked {
        if let Err(e) =
            crate::transfer_sender::persist_coin_status(cc, wallet_name, sid, previous.clone()).await
        {
            failures.push(format!("{sid} ({e})"));
        }
    }
    failures
}

// =================================================================================================
// THE DRIVER
// =================================================================================================

/// What a completed leaf combine produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CombineOutcome {
    pub txid: String,
    pub signed_tx: String,
    pub outpoints: Vec<String>,
    pub output_sats: u64,
    pub fee_sats: u64,
}

/// **THE LEAF COMBINE.** Validate, price, park, co-sign and broadcast one transaction spending N
/// confirmed, uncoloured leaf outputs into a single consolidated UTXO at `destination_address`.
///
/// The port of `create_colored_combine_tx` (rgb.rs:351-449) minus colouring, wrapped in
/// `in_ladder_split`'s write-ahead discipline (tesr.rs:4291-4367):
/// plan → validate → journal(Planned) → PARK → point of no return → journal(Signed) the instant the
/// co-signature exists → broadcast → journal(Broadcast).
///
/// `colour` must carry a [`ColourVerdict`] for EVERY selected leaf, produced by
/// [`ColourEvidence::probe`] over something that actually queried the RGB engine — in this repo,
/// `UtexoWallet::unspendable_as_btc_outpoints`, which itself fails CLOSED. It is a required
/// argument rather than something this function fetches because the producer lives in the SDK
/// crate, which sits ABOVE this one; and it is a per-input verdict rather than a set of positives
/// so that a caller which gathered nothing is REFUSED rather than told "nothing is coloured".
///
/// **P0-1.** Each input is a distinct `statechain_id` and gets its own fresh secnonce, committed and
/// spent exactly once. Sessions are never batched or reused.
pub async fn combine_leaves(
    cc: &crate::client_config::ClientConfig,
    wallet_name: &str,
    coins: &mut [Coin],
    colour: &ColourEvidence,
    destination_address: &str,
    fee_rate_sats_per_vb: f64,
    confirmation_target: u32,
) -> Result<CombineOutcome> {
    use electrum_client::ElectrumApi;

    let network = cc.network.to_string();

    // ---- 1. VALIDATE AND PRICE. Every refusal, before anything is touched --------------------
    let plan = plan_leaf_combine(coins, colour, fee_rate_sats_per_vb)
        .map_err(|r| anyhow::anyhow!("{r}"))?;

    // ---- 2. THE CHAIN PRECONDITIONS ----------------------------------------------------------
    let tip = cc.electrum_client.block_headers_subscribe_raw()?.height as u32;
    let mut observations = Vec::with_capacity(plan.outpoints().len());
    for op in plan.outpoints() {
        observations.push(observe_leaf(&cc.electrum_client, op, tip)?);
    }
    check_leaf_observations(&plan, &observations, confirmation_target)
        .map_err(|r| anyhow::anyhow!("{r}"))?;

    // ---- 3. WRITE AHEAD ----------------------------------------------------------------------
    let mut journal = CombineJournalRecord {
        op_id: format!("leaf_combine:{}", plan.outpoints().join(",")),
        lane: "leaf_combine".to_string(),
        stage: CombineStage::Planned,
        outpoints: plan.outpoints().to_vec(),
        statechain_ids: plan.statechain_ids().to_vec(),
        status_before_park: vec![],
        destination_address: destination_address.to_string(),
        total_in_sats: plan.total_in_sats(),
        fee_sats: plan.fee_sats(),
        output_sats: plan.output_sats(),
        fee_rate_sats_per_vb: plan.fee_rate_sats_per_vb(),
        vsize: plan.vsize(),
        network: network.clone(),
        signed_tx: None,
        txid: None,
    };
    journal.validate_plan()?;
    combine_journal_write(cc, wallet_name, &journal).await?;

    // ---- 4. THE INTERLOCK: PARK THE LEAVES, DURABLY, BEFORE THE FIRST SIGNATURE ---------------
    //
    // [A2] DURABLE, and BEFORE. `persist_coin_status` writes the wallet back to disk; the tower
    // reads that field from disk on every pass, so a status changed in memory only is a status it
    // never sees — the exact window that let a stale CONFIRMED through on the conveyance lane.
    let mut parked: Vec<(String, mercurylib::wallet::CoinStatus)> = Vec::new();
    for sid in plan.statechain_ids() {
        match crate::transfer_sender::persist_coin_status(cc, wallet_name, sid, COMBINE_PARK_STATUS)
            .await
        {
            Ok(previous) => parked.push((sid.clone(), previous)),
            Err(e) => {
                let failures = restore_parked_statuses(cc, wallet_name, &parked).await;
                return Err(anyhow::anyhow!(
                    "leaf combine refused: leaf {sid} could not be durably parked out of the \
                     watchtower's CONFIRMED allowlist before co-signing ({e}). Proceeding would let \
                     `defend_ladders` broadcast that leaf's extension tier over the very output \
                     this sweep spends, invalidating the whole co-signed batch. Nothing has been \
                     signed.{}",
                    if failures.is_empty() {
                        String::new()
                    } else {
                        format!(
                            " AND these leaves are left parked and undefended: {}",
                            failures.join("; ")
                        )
                    }
                ));
            }
        }
    }
    journal.status_before_park =
        parked.iter().map(|(s, st)| (s.clone(), st.to_string())).collect();

    // Everything from here to the co-signature is staged so a failure can UNPARK.
    let staged = async {
        // ---- 5. NONCES + SE FIRST ROUND, one independent session per statechain_id ------------
        for coin in coins.iter_mut() {
            let coin_nonce = mercurylib::transaction::create_and_commit_nonces(coin)?;
            coin.secret_nonce = Some(coin_nonce.secret_nonce.clone());
            coin.public_nonce = Some(coin_nonce.public_nonce.clone());
            coin.blinding_factor = Some(coin_nonce.blinding_factor.clone());
            let server_public_nonce =
                crate::transaction::sign_first(cc, &coin_nonce.sign_first_request_payload).await?;
            coin.server_public_nonce = Some(server_public_nonce);
        }

        // ---- 6. BUILD -------------------------------------------------------------------------
        let built = build_unsigned_sweep(&plan, coins, destination_address, &network)?;

        // ---- 7. PER-INPUT BLIND MuSig2 --------------------------------------------------------
        //
        // Reused verbatim: `get_partial_sig_request_for_colored_tx_multi` matches each input back to
        // its coin BY OUTPOINT and every input's sighash commits to the whole set via
        // `Prevouts::All` (transaction.rs:998-1010). No new sighash machinery.
        let sessions = mercurylib::transaction::get_partial_sig_request_for_colored_tx_multi(
            coins,
            built.tx_hex.clone(),
            network.clone(),
        )?;
        let mut signatures = Vec::with_capacity(sessions.len());
        for session in sessions.iter() {
            let server_partial_sig =
                crate::transaction::sign_second(cc, &session.partial_signature_request_payload)
                    .await?;
            signatures.push(mercurylib::transaction::create_signature(
                session.msg.clone(),
                session.client_partial_sig.clone(),
                hex::encode(server_partial_sig.serialize()),
                session.encoded_session.clone(),
                session.output_pubkey.clone(),
            )?);
        }
        let signed_tx =
            mercurylib::transaction::new_backup_transaction_multi(built.tx_hex.clone(), signatures)?;
        Ok::<_, anyhow::Error>((signed_tx, built.txid))
    }
    .await;

    // Nothing irreversible happened if we did not get here with a signature: UNPARK.
    let (signed_tx, txid) = match staged {
        Ok(v) => v,
        Err(e) => {
            let failures = restore_parked_statuses(cc, wallet_name, &parked).await;
            journal.stage = CombineStage::Abandoned;
            let _ = combine_journal_write(cc, wallet_name, &journal).await;
            return Err(if failures.is_empty() {
                anyhow::anyhow!(
                    "{e}\n\nNothing was broadcast and every leaf was put back as it was found — the \
                     leaves remain exitable through their pre-signed tiers, which need no further \
                     SE signature."
                )
            } else {
                anyhow::anyhow!(
                    "{e}\n\nAND these leaves could not be un-parked ({}) — they are left \
                     {COMBINE_PARK_STATUS} for a sweep that never happened, so `defend_ladders` \
                     will not drive their chains. Repair the status or exit them.",
                    failures.join("; ")
                )
            });
        }
    };

    // ---- 8. THE POINT OF NO RETURN. N secnonces are spent; record before doing anything else ---
    journal.stage = CombineStage::Signed;
    journal.signed_tx = Some(signed_tx.clone());
    journal.txid = Some(txid.clone());
    combine_journal_write(cc, wallet_name, &journal).await?;

    // ---- 9. BROADCAST -------------------------------------------------------------------------
    let raw = hex::decode(&signed_tx)?;
    cc.electrum_client.transaction_broadcast_raw(&raw).map_err(|e| {
        anyhow::anyhow!(
            "the leaf combine {txid} is CO-SIGNED but could not be broadcast ({e}). It is stored in \
             the `{COMBINE_JOURNAL_PREFIX}` journal at stage Signed — re-broadcast it, never \
             re-sign it. If it is abandoned instead, un-park the leaves: each remains exitable \
             through its own pre-signed extension + state tiers, which need no SE signature."
        )
    })?;

    journal.stage = CombineStage::Broadcast;
    combine_journal_write(cc, wallet_name, &journal).await?;
    // AUDITED-SWALLOW: this one fails toward MORE protection, which is why it is not propagated.
    // The sweep is already in the mempool, so the leaves ARE spent; this write only relabels them
    // WITHDRAWING -> WITHDRAWN for the UI. If it fails they stay parked at COMBINE_PARK_STATUS,
    // which is still outside the watchtower's CONFIRMED allowlist — the tower stands down either
    // way, and it must, because driving a spent leaf's tier would be broadcasting a tx whose input
    // no longer exists. Returning `Err` here, by contrast, would report a FAILED combine that in
    // fact succeeded, and the caller's retry would rebuild a sweep over spent outputs.
    for sid in plan.statechain_ids() {
        let _ = crate::transfer_sender::persist_coin_status(
            cc,
            wallet_name,
            sid,
            mercurylib::wallet::CoinStatus::WITHDRAWN,
        )
        .await;
    }

    Ok(CombineOutcome {
        txid,
        signed_tx,
        outpoints: plan.outpoints().to_vec(),
        output_sats: plan.output_sats(),
        fee_sats: plan.fee_sats(),
    })
}

// =================================================================================================
// TESTS
// =================================================================================================

#[cfg(test)]
mod combine_refusal_tests {
    use super::*;
    use mercurylib::transaction::combine_is_economic;
    use mercurylib::wallet::CoinStatus;
    use std::collections::HashSet;

    const TXID_A: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const TXID_B: &str = "2222222222222222222222222222222222222222222222222222222222222222";
    const TXID_C: &str = "3333333333333333333333333333333333333333333333333333333333333333";

    /// A regtest P2TR address, derived so it cannot drift from its key.
    fn agg_address() -> String {
        use bitcoin::{Address, Network};
        use secp256k1_zkp::{PublicKey, Secp256k1};
        use std::str::FromStr;
        let pk = PublicKey::from_str(
            "0250929b74c1a04954b78b4b6035e97a5e078a5a0f28ec96d547bfee9ace803ac0",
        )
        .unwrap();
        Address::p2tr(&Secp256k1::new(), pk.x_only_public_key().0, None, Network::Regtest).to_string()
    }

    /// A leaf `Coin` exactly as `transfer_receiver.rs:1500-1513` leaves it after adoption:
    /// `utxo_txid` = SP.txid, `utxo_vout` = cb.sp_vout, `amount` = SP.out[j].value, its own
    /// `aggregated_address`, `locktime: None`, `status: CONFIRMED`.
    fn leaf(txid: &str, vout: u32, amount: u32) -> Coin {
        Coin {
            index: 0,
            user_privkey: String::new(),
            user_pubkey: String::new(),
            auth_privkey: String::new(),
            auth_pubkey: String::new(),
            derivation_path: String::new(),
            fingerprint: String::new(),
            address: String::new(),
            backup_address: String::new(),
            server_pubkey: None,
            aggregated_pubkey: Some(
                "0250929b74c1a04954b78b4b6035e97a5e078a5a0f28ec96d547bfee9ace803ac0".to_string(),
            ),
            aggregated_address: Some(agg_address()),
            utxo_txid: Some(txid.to_string()),
            utxo_vout: Some(vout),
            amount: Some(amount),
            statechain_id: Some(format!("sid-{}-{vout}", &txid[..4])),
            signed_statechain_id: None,
            locktime: None,
            secret_nonce: None,
            public_nonce: None,
            blinding_factor: None,
            server_public_nonce: None,
            tx_cpfp: None,
            tx_withdraw: None,
            withdrawal_address: None,
            status: CoinStatus::CONFIRMED,
            duplicate_index: 0,
            single_use: false,
            epoch_deadline: None,
        }
    }

    /// NO colour evidence at all — what a caller that never looked supplies. Under the old
    /// `&HashSet` parameter this was indistinguishable from "I looked, nothing is coloured".
    fn none() -> ColourEvidence {
        ColourEvidence::probe::<anyhow::Error, _>(&[], |_| Ok(ColourVerdict::Plain))
            .expect("an empty probe cannot fail")
    }

    /// Colour evidence saying "every one of these outpoints was LOOKED AT and is plain".
    fn plain(outpoints: &[String]) -> ColourEvidence {
        ColourEvidence::probe::<anyhow::Error, _>(outpoints, |_| Ok(ColourVerdict::Plain))
            .expect("a total probe cannot fail")
    }

    /// Colour evidence over `outpoints` in which `carriers` are the ones that came back COLOURED —
    /// the shape a real caller gets from `UtexoWallet::unspendable_as_btc_outpoints`.
    fn verdicts(outpoints: &[String], carriers: &HashSet<String>) -> ColourEvidence {
        ColourEvidence::probe::<anyhow::Error, _>(outpoints, |op| {
            Ok(if carriers.contains(op) { ColourVerdict::Coloured } else { ColourVerdict::Plain })
        })
        .expect("a total probe cannot fail")
    }

    /// Every outpoint of a selection, for the helpers above.
    fn ops(coins: &[Coin]) -> Vec<String> {
        coins.iter().filter_map(coin_outpoint).collect()
    }

    fn obs(op: &str, confs: u32, unspent: bool) -> LeafObservation {
        LeafObservation { outpoint: op.to_string(), confirmations: confs, unspent }
    }

    // ---- (b) THE LIVENESS ALLOWLIST -------------------------------------------------------------

    /// Every `CoinStatus` this wallet has, so the tests below cannot silently stop covering a
    /// status somebody adds. The compiler enforces the completeness: adding a variant to
    /// `CoinStatus` without extending this list is a non-exhaustive-match error here.
    fn every_status() -> Vec<CoinStatus> {
        let all = vec![
            CoinStatus::INITIALISED,
            CoinStatus::IN_MEMPOOL,
            CoinStatus::UNCONFIRMED,
            CoinStatus::CONFIRMED,
            CoinStatus::IN_TRANSFER,
            CoinStatus::WITHDRAWING,
            CoinStatus::TRANSFERRED,
            CoinStatus::WITHDRAWN,
            CoinStatus::DUPLICATED,
            CoinStatus::INVALIDATED,
        ];
        // The exhaustiveness proof. A new `CoinStatus` variant makes this match fail to compile,
        // which is the only way a census like this stays a census.
        for s in &all {
            match s {
                CoinStatus::INITIALISED
                | CoinStatus::IN_MEMPOOL
                | CoinStatus::UNCONFIRMED
                | CoinStatus::CONFIRMED
                | CoinStatus::IN_TRANSFER
                | CoinStatus::WITHDRAWING
                | CoinStatus::TRANSFERRED
                | CoinStatus::WITHDRAWN
                | CoinStatus::DUPLICATED
                | CoinStatus::INVALIDATED => {}
            }
        }
        all
    }

    /// **BLOCKER 1 — THE WALLET WOULD HAVE SWEPT A COIN IT HAD ALREADY GIVEN AWAY.** The planner
    /// read six fields off each leaf and never once read `status`, so an IN_TRANSFER, TRANSFERRED
    /// or WITHDRAWN leaf planned and swept exactly like a live one: the sender spends the funding
    /// output the recipient's co-signed material hangs off.
    ///
    /// Refused for EVERY non-CONFIRMED status, not for a denylist of the three that are lethal
    /// today — the rule and its rationale are `wallet.rs:1856-1872`, and this lane now shares it.
    #[test]
    fn every_non_confirmed_leaf_status_is_refused() {
        for status in every_status() {
            if status == CoinStatus::CONFIRMED {
                continue;
            }
            let mut c = leaf(TXID_A, 0, 50_000);
            c.status = status.clone();
            let r = plan_leaf_combine(&[c], &plain(&[format!("{TXID_A}:0")]), 20.0);
            match r {
                Err(CombineRefusal::LeafNotLive { status: got, .. }) => {
                    assert_eq!(got, status.to_string(), "the refusal must name the status it saw");
                }
                other => panic!(
                    "a {status} leaf is NOT provably this wallet's own unspent coin and must be \
                     refused BY NAME — sweeping it spends an output a counterparty may already \
                     hold co-signed material over; got {other:?}"
                ),
            }
        }
    }

    /// The allowlist is discriminating, not universal: CONFIRMED — and only CONFIRMED — plans.
    #[test]
    fn a_confirmed_leaf_is_accepted() {
        let c = leaf(TXID_A, 0, 50_000);
        assert_eq!(c.status, CoinStatus::CONFIRMED);
        let accepted = every_status()
            .into_iter()
            .filter(|status| {
                let mut c = leaf(TXID_A, 0, 50_000);
                c.status = status.clone();
                plan_leaf_combine(&[c], &plain(&[format!("{TXID_A}:0")]), 20.0).is_ok()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            accepted,
            vec![CoinStatus::CONFIRMED],
            "exactly one status may plan a combine, and it is CONFIRMED"
        );
    }

    /// A duplicate-deposit clone shares the statechain id but not the funding output the ladder was
    /// built over. `defend_ladders` skips `duplicate_index != 0` for the same reason
    /// (wallet.rs:1954); the planner ignored it entirely.
    #[test]
    fn a_duplicate_deposit_clone_is_refused() {
        let mut c = leaf(TXID_A, 0, 50_000);
        c.duplicate_index = 3;
        let r = plan_leaf_combine(&[c], &plain(&[format!("{TXID_A}:0")]), 20.0);
        assert!(
            matches!(r, Err(CombineRefusal::LeafIsDuplicate { duplicate_index: 3, .. })),
            "a duplicate-deposit clone must be refused by name, got {r:?}"
        );
    }

    /// **THE FORM IS THE POINT, NOT THE LIST.** `wallet.rs:1857-1866` spells out why: "a lane added
    /// tomorrow parks its coin in SOME non-CONFIRMED status and is refused BY DEFAULT. It cannot
    /// re-arm this tower against its own recipient by forgetting to extend a list." A denylist here
    /// would have to be extended by every future lane, and the one that forgets is a theft.
    ///
    /// Asserted on this file's own source: the gate must be a `match` whose only accepting arm is
    /// `CONFIRMED` and whose catch-all REFUSES. `every_non_confirmed_leaf_status_is_refused` proves
    /// the behaviour for today's ten statuses; this proves the SHAPE, which is what covers the
    /// eleventh.
    #[test]
    fn the_liveness_gate_is_an_allowlist_not_a_denylist() {
        let src = include_str!("combine.rs");
        let at = src.find("\n    pub fn plan_leaf_combine(").expect("the planner");
        let body = &src[at..at + src[at..].find("\n    }\n}\n").expect("the planner ends")];
        let body: String = body
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            body.contains("CoinStatus::CONFIRMED => {}"),
            "the gate must ACCEPT by naming CONFIRMED, so everything else falls to the catch-all"
        );
        assert!(
            body.contains("LeafNotLive"),
            "…and the catch-all must be the refusal"
        );
        for denylisted in ["CoinStatus::WITHDRAWN =>", "CoinStatus::IN_TRANSFER =>", "!= CoinStatus"]
        {
            assert!(
                !body.contains(denylisted),
                "`{denylisted}` is DENYLIST shape. Enumerating the bad statuses is the mistake \
                 wallet.rs:1846-1852 records having already been made once on the conveyance lane \
                 — a lane added tomorrow is admitted by default. Allowlist only."
            );
        }
    }

    // ---- (c) THE COLOURED REFUSAL ---------------------------------------------------------------

    /// **THE ONE THAT DESTROYS VALUE.** An RGB allocation is sealed to `SP.out[j]`. A plain sweep
    /// spends that outpoint with no RGB transition, so the allocation is gone — not stuck, GONE,
    /// with no consignment that can recover it. This refusal is on the BUILD path (the planner every
    /// builder must go through), not in a selection helper, precisely because a lower-level caller
    /// assembling its own `Vec<Coin>` would otherwise skip it.
    ///
    /// The evidence is built the way a real caller builds it — probe every selected outpoint
    /// against the carrier set `UtexoWallet::unspendable_as_btc_outpoints` returns — so this test
    /// exercises the whole path from "the RGB engine says this outpoint holds an allocation" to the
    /// refusal, not just the membership test in the middle.
    #[test]
    fn a_coloured_leaf_is_refused_on_the_build_path() {
        let coins = [leaf(TXID_A, 0, 50_000)];
        let carriers: HashSet<String> = [format!("{TXID_A}:0")].into_iter().collect();
        let r = plan_leaf_combine(&coins, &verdicts(&ops(&coins), &carriers), 20.0);
        match r {
            Err(CombineRefusal::ColouredLeaf { outpoint, .. }) => {
                assert_eq!(outpoint, format!("{TXID_A}:0"), "the refusal must name the leaf");
            }
            other => panic!(
                "a leaf the RGB engine reports as a carrier must be refused BY NAME, got {other:?}"
            ),
        }
    }

    /// **BLOCKER 2 — THE REFUSAL WAS FAIL-OPEN, ON THE ONE PATH THAT DESTROYS VALUE.** The planner
    /// took a `HashSet` of KNOWN-COLOURED outpoints. A set is a statement about positives only, so
    /// an empty one meant "nothing is coloured" — which is also exactly what a caller that never
    /// gathered any evidence supplies. The two are indistinguishable, and only one of them is safe:
    /// a plain sweep of a coloured leaf destroys the RGB allocation with no consignment able to
    /// recover it.
    ///
    /// It is now a REQUIRED PER-INPUT VERDICT, and the planner iterates the SELECTION demanding one
    /// — the same shape as `check_leaf_observations`, for the same reason. There is no longer any
    /// value of the parameter that means "nothing is coloured": the only way to pass is N explicit
    /// `Plain` verdicts, each of which had to come out of a probe. Absence is now a refusal.
    #[test]
    fn a_leaf_whose_colour_was_never_established_is_refused_not_swept() {
        let coins = [leaf(TXID_A, 0, 50_000)];
        let r = plan_leaf_combine(&coins, &none(), 20.0);
        match r {
            Err(CombineRefusal::ColourUnknown { outpoint, .. }) => {
                assert_eq!(outpoint, format!("{TXID_A}:0"), "the refusal must name the leaf");
            }
            other => panic!(
                "a caller that gathered NO colour evidence must be REFUSED. Reading 'no positives' \
                 as 'everything is plain' is the fail-open shape, and here it sweeps a coloured \
                 leaf and destroys the allocation irrecoverably; got {other:?}"
            ),
        }
    }

    /// The same fail-open shape one input at a time: evidence covering SOME of the selection is
    /// evidence covering none of it, as far as the leaves nobody looked at are concerned. This is
    /// the identical rule `a_leaf_with_no_observation_at_all_is_refused_not_waved_through` enforces
    /// for chain observations — iterate the selection, not the evidence.
    #[test]
    fn colour_evidence_that_covers_only_part_of_the_selection_is_refused() {
        let coins = [leaf(TXID_A, 0, 50_000), leaf(TXID_B, 1, 50_000)];
        // Only leaf A was looked at.
        let partial = plain(&[format!("{TXID_A}:0")]);
        let r = plan_leaf_combine(&coins, &partial, 20.0);
        match r {
            Err(CombineRefusal::ColourUnknown { outpoint, .. }) => {
                assert_eq!(outpoint, format!("{TXID_B}:1"), "the UNLOOKED-AT leaf must be named");
            }
            other => panic!(
                "an input with no colour verdict must be refused, not inherit its neighbours' \
                 verdicts; got {other:?}"
            ),
        }
    }

    /// **A PROBE THAT COULD NOT ANSWER NEVER BECOMES A `Plain`.** `ColourEvidence` is only
    /// constructible by running a FALLIBLE probe over every outpoint, so an RGB engine that cannot
    /// be read propagates out as an `Err` and no evidence object exists at all — there is no path
    /// on which a failed lookup is silently recorded as "not coloured". This is the same discipline
    /// `observe_leaf` holds for the chain backend.
    #[test]
    fn a_colour_probe_that_fails_yields_no_evidence_at_all() {
        let outpoints = vec![format!("{TXID_A}:0"), format!("{TXID_B}:1")];
        let mut seen = 0;
        let r = ColourEvidence::probe(&outpoints, |_op| {
            seen += 1;
            Err::<ColourVerdict, anyhow::Error>(anyhow::anyhow!("rgb engine unreadable"))
        });
        assert!(
            r.is_err(),
            "a colour source that could not be read must abort the whole evidence set — there is \
             no path on which a failed lookup is recorded as `Plain`"
        );
        assert_eq!(seen, 1, "and it must abort at the FIRST failure, not carry on gathering");
    }

    /// The producer named in the refusal text really is fail-closed, and really does speak the same
    /// `"txid:vout"` key space this planner looks leaves up by — so a correct caller's evidence
    /// actually lands on the right entries.
    ///
    /// This is asserted on `tokens.rs`'s own source because the producer lives in the SDK crate,
    /// which sits ABOVE this one and cannot be called from here. That layering is the reason the
    /// verdict is a parameter at all; pinning the contract across the boundary is what keeps the
    /// parameter honest. Weakening `unspendable_as_btc_outpoints` into something best-effort turns
    /// THIS red.
    #[test]
    fn the_named_evidence_producer_is_itself_fail_closed() {
        let src = include_str!("../../rust-sdk/src/tokens.rs");
        let cut = |marker: &str| -> String {
            let at = src.find(marker).unwrap_or_else(|| panic!("{marker} not found in tokens.rs"));
            src[at..at + src[at..].find("\n    }\n").expect("fn ends")].to_string()
        };
        let producer = cut("\n    pub(crate) async fn unspendable_as_btc_outpoints(");
        assert!(
            producer.contains("token_carrier_outpoints().await?")
                && producer.contains("consignment_bearing_outpoints().await?"),
            "the producer must PROPAGATE a failure from either half with `?`. A best-effort union \
             (`.unwrap_or_default()`, `if let Ok`) would hand this planner a confident 'nothing is \
             coloured' assembled out of not knowing — which the per-input verdict cannot detect, \
             because a wrong verdict is still a verdict"
        );
        // **THE KEY SPACES MUST AGREE**, or correct evidence lands on no entry and the planner
        // refuses everything (loud, survivable) — or worse, a caller "fixes" that by loosening the
        // lookup. Both halves of the producer are pinned to `"<txid>:<vout>"`, which is exactly
        // what `coin_outpoint` above formats:
        //   * the PENDING half calls `coin_outpoint` itself, so it agrees by construction;
        //   * the BOOKED half comes from rgb-lib via `RgbWallet::list_allocations`.
        let pending = cut("\n    pub(crate) async fn consignment_bearing_outpoints(");
        assert!(
            pending.contains("crate::wallet::coin_outpoint(coin)"),
            "the pending half must key on `coin_outpoint`, the same formatter this planner uses"
        );
        let rgb = include_str!("../../rust-rgb/src/lib.rs");
        let at = rgb.find("pub fn list_allocations(").expect("list_allocations");
        let body = &rgb[at..at + rgb[at..].find("\n    }\n").expect("fn ends")];
        assert!(
            body.contains("format!(\"{}:{}\", u.utxo.outpoint.txid, u.utxo.outpoint.vout)"),
            "the booked half must emit `<txid>:<vout>` — the same key space as `coin_outpoint` — or \
             a leaf that really is coloured never matches its own verdict entry"
        );
    }

    /// A mixed selection gets its OWN refusal. Dropping the coloured leaves and sweeping the rest
    /// would be a silent change to what the caller asked for; refusing the coloured ones one at a
    /// time would hide that the caller has mixed two inventories.
    #[test]
    fn a_mixed_coloured_and_plain_selection_is_refused_as_mixed() {
        let coins = [leaf(TXID_A, 0, 50_000), leaf(TXID_B, 1, 50_000), leaf(TXID_C, 2, 50_000)];
        let carriers: HashSet<String> = [format!("{TXID_B}:1")].into_iter().collect();
        let r = plan_leaf_combine(&coins, &verdicts(&ops(&coins), &carriers), 20.0);
        match r {
            Err(CombineRefusal::MixedSelection { coloured: c, plain_count }) => {
                assert_eq!(c.len(), 1);
                assert_eq!(plain_count, 2, "the other two leaves are plain");
            }
            other => panic!("a mixed selection must be refused AS MIXED, got {other:?}"),
        }
    }

    // ---- (d) PRECONDITIONS ----------------------------------------------------------------------

    /// Bitcoin rejects a transaction that spends one output twice — but only after the SE has
    /// co-signed every input, and those signatures are not free.
    #[test]
    fn a_duplicated_input_is_refused() {
        let coins = [leaf(TXID_A, 0, 50_000), leaf(TXID_A, 0, 50_000)];
        let r = plan_leaf_combine(&coins, &plain(&ops(&coins)), 20.0);
        assert!(
            matches!(r, Err(CombineRefusal::DuplicateInput { .. })),
            "the same outpoint twice must be refused, got {r:?}"
        );
    }

    /// An SP still in the mempool is REPLACEABLE. Building on it risks every co-signature in the
    /// batch. Electrum reports height 0 for a mempool UTXO, so 0 confirmations covers both "in the
    /// mempool" and "not found".
    #[test]
    fn a_leaf_whose_sp_is_not_confirmed_is_refused() {
        let coins = [leaf(TXID_A, 0, 50_000), leaf(TXID_B, 1, 50_000)];
        let plan = plan_leaf_combine(&coins, &plain(&ops(&coins)), 20.0).expect("plan");
        let o = [obs(&format!("{TXID_A}:0"), 3, true), obs(&format!("{TXID_B}:1"), 0, true)];
        let r = check_leaf_observations(&plan, &o, 1);
        assert!(
            matches!(r, Err(CombineRefusal::LeafNotConfirmed { confirmations: 0, .. })),
            "a 0-conf (mempool) SP must be refused, got {r:?}"
        );
    }

    /// …and below the wallet's own target, not merely below one.
    #[test]
    fn a_leaf_below_the_confirmation_target_is_refused() {
        let coins = [leaf(TXID_A, 0, 50_000)];
        let plan = plan_leaf_combine(&coins, &plain(&ops(&coins)), 20.0).expect("plan");
        let o = [obs(&format!("{TXID_A}:0"), 2, true)];
        assert!(
            matches!(
                check_leaf_observations(&plan, &o, 3),
                Err(CombineRefusal::LeafNotConfirmed { confirmations: 2, required: 3, .. })
            ),
            "2 confirmations against a target of 3 must be refused"
        );
        assert!(
            check_leaf_observations(&plan, &o, 2).is_ok(),
            "exactly at the target must be accepted"
        );
    }

    /// A leaf whose output the extension tier (or a previous combine, or a rival) already took.
    #[test]
    fn an_already_spent_leaf_is_refused() {
        let coins = [leaf(TXID_A, 0, 50_000)];
        let plan = plan_leaf_combine(&coins, &plain(&ops(&coins)), 20.0).expect("plan");
        let o = [obs(&format!("{TXID_A}:0"), 6, false)];
        assert!(
            matches!(check_leaf_observations(&plan, &o, 1), Err(CombineRefusal::LeafAlreadySpent { .. })),
            "a spent leaf output must be refused"
        );
    }

    /// **AN ABSENT OBSERVATION IS NOT A PASSING ONE.** This is the silent-degradation shape this
    /// repo has been bitten by repeatedly: a checker that iterates the observations it was given
    /// waves through every input nobody looked at. Iterate the PLAN and demand an observation per
    /// outpoint.
    #[test]
    fn a_leaf_with_no_observation_at_all_is_refused_not_waved_through() {
        let coins = [leaf(TXID_A, 0, 50_000), leaf(TXID_B, 1, 50_000)];
        let plan = plan_leaf_combine(&coins, &plain(&ops(&coins)), 20.0).expect("plan");
        let o = [obs(&format!("{TXID_A}:0"), 6, true)]; // B was never looked at
        let r = check_leaf_observations(&plan, &o, 1);
        assert!(
            r.is_err(),
            "an input with NO chain observation must be refused, not treated as confirmed-and-unspent \
             — 'nobody looked' is the failure mode that looks exactly like 'everything is fine'; got {r:?}"
        );
    }

    /// A leaf missing any of the four fields the PSBT builder reads. `get_unsigned_combine_psbt`
    /// `.unwrap()`s `amount`, `utxo_txid`, `utxo_vout` and `aggregated_address`, so this refusal is
    /// the difference between a named error and a panic in the client.
    #[test]
    fn a_leaf_missing_a_psbt_field_is_refused_by_name_not_by_panic() {
        for (field, mutate) in [
            ("utxo_txid", (|c: &mut Coin| c.utxo_txid = None) as fn(&mut Coin)),
            ("utxo_vout", |c: &mut Coin| c.utxo_vout = None),
            ("amount", |c: &mut Coin| c.amount = None),
            ("aggregated_address", |c: &mut Coin| c.aggregated_address = None),
            ("statechain_id", |c: &mut Coin| c.statechain_id = None),
        ] {
            let mut c = leaf(TXID_A, 0, 50_000);
            mutate(&mut c);
            let r = plan_leaf_combine(&[c], &none(), 20.0);
            assert!(
                matches!(r, Err(CombineRefusal::LeafMissingField { .. })),
                "a leaf with no `{field}` must be refused by name, got {r:?}"
            );
        }
    }

    #[test]
    fn an_empty_selection_is_refused() {
        assert!(matches!(plan_leaf_combine(&[], &none(), 20.0), Err(CombineRefusal::NoInputs)));
    }

    // ---- (a) THE ECONOMIC REFUSAL ---------------------------------------------------------------

    /// The typed economic refusal, following `SdkError::CoinBelowMaintenanceCost`: a batch that
    /// cannot pay for its own sweep is refused with the arithmetic attached, so the caller can see
    /// how many more leaves (or how much lower a fee rate) would fix it.
    #[test]
    fn a_batch_that_cannot_cover_its_sweep_fee_plus_dust_is_refused() {
        // 2 inputs at 20 sat/vB: vsize 169 -> fee 3380. Total must reach 3380 + 330 = 3710.
        let coins = [leaf(TXID_A, 0, 1_800), leaf(TXID_B, 1, 1_800)]; // 3600 < 3710
        match plan_leaf_combine(&coins, &plain(&ops(&coins)), 20.0) {
            Err(CombineRefusal::BelowSweepCost { n_inputs, total_in_sats, fee_sats, dust_sats, .. }) => {
                assert_eq!((n_inputs, total_in_sats, fee_sats, dust_sats), (2, 3_600, 3_380, 330));
            }
            other => panic!("an uneconomic batch must be refused with its arithmetic, got {other:?}"),
        }
        // One sat more than the floor: economic, and the plan carries the sweep output.
        let coins = [leaf(TXID_A, 0, 1_855), leaf(TXID_B, 1, 1_856)]; // 3711
        let plan = plan_leaf_combine(&coins, &plain(&ops(&coins)), 20.0).expect("3711 sats clears 3380 + 330");
        assert_eq!(plan.fee_sats(), 3_380);
        assert_eq!(plan.output_sats(), 331);
        assert!(plan.output_sats() >= DUST_LIMIT);
    }

    /// The whole point, in one assertion: 60 tiny leaves that NONE of which could pay for its own
    /// 250-vB tail exit are collectively worth sweeping.
    #[test]
    fn sixty_leaves_too_small_to_exit_alone_are_collectively_economic() {
        let coins: Vec<Coin> = (0..60u32).map(|i| leaf(TXID_A, i, 2_500)).collect();
        let plan = plan_leaf_combine(&coins, &plain(&ops(&coins)), 20.0).expect("60 x 2500 = 150 000 sats");
        assert_eq!(plan.vsize(), 3_519);
        assert_eq!(plan.fee_sats(), 70_380);
        assert_eq!(plan.total_in_sats(), 150_000);
        assert_eq!(plan.output_sats(), 79_620);
        // …and the same leaf on its own is not worth moving: a 250-vB tail at 20 sat/vB is 5 000
        // sats against a 2 500-sat leaf.
        assert!(
            !combine_is_economic(2_500, 1, 20.0).unwrap(),
            "one 2 500-sat leaf cannot pay for its own sweep at 20 sat/vB"
        );
    }

    /// The happy path, spelled out, so the refusals above are known to be discriminating rather
    /// than universal.
    #[test]
    fn a_clean_plain_multi_tree_selection_is_accepted() {
        // Deliberately from THREE DIFFERENT trees: after confirmation these are just UTXOs, and
        // nothing about a combine reads which SP produced them.
        let coins = [leaf(TXID_A, 0, 50_000), leaf(TXID_B, 3, 20_000), leaf(TXID_C, 7, 10_000)];
        let plan = plan_leaf_combine(&coins, &plain(&ops(&coins)), 5.0).expect("a clean selection must plan");
        assert_eq!(plan.outpoints().len(), 3);
        assert_eq!(plan.total_in_sats(), 80_000);
        let o: Vec<LeafObservation> = plan.outpoints().iter().map(|p| obs(p, 6, true)).collect();
        assert!(check_leaf_observations(&plan, &o, 1).is_ok());
    }

    // ---- STEP 4: THE BUILDER --------------------------------------------------------------------

    /// **THE STRUCTURAL FACT, ASSERTED ON THE BYTES.** The whole feature rests on `SP.out[j]` being
    /// an ordinary P2TR the owner may spend with a fresh, TIMELOCK-FREE co-signature. If the sweep
    /// this builder emits carried an absolute locktime (`nLockTime`) or a relative one
    /// (`nSequence`), the "no CSV at all, ~15 days faster" claim would be false and the combine
    /// would be just another tier.
    #[test]
    fn the_built_sweep_carries_no_absolute_and_no_relative_timelock() {
        use bitcoin::consensus::deserialize;
        let coins = [leaf(TXID_A, 0, 50_000), leaf(TXID_B, 1, 50_000)];
        let plan = plan_leaf_combine(&coins, &plain(&ops(&coins)), 20.0).expect("plan");
        let built = build_unsigned_sweep(&plan, &coins, &agg_address(), "regtest").expect("build");
        let tx: bitcoin::Transaction = deserialize(&hex::decode(&built.tx_hex).unwrap()).unwrap();
        assert_eq!(tx.lock_time.to_consensus_u32(), 0, "a sweep must be broadcastable NOW");
        for i in tx.input.iter() {
            assert_eq!(i.sequence.0, 0, "a sweep input must carry no relative timelock");
        }
    }

    /// One input per leaf, IN PLAN ORDER, and exactly one output paying the plan's arithmetic to the
    /// caller's address. Order is load-bearing: `get_partial_sig_request_for_colored_tx_multi`
    /// matches inputs back to coins BY OUTPOINT and every sighash commits to the whole set via
    /// `Prevouts::All`, so a builder that reordered or dropped an input would produce N signatures
    /// over a transaction that is not the one it planned.
    #[test]
    fn the_built_sweep_has_one_input_per_leaf_in_plan_order_and_one_output() {
        use bitcoin::consensus::deserialize;
        let coins = [leaf(TXID_A, 0, 50_000), leaf(TXID_B, 3, 20_000), leaf(TXID_C, 7, 10_000)];
        let plan = plan_leaf_combine(&coins, &plain(&ops(&coins)), 5.0).expect("plan");
        let built = build_unsigned_sweep(&plan, &coins, &agg_address(), "regtest").expect("build");
        let tx: bitcoin::Transaction = deserialize(&hex::decode(&built.tx_hex).unwrap()).unwrap();
        let got: Vec<String> = tx
            .input
            .iter()
            .map(|i| format!("{}:{}", i.previous_output.txid, i.previous_output.vout))
            .collect();
        assert_eq!(got, plan.outpoints(), "inputs must be the planned outpoints, in order");
        assert_eq!(tx.output.len(), 1, "V1 sweeps to ONE consolidated output");
        assert_eq!(tx.output[0].value, plan.output_sats());
        assert_eq!(plan.output_sats(), plan.total_in_sats() - plan.fee_sats());
    }

    /// **THE FEE MODEL, PINNED AGAINST THE ACTUAL BUILDER.** A vsize model that agrees only with its
    /// own arithmetic is a model that can drift from the transaction it prices. Serialise what the
    /// builder really emits and check its BASE bytes against the model's base term — at N = 1, 2 and
    /// 60, the three sizes the feature is specified at. (An unsigned tx has no witnesses, so
    /// consensus serialisation here is exactly the base size.)
    #[test]
    fn the_builders_real_serialised_size_agrees_with_the_fee_model() {
        for n in [1usize, 2, 60] {
            let coins: Vec<Coin> = (0..n as u32).map(|i| leaf(TXID_A, i, 5_000)).collect();
            let plan = plan_leaf_combine(&coins, &plain(&ops(&coins)), 5.0).expect("plan");
            let built = build_unsigned_sweep(&plan, &coins, &agg_address(), "regtest").expect("build");
            let base = hex::decode(&built.tx_hex).unwrap().len() as u64;
            // base = 4 (version) + varint(n) + 41n + varint(1) + 43 + 4 (locktime)
            let varint = if n < 253 { 1 } else { 3 };
            assert_eq!(base, 4 + varint + 41 * n as u64 + 1 + 43 + 4, "base bytes at n={n}");
            // …and the model's vsize is that base, plus the segwit witness it will carry.
            let weight = 4 * base + 2 + 67 * n as u64;
            assert_eq!(plan.vsize(), weight.div_ceil(4), "model vsize at n={n}");
        }
    }

    /// The builder is not a second, laxer door. It takes a `CombinePlan` — which only
    /// [`plan_leaf_combine`] can mint — and REFUSES a coin set that is not the one planned, so a
    /// caller cannot validate one selection and build another.
    #[test]
    fn the_builder_refuses_coins_that_are_not_the_planned_ones() {
        let planned = [leaf(TXID_A, 0, 50_000), leaf(TXID_B, 1, 50_000)];
        let plan = plan_leaf_combine(&planned, &plain(&ops(&planned)), 20.0).expect("plan");
        let swapped = [leaf(TXID_A, 0, 50_000), leaf(TXID_C, 9, 50_000)];
        assert!(
            build_unsigned_sweep(&plan, &swapped, &agg_address(), "regtest").is_err(),
            "building a different coin set than was validated must be refused"
        );
    }

    // ---- (e) THE ENCAPSULATION CENSUS ------------------------------------------------------------

    /// Everything before the first top-level `#[cfg(test)]` — the production half of this file. The
    /// assertions below must run against that and not the whole file, or the string literals in
    /// this very module would satisfy them. (Same device as
    /// `tesr.rs::payload_vout_access_census::production_half`.)
    fn production_half() -> &'static str {
        let src = include_str!("combine.rs");
        match src.find("\n#[cfg(test)]") {
            Some(at) => &src[..at + 1],
            None => src,
        }
    }

    /// The `mod gated { … }` span, by brace counting from its opening line.
    fn gated_span(src: &str) -> (usize, usize) {
        let at = src.find("\nmod gated {").expect(
            "the validating constructor must live in a PRIVATE module — `mod linked` \
             (tesr.rs:83) is this repo's precedent for making an encapsulation claim a compile \
             error instead of a docstring",
        );
        let open = at + src[at..].find('{').expect("the module opens");
        let mut depth = 0usize;
        for (i, ch) in src[open..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return (at, open + i);
                    }
                }
                _ => {}
            }
        }
        panic!("`mod gated` never closes");
    }

    /// **THE DOCSTRING'S CLAIM, MADE TRUE.** `CombinePlan` used to say "Produced ONLY by
    /// `plan_leaf_combine`, so there is no way to reach the builder with an unvalidated selection"
    /// while being a `pub struct` with every field `pub` — i.e. anyone could write the literal
    /// `CombinePlan { outpoints: …, fee_sats: 0, … }` and hand it straight to
    /// `build_unsigned_sweep`, skipping every refusal in this file. The claim was false.
    ///
    /// **Rust has no built-in compile-fail test**, so "this does not compile elsewhere" cannot be
    /// asserted by running something (a `trybuild` dependency would be the only way, and this
    /// crate has none). The property is instead guaranteed by MODULE PRIVACY, exactly as
    /// `mod linked` guarantees its ordering property: `CombinePlan` and `ColourEvidence` have
    /// PRIVATE FIELDS and live inside `mod gated`, which exports no constructor other than the
    /// validating ones. A private field cannot be named in a struct literal outside its defining
    /// module — that is a hard language rule, not a convention — so the only route to a
    /// `CombinePlan` is `plan_leaf_combine`, and the only route to a `ColourEvidence` is a probe.
    ///
    /// What CAN be asserted at runtime is that those two source facts still hold, since a future
    /// edit could always widen a visibility again. That is what this census does.
    #[test]
    fn a_combine_plan_cannot_be_built_outside_its_validating_constructor() {
        let src = production_half();
        let (start, end) = gated_span(src);
        let gated = &src[start..end];

        // 1. Both gated types are DECLARED inside the module…
        for ty in ["struct CombinePlan {", "struct ColourEvidence {"] {
            assert!(gated.contains(ty), "`{ty}` must be declared inside `mod gated`");
        }

        // 2. …with no `pub` field. A single `pub` field would not by itself allow a literal, but it
        //    is the first step back to one and there is no reason to have it: the accessors below
        //    give reads without giving construction.
        for ty in ["pub struct CombinePlan {", "pub struct ColourEvidence {"] {
            let at = gated.find(ty).expect("declared");
            let decl = &gated[at..at + gated[at..].find("\n    }\n").expect("the struct closes")];
            for line in decl.lines().skip(1) {
                assert!(
                    !line.trim_start().starts_with("pub "),
                    "`{ty}` must keep every field PRIVATE — `{}` re-opens the struct literal and \
                     with it the door around every refusal in this file",
                    line.trim()
                );
            }
        }

        // 3. …and no struct literal for either type exists anywhere OUTSIDE the module. This is the
        //    assertion that actually fails if somebody re-exports a constructor or moves the type
        //    back out to file scope.
        let outside: String = format!("{}{}", &src[..start], &src[end..])
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        for lit in ["CombinePlan {", "ColourEvidence {"] {
            assert!(
                !outside.contains(lit),
                "a `{lit}` literal outside `mod gated` is an UNVALIDATED value of a type whose \
                 whole purpose is to be evidence that validation ran"
            );
        }

        // 4. The validating constructor is itself inside the module — the `mod linked` shape, where
        //    the raw constructor has zero users outside the file's one gate.
        assert!(
            gated.contains("pub fn plan_leaf_combine("),
            "`plan_leaf_combine` must live INSIDE `mod gated`: if it were outside, `CombinePlan::new`\
             would have to be `pub(super)` and every other function in this file could call it"
        );
    }

    // ---- STEP 4 (journal) + STEP 5 (interlock): THE ORDERING, ON THIS FILE'S OWN SOURCE ----------

    /// Source of this module, comments stripped, from `pub async fn combine_leaves(` to its end.
    /// Comments are removed for the same reason `the_length_cap_is_not_reachable_from_the_exit_path`
    /// removes them: every assertion below is about what the function DOES, and prose describing an
    /// ordering would otherwise satisfy an assertion about that ordering.
    fn driver_body() -> String {
        let src = include_str!("combine.rs");
        let at = src.find("\npub async fn combine_leaves(").expect("driver not found");
        let end = at + src[at..].find("\n}\n").expect("driver ends");
        src[at..end]
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// **THE WRITE-AHEAD DISCIPLINE, copied from `in_ladder_split` (tesr.rs:4291-4367).** Everything
    /// past the first `sign_first` produces material the SE will not re-issue for free, and the
    /// signed sweep itself is unregenerable. The plan must be on disk BEFORE the first round-trip,
    /// and the signature must be recorded the moment it exists — otherwise a crash leaves N consumed
    /// nonces and no record of what they were spent on.
    #[test]
    fn the_journal_is_written_before_the_first_signature_and_again_the_moment_it_exists() {
        let b = driver_body();
        let planned = b.find("CombineStage::Planned").expect("a Planned journal write");
        let first_sign = b.find("sign_first(").expect("the first SE round-trip");
        let signed = b.find("CombineStage::Signed").expect("a Signed journal write");
        let broadcast = b.find("transaction_broadcast_raw").expect("the broadcast");
        assert!(
            planned < first_sign,
            "the plan must be journalled BEFORE the first co-signature round-trip"
        );
        assert!(
            signed > first_sign && signed < broadcast,
            "the co-signature must be journalled after it exists and BEFORE it is broadcast — a \
             crash between signing and broadcasting must not lose the only copy of an \
             unregenerable transaction"
        );
    }

    /// **THE INTERLOCK (step 5).** `defend_ladders` drives an adopted leaf's `ctesr-` row through
    /// `watch_child_pass`, which broadcasts `ext_child` — the SAME `SP.out[j]` the combine is
    /// spending. Both are the owner's own, so the loss is not funds; it is that ONE evicted leaf
    /// invalidates the whole batch, after every input has been co-signed.
    ///
    /// The interlock reuses the tower's own L1 LIVENESS ALLOWLIST rather than adding anything to the
    /// tower: the leaf is parked OUT of `CONFIRMED`, durably, before the first co-signature, and the
    /// tower is already written to broadcast only for `CONFIRMED` coins. That is the shape L1 was
    /// designed for — "a lane added tomorrow parks its coin in SOME non-CONFIRMED status and is
    /// refused BY DEFAULT" (wallet.rs:1857-1866). Nothing is added to the broadcast path, which
    /// step 6 then pins.
    #[test]
    fn the_leaf_is_parked_out_of_the_watchtower_allowlist_before_the_first_signature() {
        assert_ne!(
            COMBINE_PARK_STATUS,
            CoinStatus::CONFIRMED,
            "the park status must be OUTSIDE the tower's CONFIRMED allowlist, or the interlock is a \
             no-op"
        );
        let b = driver_body();
        let park = b.find("COMBINE_PARK_STATUS").expect("the driver must park the leaves");
        let first_sign = b.find("sign_first(").expect("the first SE round-trip");
        assert!(
            park < first_sign,
            "the park must be DURABLE BEFORE the first co-signature — the [A2] window: a status \
             changed in memory only is a status the per-block tower never reads"
        );
        assert!(
            b.contains("restore_parked_statuses"),
            "an abandoned combine must UNPARK, or the leaf is left undefended by the tower for a \
             sweep that never happened"
        );
    }

    /// The other half of the interlock: the tower really is keyed on `CONFIRMED`, in both passes
    /// that can broadcast. Asserted on `wallet.rs`'s own source so that weakening the allowlist
    /// there — which would silently re-open this conflict window — turns THIS red.
    #[test]
    fn the_watchtower_passes_are_still_keyed_on_the_confirmed_allowlist() {
        let src = include_str!("../../rust-sdk/src/wallet.rs");
        let cut = |marker: &str| -> String {
            let at = src.find(marker).unwrap_or_else(|| panic!("{marker} not found"));
            src[at..at + src[at..].find("\n    }\n").expect("fn ends")]
                .lines()
                .filter(|l| !l.trim_start().starts_with("//"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let defend = cut("\n    async fn defend_ladders_inner(");
        assert!(
            defend.contains("c.status != CoinStatus::CONFIRMED")
                && defend.contains("c.status == CoinStatus::CONFIRMED"),
            "defend_ladders must keep BOTH L1 allowlist filters (the root loop's and `live_sids`, \
             which gates the `ctesr-` leaf loop) — the combine's interlock is exactly those filters"
        );
        let auto = cut("\n    async fn auto_exit_due_inner(");
        assert!(
            auto.contains("c.status == CoinStatus::CONFIRMED"),
            "auto_exit_due must keep its CONFIRMED filter — a parked leaf must not be force-exited \
             out from under an in-flight combine"
        );
    }

    // ---- STEP 6: NOT REACHABLE FROM THE EXIT PATH ------------------------------------------------

    /// **THE MOST IMPORTANT PROPERTY IN THIS FILE**, in the style of
    /// `tesr.rs::the_length_cap_is_not_reachable_from_the_exit_path`.
    ///
    /// Every refusal above is safe because refusing to BUILD a combine leaves the leaf exactly where
    /// it was — with its pre-signed tiers, exitable, needing no SE signature (see
    /// `an_abandoned_combine_leaves_the_leaf_exitable`). The same refusal placed on the BROADCAST
    /// path would be the opposite: a leaf that cannot be swept AND cannot be exited is a permanent
    /// loss. So no combine symbol may appear in any exit-path function, asserted by source text.
    #[test]
    fn the_combine_refusals_are_not_reachable_from_the_exit_path() {
        // Combine symbols that REFUSE. If any of these can be evaluated while walking a ladder to
        // chain, a slow exit becomes an impossible one.
        const COMBINE_SYMBOLS: &[&str] = &[
            "CombineRefusal",
            "plan_leaf_combine",
            "check_leaf_observations",
            "combine_is_economic",
            "sweep_fee_sats",
            "sweep_tx_vsize",
            "build_unsigned_sweep",
            "combine_leaves",
            "COMBINE_PARK_STATUS",
        ];

        let cut = |src: &str, marker: &str, end_pat: &str| -> String {
            let at = src.find(marker).unwrap_or_else(|| panic!("{marker} not found"));
            src[at..at + src[at..].find(end_pat).expect("function ends")]
                .lines()
                .filter(|l| !l.trim_start().starts_with("//"))
                .collect::<Vec<_>>()
                .join("\n")
        };

        // ---- the client-library broadcast path -------------------------------------------------
        let tesr = include_str!("tesr.rs");
        for f in [
            "\npub fn exit_pass(",
            "\npub fn next_exit_tier(",
            "\npub fn exit_child_pass(",
            "\npub fn next_child_exit_tier(",
            "\npub fn child_exit_chain(",
            "\npub fn child_exit_chain_bound(",
            "\npub fn spine_tip_exit_chain(",
            "\npub fn spine_tip_exit_chain_bound(",
            "\npub fn watch_child_pass(",
            "\nfn watch_child_pass_seen(",
        ] {
            let body = cut(tesr, f, "\n}\n");
            for sym in COMBINE_SYMBOLS {
                assert!(
                    !body.contains(sym),
                    "{f} IS ON THE BROADCAST PATH. `{sym}` there turns a slow exit into a \
                     PERMANENT LOSS — a leaf that can neither be swept nor exited. Put combine \
                     refusals on the build path, never here."
                );
            }
        }

        // ---- the SDK's own broadcast/watchtower passes -----------------------------------------
        let wallet = include_str!("../../rust-sdk/src/wallet.rs");
        for f in [
            "\n    async fn defend_ladders_inner(",
            "\n    async fn auto_exit_due_inner(",
            "\n    pub async fn unilateral_exit(",
        ] {
            let body = cut(wallet, f, "\n    }\n");
            for sym in COMBINE_SYMBOLS {
                assert!(
                    !body.contains(sym),
                    "{f} IS ON THE BROADCAST PATH — `{sym}` must not be reachable from it."
                );
            }
        }
    }

    /// **STEP 0's TRACE, PINNED.** A combine that is co-signed and then ABANDONED (crash, fee spike,
    /// operator change of mind) must leave the leaf exitable. It does, and the reason is structural
    /// rather than careful:
    ///
    ///   * `exit_child_pass` (tesr.rs:5356-5384) walks `child_exit_chain(cb)` (tesr.rs:5327-5347),
    ///     which returns the bundle's ALREADY-`signed_tx` hex for every tier, and hands each one
    ///     straight to `electrum.transaction_broadcast_raw` (tesr.rs:5373). Neither function is
    ///     `async` and neither contains `sign_first` / `sign_second` — the unilateral exit asks the
    ///     SE for NOTHING.
    ///   * The combine's own SE round-trips produce a signature over a DIFFERENT message. They
    ///     cannot un-sign a transaction the client already holds complete.
    ///   * The only event that invalidates the pre-signed tail is `SP.out[j]` actually being SPENT —
    ///     i.e. the combine being BROADCAST, which by definition an abandoned one is not.
    ///
    /// Asserted on the source so that making the exit path need a fresh signature — the one change
    /// that would turn this feature into a leaf-bricking one — turns this red.
    #[test]
    fn an_abandoned_combine_leaves_the_leaf_exitable() {
        let tesr = include_str!("tesr.rs");
        let cut = |marker: &str| -> String {
            let at = tesr.find(marker).unwrap_or_else(|| panic!("{marker} not found"));
            tesr[at..at + tesr[at..].find("\n}\n").expect("fn ends")]
                .lines()
                .filter(|l| !l.trim_start().starts_with("//"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        for f in ["\npub fn exit_child_pass(", "\npub fn child_exit_chain(", "\npub fn exit_pass("] {
            let body = cut(f);
            assert!(
                !body.contains("sign_first") && !body.contains("sign_second") && !body.contains("cosign"),
                "{f} MUST NOT need a fresh SE signature. If the unilateral exit ever asks the SE to \
                 sign, a co-signed-but-unbroadcast combine could consume the budget that exit needs \
                 and the leaf would be BRICKED — the feature must not ship in that form."
            );
            assert!(
                !body.contains(".await"),
                "{f} must stay synchronous and offline-capable: an exit that needs a network \
                 round-trip to anything but the chain is an exit an uncooperative SE can block"
            );
        }
        // …and the tail really does spend the leaf's own funding output, so it stays valid for
        // exactly as long as that output is unspent.
        assert!(
            cut("\npub fn child_exit_chain(").contains("cb.child_extension.signed_tx.clone()"),
            "the leaf's tail must be the stored, already-signed child extension"
        );
    }
}
