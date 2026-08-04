//! Client-side driver for TES-R tier co-signing against the live blind SE.
//!
//! The SE is unchanged: it blind-co-signs whatever sighash the client presents (`/sign/first` +
//! `/sign/second`), so a tier tx (v3, relative-timelock, P2A anchor) round-trips through exactly the
//! same MuSig2 flow as an un-laddered coin's backup tx. This module wires [`mercurylib::tesr::cosign_tier_request`] into
//! that round-trip and returns the fully-signed, broadcast-ready tier tx.

use std::str::FromStr;

use anyhow::Result;
use electrum_client::ElectrumApi;
use mercurylib::{
    tesr::cosign_tier_request,
    transaction::{create_signature, new_backup_transaction},
    wallet::Coin,
};
use serde::{Deserialize, Serialize};

use crate::{
    client_config::ClientConfig,
    transaction::{sign_first, sign_second},
};

const TESR_BUNDLE_VERSION: u32 = 1;

/// One pre-signed, broadcast-ready TES-R tier tx as persisted in a coin's bundle.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TesrTier {
    pub txid: String,
    pub signed_tx: String,
    /// The value this tier pays forward (its payload output) — the prevout the child tier spends.
    pub out_value: u64,
    /// The relative-timelock (BIP-68 blocks) on this tier's input; `None` for the trigger.
    pub csv: Option<u16>,
    /// **Payload-vout accessor** — the output index at which this tier's PAYLOAD (value-carrying)
    /// outputs begin. `mercurylib::tesr::UNCOLORED_PAYLOAD_VOUT` (= 0) today; a coloured CTES-R tier
    /// carries the opret at 0 and shifts every payload by one.
    ///
    /// ⚠️ In a CONVEYED bundle this field is ATTACKER-SUPPLIED, exactly like every other field. It is
    /// never trusted on its own: every site that reads it cross-checks the index against transaction
    /// CONTENT (`spk == agg_spk`, `taproot_key_hex` on the spk, or the prevout amount feeding
    /// `verify_tier_cosigned`'s sighash), so a wrong value fails CLOSED with a named verification
    /// error rather than mis-chaining. `serde(default)` keeps every already-persisted `tesr-*` /
    /// `ctesr-*` row and every in-flight mailbox message deserializing byte-identically.
    #[serde(default)]
    pub payload_vout: u32,
}

use linked::{tier_payload_prevout, LinkedPayload};

// =================================================================================================
// `payload_vout` → an OUTPUT: the ONE place in this file where that step may be taken.
// =================================================================================================
//
// `TesrTier::payload_vout` is ATTACKER-SUPPLIED in every conveyed bundle. Turning it into a `TxOut`
// — `tx.output[payload_vout]` — is a step that can only be justified by a STRUCTURAL fact already
// proven about that index: that the next tier really spends it, or that the output sitting there
// really pays the key it is supposed to pay. Taken without one, a tampered index silently yields
// the P2A ANCHOR (240 sat), so every VALUE law downstream computes on the anchor and refuses with an
// arithmetic message — hiding the accurate structural cause, and in one case shadowing a refusal an
// E2E pins.
//
// That mistake was made FOUR times in a single day, by one author, in four different places:
// `deed25c`'s root value law, the GAP-1 payload-count check, the declared-`out_value` check, and
// again while FIXING the third — one check was moved below the linkage test and its two neighbours
// were left above it. Each instance was fixed by re-ordering statements and writing a comment
// explaining the order. Ordering that only a comment defends is not defended.
//
// So the accessor is PRIVATE TO THIS MODULE, and the module exports no way to reach it except the
// three `link_*` constructors, each of which performs a structural pin and only then hands back a
// [`LinkedPayload`]. The value laws take that type. A `LinkedPayload` cannot exist before its pin
// ran, so "value check before linkage check" stops being a review comment and becomes a thing that
// does not compile.
//
// **Trusted construction is a different case and is NOT put through this ceremony.** A builder that
// reads a tier IT JUST BUILT, from a bundle this process owns both key halves for, has no attacker
// to defend against and no next tier to link to yet — there is nothing for a pin to prove. Those
// four call sites go through [`tier_payload_prevout`], which lives in here too (so the raw accessor
// still has zero users outside this module) and does the binding that IS available to a builder:
// re-derive the transaction from its own hex, check it hashes to the declared txid, and check the
// output matches the declared `out_value`. The test-side census `payload_vout_access_census` pins
// the encapsulation itself, since a future edit could always widen a visibility again.
mod linked {
    use anyhow::Result;
    use electrum_client::bitcoin::{ScriptBuf, Transaction, TxOut};

    use super::TesrTier;

    /// A tier's payload output, **plus the proof that it is the payload**.
    ///
    /// Constructible only by one of the `link_*` methods below, each of which pins the declared
    /// `payload_vout` to a structural fact before yielding one. Holding a `LinkedPayload` is
    /// therefore evidence — carried in the type system rather than in a comment — that the
    /// structural check ran FIRST. Give the value laws this instead of a `&TesrTier` + `&Transaction`
    /// pair and the ordering defect becomes unrepresentable.
    pub(super) struct LinkedPayload<'a> {
        out: &'a TxOut,
    }

    impl<'a> LinkedPayload<'a> {
        /// The value the pinned output carries — the term every conservation law measures against.
        pub(super) fn value(&self) -> u64 {
            self.out.value
        }

        /// The pinned output's scriptPubKey. Safe to hand out: reaching here already cost a pin.
        pub(super) fn script_pubkey(&self) -> &'a ScriptBuf {
            &self.out.script_pubkey
        }
    }

    impl TesrTier {
        /// This tier's payload output within `tx`. Fails CLOSED if `payload_vout` is out of range — a
        /// conveyed bundle can claim any index, and an out-of-range one must be a rejection, never a
        /// panic and never a silent fallback to `output[0]`.
        ///
        /// **Private to this module on purpose** — see the module comment. Everything a verifier is
        /// allowed to do with it is one of the `link_*` methods below.
        fn payload_out<'a>(&self, tx: &'a Transaction, what: &str) -> Result<&'a TxOut> {
            tx.output.get(self.payload_vout as usize).ok_or_else(|| {
                anyhow::anyhow!(
                    "{what}: declared payload_vout {} is out of range ({} outputs)",
                    self.payload_vout,
                    tx.output.len()
                )
            })
        }

        /// **PIN 1 — the next tier really spends it.** The strongest of the three: the declared index
        /// is not merely plausible, it is the outpoint the chain below actually hangs off, so a value
        /// read here is the value that chain is funded with.
        ///
        /// `what` names this tier in the out-of-range refusal; `mismatch` is the CALLER's own refusal
        /// text for a child that spends something else, because those messages are pinned by E2Es
        /// (sdk70 D1) and by the unit tests in this file, and a generic replacement would lose the
        /// caller's tier numbering.
        ///
        /// The range check runs BEFORE the linkage comparison, which is the order every one of these
        /// sites had before this type existed (each tier's index was already range-checked by its own
        /// payee check, one loop iteration earlier).
        pub(super) fn link_child<'a>(
            &self,
            tx: &'a Transaction,
            child: &Transaction,
            what: &str,
            mismatch: &str,
        ) -> Result<LinkedPayload<'a>> {
            let out = self.payload_out(tx, what)?;
            if child.input.len() != 1
                || child.input[0].previous_output.txid != tx.txid()
                || child.input[0].previous_output.vout != self.payload_vout
            {
                return Err(anyhow::anyhow!("{mismatch}"));
            }
            Ok(LinkedPayload { out })
        }

        /// **PIN 2 — the output at the declared index pays the key it is supposed to pay.** The
        /// TERMINAL tier of a chain has no child to link it, so the payee is the pin — and it is a
        /// real one: the P2A anchor, an opret and a skim-to-self change output all carry a different
        /// script, so none of them can be passed off as the payload.
        pub(super) fn link_pays<'a>(
            &self,
            tx: &'a Transaction,
            want: &ScriptBuf,
            what: &str,
            mismatch: &str,
        ) -> Result<LinkedPayload<'a>> {
            let out = self.payload_out(tx, what)?;
            if &out.script_pubkey != want {
                return Err(anyhow::anyhow!("{mismatch}"));
            }
            Ok(LinkedPayload { out })
        }

        /// **PIN 2′ — the same pin, by taproot OUTPUT KEY rather than by whole scriptPubKey.** Model A
        /// compares the receiver's key, not their script, and a non-taproot output must keep failing
        /// with `taproot_key_hex`'s own "not a v1 taproot scriptPubKey" rather than with the payee
        /// message — so the comparison is reproduced here exactly rather than approximated by
        /// [`Self::link_pays`].
        pub(super) fn link_pays_taproot_key<'a>(
            &self,
            tx: &'a Transaction,
            want_key_hex: &str,
            what: &str,
            mismatch: &str,
        ) -> Result<LinkedPayload<'a>> {
            let out = self.payload_out(tx, what)?;
            if super::taproot_key_hex(out.script_pubkey.as_bytes())? != want_key_hex {
                return Err(anyhow::anyhow!("{mismatch}"));
            }
            Ok(LinkedPayload { out })
        }
    }

    /// The PAYLOAD output of an already-built tier, as `(value, scriptPubKey hex)` — read from the
    /// transaction, never from the declared `out_value`. A coloured child needs both: the value feeds
    /// the fee arithmetic and the taproot sighash, the script is the prevout rgb-lib must see.
    ///
    /// **TRUSTED CONSTRUCTION ONLY — this is the builders' door, and it is not a verifier's.** Its
    /// four callers (`build_colored_receiver_state`, `build_colored_renewal`, the coloured split, and
    /// the coloured child re-transfer) each read a tier out of a bundle THIS process owns: either one
    /// it just built, or one a claim already put through `verify_bundle_ex` / `verify_child_bundle`
    /// end to end. There is no attacker in the loop and, for a tier being extended, no child tier in
    /// existence yet — so there is no structural fact for a `link_*` pin to prove. What a builder CAN
    /// bind, it binds: the hex must hash to the declared txid, and the parsed output must equal the
    /// declared `out_value`.
    ///
    /// Do not reach for this from an acceptance path. If a value is about to be measured, compared or
    /// booked on behalf of a receiver, it needs a `LinkedPayload`.
    pub(super) fn tier_payload_prevout(tier: &TesrTier, what: &str) -> Result<(u64, String)> {
        use electrum_client::bitcoin::consensus::deserialize;
        let raw = hex::decode(&tier.signed_tx)
            .map_err(|_| anyhow::anyhow!("{what}: tier hex does not decode"))?;
        let tx: Transaction =
            deserialize(&raw).map_err(|_| anyhow::anyhow!("{what}: tier tx does not parse"))?;
        if tx.txid().to_string() != tier.txid {
            return Err(anyhow::anyhow!(
                "{what}: stored tier tx hashes to {} but the bundle names {}",
                tx.txid(),
                tier.txid
            ));
        }
        let out = tier.payload_out(&tx, what)?;
        if out.value != tier.out_value {
            return Err(anyhow::anyhow!(
                "{what}: declared out_value {} disagrees with the transaction's {} at vout {}",
                tier.out_value,
                out.value,
                tier.payload_vout
            ));
        }
        Ok((out.value, hex::encode(out.script_pubkey.as_bytes())))
    }
}

/// One depth LEVEL of the ladder: an extension and the state hanging off it. For a non-final level
/// the state is a SELF-SPLIT paying the aggregate `A` (its output hosts the next level — this is the
/// off-chain rollover); the final level's state is the exit leg to the owner.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TesrLevel {
    pub extension: TesrTier,
    pub state: TesrTier,
}

/// A coin's persisted TES-R ladder — everything an owner or a keyless tower needs to renew, roll over
/// or exit the coin, held on the owner's disk (the SE never sees it). Persisted under `tesr-<id>`.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TesrBundle {
    pub version: u32,
    pub statechain_id: String,
    pub network: String,
    pub fee_rate: f64,
    /// P2TR(A) — the aggregate address every tier pays (invariant across renewals and transfers).
    pub agg_address: String,
    /// Where the final state pays on a unilateral exit.
    pub owner_exit_address: String,
    pub f_txid: String,
    pub f_vout: u32,
    pub f_value: u64,
    pub trigger: TesrTier,
    /// One entry per rollover depth level; the LAST level's state is the exit leg to the owner.
    pub levels: Vec<TesrLevel>,
    /// Renewal counter at the current (deepest) level.
    pub m: u32,
    /// SUPERSEDED states — every owner-paying state the SE co-signed that a later renewal/rollover/
    /// transfer replaced. Kept for FULL-DISCLOSURE counting (history/MIGRATION.md): the SE counts their
    /// co-signs, so verify_bundle must see them or num_sigs won't balance; and the current state must
    /// be at a strictly LOWER CSV than every one of these (it matures first, so a retained stale state
    /// loses the race). Not part of the exit chain.
    #[serde(default)]
    pub superseded_states: Vec<TesrTier>,
    /// SUPERSEDED extensions (from renewals) — kept only so their co-signs are counted.
    #[serde(default)]
    pub superseded_extensions: Vec<TesrTier>,
    /// The relative-timelock schedule this coin runs on (mainnet/regtest preset). Drives the
    /// param-based cadence in [`renew_auto`] / [`rollover_auto`]; defaults to mainnet for bundles
    /// persisted before this field existed.
    #[serde(default)]
    pub params: mercurylib::tesr::TesrParams,
    /// **CTES-R.** Present iff this is a COLOURED ladder: every tier carries a valid RGB state
    /// transition, so laddering the carrier MOVES the allocation instead of destroying it.
    ///
    /// `None` on every plain coin — and `#[serde(default)]` keeps every already-persisted `tesr-*`
    /// row and every in-flight mailbox message deserializing byte-identically, so the plain path is
    /// unchanged on the wire as well as in behaviour.
    #[serde(default)]
    pub rgb: Option<ColoredLadder>,
}

/// The RGB half of a CTES-R ladder: which contract rides it, how much, and the per-tier
/// consignments proving each tier's transition.
///
/// The consignments are held in **exit-tier order** — exactly [`TesrBundle::exit_tiers`] —
/// `[trigger, ext_0, state_0, ext_1, state_1, …]`, so `consignments[i]` is the proof for
/// `exit_tiers()[i]`. A receiver validates the LEAF one (`consignments.last()`) against the ordered
/// ladder txid list; the earlier ones are what let it resolve the un-broadcast chain above it.
///
/// Seal blindings are **not** stored: they are DERIVED on both sides from
/// `mercuryrustlib::rgb::TierSeal(statechain_id, role, tier_index, rung)`, and a stored blinding
/// would be an attacker-supplied field the receiver could be talked into trusting.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ColoredLadder {
    /// RGB contract id whose allocation this ladder carries.
    pub contract_id: String,
    /// The full fungible amount riding the ladder (the whole allocation — CTES-R moves the
    /// allocation as a unit; partial amounts are the in-ladder coloured split, a later commit).
    pub amount: u64,
    /// Per-tier consignments, base64, in [`TesrBundle::exit_tiers`] order.
    pub consignments: Vec<String>,
}

/// [in-ladder split] A split child's exit bundle. It spans TWO aggregates: the ancestor segment
/// (`T, X_m, SP`) under the PARENT's aggregate `A_parent` (rooted at the on-chain funding `F`), and the
/// child's own headless ladder (`ext_child, state_child`) under `A_child` = the key `SP.out[sp_vout]`
/// pays. `verify_child_bundle` (the 8-check Stage-2 predicate, ruling wqvoxvusg) proves the child is
/// safe from a hidden parent state rivalling `SP` over `X_m.out[0]` WITHOUT any SGX change.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ChildTesrBundle {
    /// The PARENT segment as an ordinary TES-R bundle: exit chain `T -> X_m -> SP`, with `SP` the
    /// current (terminal) state paying the children+P2A, and the parent's old owner state `S_0`
    /// disclosed in `superseded_states` (it rivals `SP` over `X_m.out[0]` and must lose the race).
    pub parent: TesrBundle,
    pub parent_statechain_id: String,
    /// Which `SP` output funds this child (`j`).
    pub sp_vout: u32,
    pub child_statechain_id: String,
    /// The child receiver's own exit key (Model A: the final child state must pay THIS).
    pub child_owner_exit_address: String,
    /// Child ladder: `child_extension` spends `SP.out[sp_vout]`; `child_state` spends its `out[0]`.
    pub child_extension: TesrTier,
    pub child_state: TesrTier,
    #[serde(default)]
    pub child_superseded_states: Vec<TesrTier>,
    #[serde(default)]
    pub child_superseded_extensions: Vec<TesrTier>,
    /// INTERMEDIATE child segments, root→leaf, when this child descends from another child (a
    /// child-level in-ladder split). Empty for a depth-1 child, which is why it is `serde(default)`:
    /// every already-persisted `ctesr-*` row and every in-flight mailbox message keeps deserializing.
    ///
    /// `sp_vout` is always relative to the IMMEDIATELY PRECEDING segment — `parent.current().state`
    /// when this is empty, otherwise `ancestors.last().state`.
    #[serde(default)]
    pub ancestors: Vec<ChildSegment>,
    /// **CTES-R.** Present iff this child carries an RGB allocation — i.e. it was carved by
    /// [`colored_in_ladder_split`] out of a COLOURED parent. `None` on every plain child, and
    /// `#[serde(default)]` keeps every already-persisted `ctesr-*` row and every in-flight mailbox
    /// message deserializing byte-identically.
    #[serde(default)]
    pub rgb: Option<ColoredChild>,
    /// **The parent segment's FLAT (signed-once) backup chain, conveyed.**
    ///
    /// The census `verify_child_bundle` runs over the ancestor segment is
    /// `num_sigs(parent) == flat_backups + tiers + superseded`, and `flat_backups` is a fact about
    /// the parent's history that the receiver cannot observe: it never owned the parent. It used to
    /// be supplied as the constant [`PARENT_V2_BASELINE`] (= 1), which is right only for a parent
    /// this wallet DEPOSITED — every whole-coin hop co-signs one more flat backup
    /// (`transfer_sender::create_backup_tx_to_receiver`), so a parent received `k` times carries
    /// `1 + k` and the constant under-counts by exactly `k`.
    ///
    /// So the chain is CONVEYED and the receiver counts it itself, exactly as the whole-coin receive
    /// path does (`transfer_receiver.rs`, `transfer_msg.backup_transactions.len()`). A conveyed count
    /// is only safe because it is STRUCTURALLY VALIDATED before it is counted
    /// ([`verify_conveyed_child`] runs `validate_backup_chain_v2` against the parent's on-chain `F`):
    /// every entry must be a real taproot key-spend of `F` under `A_parent` — i.e. must have consumed
    /// a real SE co-sign — and INV-5 (`ladder_decrements_by_interval`) forbids duplicate padding and
    /// ladder inversion. An attacker can therefore not inflate this term to absorb a hidden co-signed
    /// state; that is the same argument [S2] makes for the whole-coin lane.
    ///
    /// `#[serde(default)]` for the usual reason — already-persisted `ctesr-*` rows and in-flight
    /// mailbox messages keep deserializing. An EMPTY vector is refused by the verifier (a parent
    /// always carries at least its deposit `tx1`), so the default is fail-closed, not fail-open.
    #[serde(default)]
    pub parent_flat_backups: Vec<mercurylib::wallet::BackupTx>,
}

/// The RGB half of a COLOURED split child: this child's own share of the parent's allocation, and
/// the consignments proving its two own tiers.
///
/// The ancestor segment's proofs (`T`, `X_m`, `SP`) ride in `ChildTesrBundle::parent.rgb` in
/// [`TesrBundle::exit_tiers`] order, so the full leaf-ward chain a receiver must validate is
/// `parent.rgb.consignments ++ self.consignments`, and its witness list is
/// `parent.ladder_txids() ++ [child_extension.txid, child_state.txid]`.
///
/// As with [`ColoredLadder`], seal blindings are **not** stored — they are DERIVED on both sides
/// from [`colored_tier_seal`], because a stored blinding is an attacker-supplied field the receiver
/// could be talked into trusting.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ColoredChild {
    /// RGB contract id whose allocation this child carries. MUST equal the parent segment's.
    pub contract_id: String,
    /// THIS child's share of the allocation — strictly less than the parent's whole when the split
    /// has more than one child, and `Σ children == parent.rgb.amount` by construction.
    pub amount: u64,
    /// Consignments for the child's OWN two tiers, in exit order:
    /// `[child_extension, child_state]`.
    pub consignments: Vec<String>,
}

/// **The `SP` tier a segment's `sp_vout` indexes into — ONE definition for both leaf shapes.**
///
/// A split child and a spine tip carry the same convention, spelled out in
/// [`ChildTesrBundle::ancestors`]: `sp_vout` is relative to the IMMEDIATELY PRECEDING segment, which
/// is `parent.current().state` only while `ancestors` is empty and `ancestors.last().state` after
/// that. Written out twice it drifts, and the drift is silent — a depth-2 record would name a
/// perfectly real outpoint belonging to the wrong `SP`, so every read of it (funding value, RGB
/// spend-marking, the cap's re-anchor) would be confidently wrong rather than absent.
fn segment_funding_tier<'a>(parent: &'a TesrBundle, ancestors: &'a [ChildSegment]) -> &'a TesrTier {
    ancestors.last().map_or(&parent.current().state, |seg| &seg.state)
}

impl ChildTesrBundle {
    /// True iff this child carries an RGB allocation (CTES-R).
    pub fn is_colored(&self) -> bool {
        self.rgb.is_some()
    }

    /// `SP.out[sp_vout]` — the un-broadcast outpoint this child's ladder hangs off, and the outpoint
    /// an RGB engine has registered for it while it is off-chain. See [`segment_funding_tier`] for
    /// why the `SP` is not unconditionally the root parent's.
    pub fn funding_outpoint(&self) -> (String, u32) {
        (segment_funding_tier(&self.parent, &self.ancestors).txid.clone(), self.sp_vout)
    }

    /// The FULL off-chain witness list a coloured child's consignments must be resolved against, in
    /// leaf-ward order: the ancestor segment `T, X_m, SP` followed by the child's own two tiers.
    ///
    /// Refuses a multi-level child (`ancestors` non-empty): a coloured GRANDCHILD cannot be built
    /// today (`child_in_ladder_split` refuses a coloured child), so a bundle claiming to be both
    /// coloured and multi-level did not come from this code and has no derivable seal schedule.
    /// Fail CLOSED rather than resolve a chain we cannot account for.
    pub fn colored_child_txids(&self) -> Result<Vec<String>> {
        if !self.is_colored() {
            return Err(anyhow::anyhow!("this child is PLAIN — it has no coloured witness chain"));
        }
        if !self.ancestors.is_empty() {
            return Err(anyhow::anyhow!(
                "a coloured child must be depth-1 (found {} intermediate segments) — coloured \
                 child-level split does not exist, so a multi-level coloured child has no \
                 derivable seal schedule",
                self.ancestors.len()
            ));
        }
        let mut v = self.parent.ladder_txids();
        v.push(self.child_extension.txid.clone());
        v.push(self.child_state.txid.clone());
        Ok(v)
    }

    /// **The seal schedule of a coloured child — derived by BOTH parties, stored by neither.**
    ///
    /// Five seals in leaf-ward order, matching [`Self::colored_child_txids`]:
    ///
    /// | tier | derived from |
    /// |---|---|
    /// | `T` | parent sid, [`TierRole::Trigger`] |
    /// | `X_m` | parent sid, [`TierRole::Extension`], rung `m ‖ csv` |
    /// | `SP` | parent sid, [`TierRole::SplitState`], rung `m ‖ csv` — **at THIS child's `sp_vout`** |
    /// | `ext_child` | CHILD sid, [`TierRole::ChildExtension`] |
    /// | `state_child` | CHILD sid, [`TierRole::ChildState`] |
    ///
    /// Two things here are load-bearing and easy to get wrong:
    ///
    /// 1. `SP`'s role is `SplitState`, **not** `State`. `SP` and the parent's retained `S_0` are
    ///    RIVAL transitions over the same `X_m` payload output; deriving `SP` with `TierRole::State`
    ///    would hand it `S_0`'s blinding, the two would collapse to one `OpId`, and rgb-lib would
    ///    keep whichever witness has the smaller internal txid — an arbitrary hash lottery
    ///    (`docs/utexo/CTESR-GATE.md` §2.2). This is why [`TesrBundle::colored_tier_seals`] cannot
    ///    be reused for a split parent: it hard-codes `State` for the current state.
    /// 2. Every child of one split shares `SP`'s *blinding* (one transition, one seal identity) and
    ///    is separated only by `sp_vout`. That is sound — an RGB seal is `(vout, blinding)` — and it
    ///    is also what keeps a child's siblings CONCEALED from it: revealing `(sp_vout_j, blinding)`
    ///    opens child `j`'s assignment and no other.
    pub fn colored_child_seals(&self) -> Result<Vec<(String, u32, u64)>> {
        use crate::rgb::TierRole;
        let _ = self.colored_child_txids()?;
        let p = &self.parent;
        if p.levels.len() != 1 {
            return Err(anyhow::anyhow!(
                "a coloured child's parent segment must have exactly one level (found {})",
                p.levels.len()
            ));
        }
        let psid = &p.statechain_id;
        let ext = &p.current().extension;
        let sp = &p.current().state;
        let csid = &self.child_statechain_id;
        Ok(vec![
            (
                p.trigger.txid.clone(),
                p.trigger.payload_vout,
                colored_tier_seal(psid, TierRole::Trigger, 0, 0, None).blinding(),
            ),
            (
                ext.txid.clone(),
                ext.payload_vout,
                colored_tier_seal(psid, TierRole::Extension, 0, p.m, ext.csv).blinding(),
            ),
            (
                sp.txid.clone(),
                // THIS child's payload output of the shared split transition.
                self.sp_vout,
                colored_tier_seal(psid, TierRole::SplitState, 0, p.m, sp.csv).blinding(),
            ),
            (
                self.child_extension.txid.clone(),
                self.child_extension.payload_vout,
                colored_tier_seal(csid, TierRole::ChildExtension, 0, 0, self.child_extension.csv)
                    .blinding(),
            ),
            (
                self.child_state.txid.clone(),
                self.child_state.payload_vout,
                colored_tier_seal(csid, TierRole::ChildState, 0, 0, self.child_state.csv)
                    .blinding(),
            ),
        ])
    }

    /// The LEAF consignment — the proof for the child's own final state, the one a receiver
    /// validates and books against.
    pub fn leaf_consignment(&self) -> Option<&String> {
        self.rgb.as_ref().and_then(|r| r.consignments.last())
    }
}

/// **The CTES-R interlock, child side.** Refuse an operation that would build an UNCOLOURED tier
/// over a COLOURED child's sealed output.
///
/// The mirror of [`refuse_uncolored_over_colored`], and it exists for a hazard that is strictly
/// worse than the parent one because it is *silent by omission*: `child_in_ladder_split` and
/// `child_retransfer` build plain tiers over `ext_child.out[0]` / the child's state output. Before
/// coloured children existed those two were vacuously safe — no child ever carried an allocation.
/// The moment [`colored_in_ladder_split`] can produce one, the next hop would spend a sealed output
/// with an RGB-unaware transaction and BURN the allocation, with every existing check passing.
pub fn refuse_uncolored_over_colored_child(cb: &ChildTesrBundle, what: &str) -> Result<()> {
    if cb.is_colored() {
        return Err(anyhow::anyhow!(
            "{what}: this CHILD carries an RGB allocation (CTES-R) and {what} would build an \
             UNCOLOURED tier over a sealed output, destroying it. Refusing — use the coloured \
             replacement path instead: `build_colored_child_retransfer` + \
             `cosign_colored_child_retransfer` (SDK: `transfer_colored_child`) to move a coloured \
             child whole. A coloured child-level SPLIT does not exist; split at the root instead."
        ));
    }
    Ok(())
}

/// The RGB half of a COLOURED SPINE TIP: the change's remaining share of the allocation, and the
/// consignment proving its ONE cap tier.
///
/// Deliberately **not** [`ColoredChild`]. That struct's `consignments` field is documented and read
/// as "the child's own two tiers, in exit order `[child_extension, child_state]`" — a contract a
/// one-cap tip cannot satisfy. Re-using it with a single entry would make every `consignments.last()`
/// reader (the leaf-consignment accessor, the booking path) silently correct-by-accident and every
/// `consignments[0]` reader silently wrong. One tier, one field, no index arithmetic.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ColoredTip {
    /// RGB contract id whose allocation this tip carries. MUST equal the parent segment's.
    pub contract_id: String,
    /// The change's share of the allocation.
    pub amount: u64,
    /// The consignment for the tip's single cap tier.
    pub consignment: String,
}

/// **[CATS change 2 / V4] The sender's own CHANGE leg of a split — the SPINE TIP.**
///
/// Persisted under `spinetip-<statechain_id>` ([`SPINE_TIP_KEY_PREFIX`]), and that separate key is
/// the whole point of the record. A tip is *shaped* like a split child — an un-broadcast funding
/// outpoint `SP.out[K]`, an ancestor segment to walk, a pre-signed exit that pays this wallet's own
/// key — so the cheapest thing to do would have been to write it under `ctesr-`. That would be wrong
/// in both directions at once:
///
/// * a `ctesr-` row is read as a **conveyable leaf** (`ChildTesrBundle`, two tiers, a payee's
///   `child_owner_exit_address`, a `child_extension` every reader dereferences). The tip has one
///   tier and no payee; and
/// * everything keyed `ctesr-` is treated as *someone else's coin that arrived here* — the flat-lane
///   licence, the coloured-carrier exit allowlist, the tower's child loop. A tip is the sender's own
///   change and needs the same treatment for opposite reasons.
///
/// So it gets its own key, its own readers, and — critically — its own entry at every site that
/// enumerates ladder artefacts. A site that knows `tesr-` and `ctesr-` but not `spinetip-` does not
/// merely miss the tip: it concludes something POSITIVE and wrong about it (un-laddered, un-managed
/// wallet, not a carrier, nothing to defend). Those are the fail-open sites co-edited with this type.
///
/// **The cap's CSV is `[p.d_floor, p.d0]`, not `[0, 0]`.** The tip's cap is the sender's slow exit
/// leg at `D0`; it is precisely the transaction the NEXT batch's `SP` (at [`SPINE_CSV`]) must
/// out-race. Pinning it to zero would leave the next batch with no margin at all, and the builders'
/// `s0_csv <= SPINE_CSV` guard would refuse to build that batch — stranding the tip.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SpineTipBundle {
    /// The ROOT parent segment as an ordinary TES-R bundle: `T -> X_m -> SP`, with `SP` the current
    /// (terminal) state paying the pieces, the tip and the P2A anchor.
    pub parent: TesrBundle,
    pub parent_statechain_id: String,
    /// INTERMEDIATE segments, root→leaf, when the tip descends through earlier spine levels. Empty
    /// for a tip carved directly off a root coin. Same `serde(default)` reasoning as
    /// [`ChildTesrBundle::ancestors`]: this record is local, and every row written without the field
    /// is a depth-1 tip.
    #[serde(default)]
    pub ancestors: Vec<ChildSegment>,
    /// Which `SP` output funds the tip — `K`, the LAST payload output of the split.
    pub sp_vout: u32,
    /// The VALUE of `SP.out[sp_vout]` — the amount the cap's taproot sighash commits to, and the
    /// source value the NEXT batch's `SP_{i+1}` carves its payloads out of. Stored rather than
    /// back-computed from `cap.out_value + committed_fee + P2A`, because that reconstruction assumes
    /// the fee rate the cap was built at and would silently drift if it were ever re-read at a
    /// different one.
    pub sp_out_value: u64,
    /// The tip's own statechain id (a fresh SE-registered slot, never funded on chain).
    pub statechain_id: String,
    /// Where the cap pays: this wallet's own exit key.
    pub owner_exit_address: String,
    /// **The one tier.** Spends `SP.out[sp_vout]` directly — there is no extension between them —
    /// and pays [`Self::owner_exit_address`].
    pub cap: TesrTier,
    /// Caps this tip has retired: `C_i` after batch `i+1` replaced it with `SP_{i+1}`. Each must
    /// carry a strictly HIGHER CSV than the live tier over the same outpoint, which is the ordinary
    /// replace-by-lower-timelock invariant and the reason [`SPINE_CSV`] wins by the largest margin.
    #[serde(default)]
    pub superseded_caps: Vec<TesrTier>,
    /// The parent segment's flat (signed-once) backup chain — the same conveyed census term
    /// [`ChildTesrBundle::parent_flat_backups`] carries, kept here because a tip that is later handed
    /// over becomes a conveyed child and needs it.
    #[serde(default)]
    pub parent_flat_backups: Vec<mercurylib::wallet::BackupTx>,
    /// Present iff this tip carries an RGB allocation (the change leg of a COLOURED split).
    #[serde(default)]
    pub rgb: Option<ColoredTip>,
}

impl SpineTipBundle {
    /// True iff this tip carries an RGB allocation.
    pub fn is_colored(&self) -> bool {
        self.rgb.is_some()
    }

    /// The `SP` tier whose `out[sp_vout]` funds this tip — the root parent's split state for a
    /// depth-1 tip, the LAST intermediate segment's for a tip that descends through earlier spine
    /// levels. See [`segment_funding_tier`].
    pub fn funding_tier(&self) -> &TesrTier {
        segment_funding_tier(&self.parent, &self.ancestors)
    }

    /// `SP.out[K]` — the un-broadcast outpoint the cap spends, the outpoint the NEXT batch's
    /// `SP_{i+1}` spends instead, and the outpoint an RGB engine has registered while the tip is
    /// off-chain.
    pub fn funding_outpoint(&self) -> (String, u32) {
        (self.funding_tier().txid.clone(), self.sp_vout)
    }

    /// **Is this record self-consistent?** A `SpineTipBundle` is the sender's own write, and every
    /// field of it is later read as fact by something that spends money:
    ///
    /// * `sp_out_value` feeds [`ParentShape::SpineTip::split_source_value`] straight into the NEXT
    ///   batch's payload arithmetic (`rust-sdk/src/transfer.rs`), so a wrong number mis-prices a
    ///   whole batch of payees — silently, because the split builder's own conservation law is
    ///   satisfied by any self-consistent set of amounts;
    /// * `cap.payload_vout` is where a coloured tip's allocation is booked once the exit lands
    ///   ([`colored_exit_move`]), and the P2A anchor sits one output away;
    /// * `funding_outpoint()` is what the RGB engine is told the walk has SPENT.
    ///
    /// None of that has a counterparty to catch it. So the record is checked against ITSELF, and the
    /// arbiter is the signed transaction rather than the neighbouring serde field — the cap's
    /// taproot `SIGHASH_ALL` sighash commits to `previous_output` and `nSequence`, so those two are
    /// facts the writer cannot restate.
    ///
    /// **Structural checks strictly before value checks.** The prevout re-anchor and the payee pin
    /// run first; only then is any output turned into a number. That ordering is the one this file
    /// has lost four times (see `mod linked`), and here it is what makes "the value at
    /// `SP.out[sp_vout]`" a statement about the outpoint the cap provably spends rather than about
    /// an index the writer chose.
    ///
    /// The cap's CSV band is `[d_floor, d0]` and deliberately **not** `[0, 0]`: the cap is the
    /// sender's slow exit leg, and it is precisely what the next batch's `SP` at [`SPINE_CSV`] has
    /// to out-race.
    pub fn validate(&self) -> Result<()> {
        use electrum_client::bitcoin::{consensus::deserialize, Address, Transaction};

        let sid = &self.statechain_id;
        let funding = self.funding_tier();

        // ---- STRUCTURE ------------------------------------------------------------------------
        let cap_raw = hex::decode(&self.cap.signed_tx).map_err(|e| {
            anyhow::anyhow!("spine tip {sid}: the cap's signed tx hex does not decode ({e})")
        })?;
        let cap_tx: Transaction = deserialize(&cap_raw).map_err(|e| {
            anyhow::anyhow!("spine tip {sid}: the cap's signed tx does not parse ({e})")
        })?;
        if cap_tx.txid().to_string() != self.cap.txid {
            return Err(anyhow::anyhow!(
                "spine tip {sid}: the cap's stored tx hashes to {} but the record names {} — every \
                 reader that decides 'has the exit landed?' keys on the NAME, so it would report a \
                 completed exit for a transaction that was never broadcast",
                cap_tx.txid(),
                self.cap.txid
            ));
        }
        if cap_tx.input.len() != 1 {
            return Err(anyhow::anyhow!(
                "spine tip {sid}: the cap has {} inputs — a cap spends exactly its funding \
                 `SP.out[K]` and nothing else",
                cap_tx.input.len()
            ));
        }
        // THE RE-ANCHOR. Derived, not declared: this outpoint is committed by the cap's own
        // SIGHASH_ALL sighash, so it cannot be repointed without invalidating the SE's signature.
        let spends = cap_tx.input[0].previous_output;
        if spends.txid.to_string() != funding.txid || spends.vout != self.sp_vout {
            return Err(anyhow::anyhow!(
                "spine tip {sid}: its cap spends {}:{} but the record declares it funded by \
                 {}:{} — the tip's whole shape (one cap directly over `SP.out[K]`, no extension) \
                 rests on those being the same outpoint",
                spends.txid,
                spends.vout,
                funding.txid,
                self.sp_vout
            ));
        }
        // PIN 2 — the cap pays THIS wallet's exit key at its declared payload index. Pins
        // `payload_vout` before anything reads a value through it; without this the P2A anchor
        // (240 sat) sits one index away and every value law below would compute on it.
        let net = net_from_str(&self.parent.network);
        let owner_spk = Address::from_str(&self.owner_exit_address)
            .map_err(|_| {
                anyhow::anyhow!(
                    "spine tip {sid}: owner_exit_address {} is not a valid address",
                    self.owner_exit_address
                )
            })?
            .require_network(net)
            .map_err(|_| {
                anyhow::anyhow!(
                    "spine tip {sid}: owner_exit_address {} is for the wrong network",
                    self.owner_exit_address
                )
            })?
            .script_pubkey();
        let cap_payload = self.cap.link_pays(
            &cap_tx,
            &owner_spk,
            "spine tip cap",
            &format!(
                "spine tip {sid}: the cap's payload output does not pay the recorded exit address \
                 {} — the change would land somewhere this wallet cannot spend it",
                self.owner_exit_address
            ),
        )?;

        // ---- VALUE (only now) -----------------------------------------------------------------
        if cap_payload.value() != self.cap.out_value {
            return Err(anyhow::anyhow!(
                "spine tip {sid}: the cap declares out_value {} but its payload output carries {}",
                self.cap.out_value,
                cap_payload.value()
            ));
        }
        let sp_raw = hex::decode(&funding.signed_tx).map_err(|e| {
            anyhow::anyhow!("spine tip {sid}: the funding SP's signed tx hex does not decode ({e})")
        })?;
        let sp_tx: Transaction = deserialize(&sp_raw).map_err(|e| {
            anyhow::anyhow!("spine tip {sid}: the funding SP's signed tx does not parse ({e})")
        })?;
        if sp_tx.txid().to_string() != funding.txid {
            return Err(anyhow::anyhow!(
                "spine tip {sid}: the funding SP's stored tx hashes to {} but the segment names {}",
                sp_tx.txid(),
                funding.txid
            ));
        }
        // Safe by the re-anchor above: `sp_vout` is the index the cap's SIGNATURE names, not one
        // this record chose.
        let sp_out = sp_tx.output.get(self.sp_vout as usize).ok_or_else(|| {
            anyhow::anyhow!(
                "spine tip {sid}: the funding SP has no output {} ({} outputs)",
                self.sp_vout,
                sp_tx.output.len()
            )
        })?;
        if sp_out.value != self.sp_out_value {
            return Err(anyhow::anyhow!(
                "spine tip {sid}: sp_out_value is recorded as {} but `SP.out[{}]` carries {} — the \
                 next batch would carve its payees out of an amount that does not exist",
                self.sp_out_value,
                self.sp_vout,
                sp_out.value
            ));
        }

        // ---- THE CSV BAND, read off the SIGNATURE ---------------------------------------------
        let signed_csv = signed_relative_csv(&cap_tx, "spine tip cap")?;
        let csv = mercurylib::transfer::receiver::bind_declared_csv(
            0,
            "spine tip cap",
            self.cap.csv,
            signed_csv,
        )?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "spine tip {sid}: the cap's relative timelock is DISABLED — the cap is the sender's \
                 SLOW leg and must sit in [{}, {}], or the next batch's SP has nothing to out-race",
                self.parent.params.d_floor,
                self.parent.params.d0
            )
        })?;
        let p = &self.parent.params;
        if csv < p.d_floor || csv > p.d0 {
            return Err(anyhow::anyhow!(
                "spine tip {sid}: the cap's signed CSV is {csv}, outside the state band [{}, {}]. \
                 It is NOT pinned to {SPINE_CSV} like a spine tier: a cap at {SPINE_CSV} would \
                 leave the next batch's SP no margin at all, and the builders' `s0_csv <= \
                 {SPINE_CSV}` guard would then refuse to build that batch, stranding the tip",
                p.d_floor,
                p.d0
            ));
        }
        Ok(())
    }
}

/// **Where a COLOURED record's allocation moves when its unilateral exit lands.** Consumed by
/// [`colored_exit_move`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColoredExitMove {
    /// The outpoint the walk's LAST tier pays — where the allocation physically ends up.
    pub tip_txid: String,
    pub tip_vout: u32,
    pub tip_value: u64,
    pub contract_id: String,
    pub amount: u64,
    /// `txid:vout` of the outpoint an RGB engine still has registered and the walk has now SPENT.
    /// Marking the wrong one leaves the allocation counted twice.
    pub spent_outpoint: String,
}

/// The three shapes a coin's off-chain exit material can take — a root ladder (`tesr-`), an adopted
/// split child (`ctesr-`), or a spine tip (`spinetip-`).
///
/// **The point of the enum is the `match`.** Every one of these shapes moves an RGB allocation from
/// one outpoint to another when it exits, and the sites that resolve that move were written as an
/// `if let … else if let … else { None }` chain. A new record shape then does not produce an
/// absence there — it produces `Ok(None)`, which the caller reads as "plain coin, nothing to do",
/// with no event and no error. That is exactly how the coloured spine tip fell through
/// [`colored_exit_move`]'s caller. Routed through an exhaustive `match`, the next shape added is a
/// compile error instead.
pub enum LadderRecord<'a> {
    Root(&'a TesrBundle),
    Child(&'a ChildTesrBundle),
    Tip(&'a SpineTipBundle),
}

/// **What the RGB engine must be told once a COLOURED record's unilateral exit has landed.**
/// `None` for a PLAIN record — a plain walk moves no allocation, so there is nothing to book.
///
/// Until this runs for a record, every UTXO-driven rgb-lib view (`get_asset_balance`,
/// `list_unspents`, `list_allocations`, `blind_receive`) is not merely incomplete but **STALE**: it
/// reports the asset at an outpoint that has been spent on chain and no longer exists. The engine
/// cannot discover the new one by itself — every tier pays a Mercury seed-derived key that is not in
/// its BDK descriptor, so no wallet sync will ever surface it.
///
/// The `spent_outpoint` is the interesting term in all three arms, and it is NOT the coin's `F`
/// except for a root ladder. A child's and a tip's allocation was booked at their own funding
/// `SP.out[j]` when the split was made (the parent's `F` was marked spent then); naming `F` again
/// here would leave the leaf's own allocation counted twice.
pub fn colored_exit_move(rec: LadderRecord<'_>) -> Option<ColoredExitMove> {
    match rec {
        LadderRecord::Root(b) => {
            let rgb = b.rgb.as_ref()?;
            let tip = &b.current().state;
            Some(ColoredExitMove {
                tip_txid: tip.txid.clone(),
                tip_vout: tip.payload_vout,
                tip_value: tip.out_value,
                contract_id: rgb.contract_id.clone(),
                amount: rgb.amount,
                spent_outpoint: format!("{}:{}", b.f_txid, b.f_vout),
            })
        }
        LadderRecord::Child(cb) => {
            let rgb = cb.rgb.as_ref()?;
            let tip = &cb.child_state;
            let (sp_txid, sp_vout) = cb.funding_outpoint();
            Some(ColoredExitMove {
                tip_txid: tip.txid.clone(),
                tip_vout: tip.payload_vout,
                tip_value: tip.out_value,
                contract_id: rgb.contract_id.clone(),
                amount: rgb.amount,
                spent_outpoint: format!("{sp_txid}:{sp_vout}"),
            })
        }
        // [CATS/V4] **The arm the enumerator sweep missed.** A coloured tip is the sender's own
        // change: its allocation sits on the un-broadcast `SP.out[K]` (booked there by the coloured
        // split's own `register_statechain`), and its ONE cap moves it to the sender's exit key.
        // With no arm here the caller returned `Ok(None)` — indistinguishable from "plain coin" —
        // so the walk landed on chain and the engine went on advertising the balance at an outpoint
        // the cap had just spent. No event, no error, a confident wrong answer.
        LadderRecord::Tip(tip) => {
            let rgb = tip.rgb.as_ref()?;
            let (sp_txid, sp_vout) = tip.funding_outpoint();
            Some(ColoredExitMove {
                tip_txid: tip.cap.txid.clone(),
                tip_vout: tip.cap.payload_vout,
                tip_value: tip.cap.out_value,
                contract_id: rgb.contract_id.clone(),
                amount: rgb.amount,
                spent_outpoint: format!("{sp_txid}:{sp_vout}"),
            })
        }
    }
}

/// The wallet-DB key prefix for a [`SpineTipBundle`]. **One spelling, one constant** — every reader
/// that enumerates ladder artefacts must use this rather than a literal, because the failure mode of
/// a missed site is a confident wrong answer, not an absence (see the type's docs).
pub const SPINE_TIP_KEY_PREFIX: &str = "spinetip-";

/// Persist a spine tip under `spinetip-<statechain_id>` (replaces any prior tip for that slot).
///
/// **[`SpineTipBundle::validate`] is a PRECONDITION, not an afterthought.** This is the producer's
/// only door, and it is the last moment at which a mis-shaped tip is still just a value in memory.
/// One write later it is the wallet's own source of truth: `parent_shape` prices the next batch off
/// its `sp_out_value`, `unilateral_exit` broadcasts its cap, and `colored_exit_move` books its
/// allocation — none of which has a counterparty to notice. Refusing here costs the caller an error
/// on a record it can still rebuild; refusing later costs it a batch of payees.
pub async fn persist_spine_tip(
    cc: &ClientConfig,
    wallet_name: &str,
    tip: &SpineTipBundle,
) -> Result<()> {
    tip.validate()?;
    let json = serde_json::to_string(tip)?;
    crate::sqlite_manager::insert_raw_backup_txs(
        &cc.pool,
        wallet_name,
        &format!("{SPINE_TIP_KEY_PREFIX}{}", tip.statechain_id),
        &json,
    )
    .await
}

/// Load a coin's persisted spine-tip record, if any.
pub async fn load_spine_tip(
    cc: &ClientConfig,
    wallet_name: &str,
    statechain_id: &str,
) -> Result<Option<SpineTipBundle>> {
    let key = format!("{SPINE_TIP_KEY_PREFIX}{statechain_id}");
    for (k, json) in crate::sqlite_manager::get_all_backup_txs(&cc.pool, wallet_name).await? {
        if k == key {
            return Ok(Some(serde_json::from_str(&json)?));
        }
    }
    Ok(None)
}

// **DELETED: `spine_tip_sids`.** It was written as the tip-lane sibling of `child_claim_sids` — a
// set of "coins that must be withdrawn by unilateral exit rather than cooperatively" — and it had
// zero callers, in this commit and in principle: `child_claim_sids` itself was RETIRED when the
// candidate-selection exclusion it fed moved into `payment_coins`/`plan_payment`, and the two
// remaining sites that need the same fact (`withdraw`, `parent_shape`) ask about ONE coin and use
// `load_spine_tip`. A `pub` helper with no caller reads as coverage of a case nobody covers; the
// V4 sweep is only worth what its WIRED sites are worth, so an unwired one is worse than absent.

/// Human names for [`spine_tip_exit_chain`]'s entries, in the same order — same lock-step contract as
/// [`child_exit_labels`]: the two loops are reconciled only by a length check, so they are edited
/// together or not at all.
fn spine_tip_exit_labels(tip: &SpineTipBundle) -> Vec<String> {
    let mut v = vec!["parent trigger".to_string()];
    for l in 0..tip.parent.levels.len() {
        v.push(format!("parent level {l} extension"));
        v.push(format!("parent level {l} state"));
    }
    for (i, seg) in tip.ancestors.iter().enumerate() {
        if seg.extension.is_some() {
            v.push(format!("ancestor {i} extension"));
        }
        v.push(format!("ancestor {i} state"));
    }
    v.push("spine tip cap".to_string());
    v
}

/// The tip's full unilateral-exit chain, in broadcast order: `T -> X_m -> SP` (parent segment), every
/// intermediate segment, then the tip's single `cap`. Each entry is `(signed_tx_hex, relative_csv)`.
///
/// ⚠️ Same [B1] warning as [`child_exit_chain`]: the `csv` here is the DECLARED field. It is safe for
/// the broadcast-side callers (this record is the wallet's own write, and they only use it as a
/// wait-time hint) and must never seed an admission decision — use [`spine_tip_exit_chain_bound`].
pub fn spine_tip_exit_chain(tip: &SpineTipBundle) -> Vec<(String, Option<u16>)> {
    let mut chain: Vec<(String, Option<u16>)> =
        tip.parent.exit_tiers().iter().map(|t| (t.signed_tx.clone(), t.csv)).collect();
    for seg in tip.ancestors.iter() {
        if let Some(ext) = &seg.extension {
            chain.push((ext.signed_tx.clone(), ext.csv));
        }
        chain.push((seg.state.signed_tx.clone(), seg.state.csv));
    }
    chain.push((tip.cap.signed_tx.clone(), tip.cap.csv));
    chain
}

/// [B1] The tip's exit chain with every timelock read off the SIGNATURE that enforces it — the
/// sibling of [`child_exit_chain_bound`], and the only form an admission decision may read.
pub fn spine_tip_exit_chain_bound(tip: &SpineTipBundle) -> Result<Vec<(String, Option<u16>)>> {
    use electrum_client::bitcoin::{consensus::deserialize, Transaction};
    let declared = spine_tip_exit_chain(tip);
    let labels = spine_tip_exit_labels(tip);
    if labels.len() != declared.len() {
        return Err(anyhow::anyhow!(
            "internal: spine-tip exit chain has {} tiers but {} labels — refusing to verify a chain \
             this code cannot describe",
            declared.len(),
            labels.len()
        ));
    }
    let mut bound = Vec::with_capacity(declared.len());
    for (i, (signed_hex, declared_csv)) in declared.into_iter().enumerate() {
        let what = &labels[i];
        let raw = hex::decode(&signed_hex)
            .map_err(|e| anyhow::anyhow!("{what}: signed tx hex does not decode ({e})"))?;
        let tx: Transaction = deserialize(&raw)
            .map_err(|e| anyhow::anyhow!("{what}: signed tx does not parse ({e})"))?;
        let signed_csv = signed_relative_csv(&tx, what)?;
        let csv =
            mercurylib::transfer::receiver::bind_declared_csv(i, what, declared_csv, signed_csv)?;
        bound.push((signed_hex, csv));
    }
    Ok(bound)
}

/// **Tower pass for a SPINE TIP** — the tip-lane sibling of [`watch_child_pass`], on the identical
/// contract: `Idle` when the parent's `F` is verifiably unspent, `Blind` when the backend could not
/// be read, and a not-yet-mature tier reported as a `failures` entry rather than as silence.
///
/// Without this the tip is the one slot in the wallet with NO defence at all: it has no `tesr-` row
/// (so `watch_pass`'s loop never sees it) and no `ctesr-` row (so the child loop never sees it), and
/// what it has to survive is the same race a child does — the parent's retained state rivalling `SP`
/// over `X_m.out[0]` — with the sender's entire change riding on it.
pub fn watch_spine_tip_pass(
    electrum: &electrum_client::Client,
    tip: &SpineTipBundle,
) -> WatchState {
    match watch_spine_tip_pass_seen(electrum, tip) {
        Ok(state) => state,
        Err(e) => WatchState::Blind { reason: e.to_string() },
    }
}

fn watch_spine_tip_pass_seen(
    electrum: &electrum_client::Client,
    tip: &SpineTipBundle,
) -> Result<WatchState> {
    use electrum_client::bitcoin::{consensus::deserialize, Transaction};
    // `?` is load-bearing, exactly as in `watch_pass_seen`: a backend that cannot answer "is F
    // spent?" must never be read as "F is unspent".
    if !outpoint_spent(electrum, &tip.parent.f_txid, tip.parent.f_vout)? {
        return Ok(WatchState::Idle);
    }
    let mut ids = Vec::new();
    let mut failures = Vec::new();
    for (signed, _csv) in spine_tip_exit_chain(tip) {
        let raw = hex::decode(&signed)
            .map_err(|e| anyhow::anyhow!("spine-tip exit chain carries unusable signed tx hex: {e}"))?;
        let txid = deserialize::<Transaction>(&raw)
            .map_err(|e| anyhow::anyhow!("spine-tip exit chain tx did not deserialize: {e}"))?
            .txid()
            .to_string();
        if tx_known(electrum, &txid)? {
            continue; // already on-chain / in mempool (often the racer's own T or X_m)
        }
        match electrum.transaction_broadcast_raw(&raw) {
            Ok(_) => ids.push(txid),
            Err(e) => {
                failures.push(format!("{txid}: {e}"));
                break;
            }
        }
    }
    Ok(WatchState::Acted { ids, failures, blind: vec![] })
}

/// **Owner-initiated unilateral exit of a SPINE TIP.** Broadcasts the tip's full pre-co-signed chain
/// in order, each tier once its relative-CSV is met, stopping at the first not-yet-mature one.
/// Keyless and idempotent, exactly like [`exit_child_pass`]; **`Err` = blind**.
pub fn exit_spine_tip_pass(
    electrum: &electrum_client::Client,
    tip: &SpineTipBundle,
) -> Result<ExitProgress> {
    use electrum_client::bitcoin::{consensus::deserialize, Transaction};
    let mut broadcast = Vec::new();
    let mut stalled = None;
    for (signed, _csv) in spine_tip_exit_chain(tip) {
        let raw = hex::decode(&signed)
            .map_err(|e| anyhow::anyhow!("spine-tip exit chain carries unusable signed tx hex: {e}"))?;
        let txid = deserialize::<Transaction>(&raw)
            .map_err(|e| anyhow::anyhow!("spine-tip exit chain tx did not deserialize: {e}"))?
            .txid()
            .to_string();
        if tx_known(electrum, &txid)? {
            continue;
        }
        match electrum.transaction_broadcast_raw(&raw) {
            Ok(_) => broadcast.push(txid),
            Err(e) => {
                stalled = Some(format!("{txid}: {e}"));
                break;
            }
        }
    }
    let complete = tx_known(electrum, &tip.cap.txid)?;
    Ok(ExitProgress { broadcast, complete, stalled })
}

/// The relative-CSV of the first tip-exit tier not yet on-chain (a wait-time hint), or `Ok(None)`
/// once the exit is complete. Reads the SIGNED timelocks ([`spine_tip_exit_chain_bound`]) for the
/// same reason [`next_child_exit_tier`] does. **`Err` = blind**, never a fabricated wait.
pub fn next_spine_tip_exit_tier(
    electrum: &electrum_client::Client,
    tip: &SpineTipBundle,
) -> Result<Option<u16>> {
    use electrum_client::bitcoin::{consensus::deserialize, Transaction};
    for (signed, csv) in spine_tip_exit_chain_bound(tip)? {
        let raw = hex::decode(&signed)
            .map_err(|e| anyhow::anyhow!("spine-tip exit chain carries unusable signed tx hex: {e}"))?;
        let txid = deserialize::<Transaction>(&raw)
            .map_err(|e| anyhow::anyhow!("spine-tip exit chain tx did not deserialize: {e}"))?
            .txid()
            .to_string();
        if !tx_known(electrum, &txid)? {
            return Ok(Some(csv.unwrap_or(0)));
        }
    }
    Ok(None)
}

/// One INTERMEDIATE child segment of a multi-level child chain: the split state that funds the next
/// level, plus the ladder that hangs off it. Its `state` is the split (`SP`-like) tier whose
/// `out[next_vout]` funds the segment below.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ChildSegment {
    /// This segment's statechain id (an ancestor of the leaf child; must be TERMINAL at the SE).
    pub statechain_id: String,
    /// Which output of the PRECEDING segment's `state` funds THIS segment (the preceding segment is
    /// `parent.current().state` for `ancestors[0]`, else `ancestors[i-1].state`).
    pub funding_vout: u32,
    /// The segment's own ladder.
    ///
    /// * `Some(ext)` — a **two-tier** segment (the shape every split child has): `extension` spends
    ///   this segment's funding outpoint, and `state` spends `ext.out[payload_vout]`.
    /// * `None` — a **[CATS] SPINE** segment: ONE tier, and `state` re-anchors directly on the
    ///   segment's own funding outpoint. This is the sender's change leg, whose live tier is the next
    ///   batch's split state `SP_{i+1}` at [`SPINE_CSV`] and whose retained cap `C_i` is disclosed in
    ///   `superseded_states`.
    ///
    /// ⚠️ **This field is a CROSS-CHECKED DECLARATION, never the source of truth.** The verifier
    /// DERIVES the shape from the outpoint `state` actually spends — an outpoint committed by the
    /// taproot `SIGHASH_ALL` sighash and therefore inseparable from the SE's own signature (see the
    /// `None` branch of the ancestor loop in [`verify_child_bundle`], and `ADMISSION-INPUTS.md`).
    /// A two-tier segment re-labelled `None` is refused because its `state` spends its extension's
    /// payload output rather than the funding outpoint, and the re-label cannot be repaired without
    /// invalidating a signature the sender cannot forge.
    ///
    /// ⚠️ **No `#[serde(default)]`, deliberately.** `Option<T>` already deserialises from a MISSING
    /// field, so the house-style `default` would be pure downgrade surface: a conveyed mailbox
    /// message could simply OMIT `extension` and be read as a spine segment. There is nothing to
    /// default — the absent case is already the `None` case.
    pub extension: Option<TesrTier>,
    pub state: TesrTier,
    #[serde(default)]
    pub superseded_states: Vec<TesrTier>,
    /// Extension rungs this segment retired by renewal. **Must be EMPTY when `extension` is `None`** —
    /// a spine segment has no extension rung, so there is no honest writer for this list, and the
    /// verifier refuses a non-empty one. That refusal is free and independent of the prevout
    /// derivation above, and it closes the re-declaration route directly: a two-tier segment
    /// re-labelled as a spine has to put its dropped extension SOMEWHERE, and the census re-balances
    /// exactly if it lands here (`CHILD_V2_BASELINE + 1 + 1 == CHILD_V2_BASELINE + 2 + 0`).
    #[serde(default)]
    pub superseded_extensions: Vec<TesrTier>,
}

/// The SE-authoritative facts a verifier needs about ONE intermediate child segment. Supplied by the
/// caller (fetched from `/info/statechain` + `/statechain/spend_budget`), never taken from the bundle.
#[derive(Debug, Clone)]
pub struct AncestorFacts {
    pub num_sigs: u32,
    pub aggregate_pubkey: Option<String>,
    pub terminal: bool,
}

impl TesrBundle {
    /// The current (deepest) level.
    pub fn current(&self) -> &TesrLevel {
        self.levels.last().expect("a bundle always has >= 1 level")
    }
    /// Rollover depth (0 = a single level).
    pub fn level(&self) -> u32 {
        (self.levels.len() as u32).saturating_sub(1)
    }
    /// The prevout (txid, value) the current level's extension spends: the trigger at level 0, else
    /// the previous level's self-split state.
    fn current_parent(&self) -> (String, u64) {
        let n = self.levels.len();
        if n <= 1 {
            (self.trigger.txid.clone(), self.trigger.out_value)
        } else {
            (self.levels[n - 2].state.txid.clone(), self.levels[n - 2].state.out_value)
        }
    }
    /// **Is this a CTES-R (coloured) ladder?** Every tier of a coloured ladder carries a valid RGB
    /// state transition; every tier of a plain one carries none.
    ///
    /// This is the discriminator every lane that could produce a RIVAL, allocation-destroying spend
    /// must consult. It is deliberately a property of the BUNDLE, not of the coin: a coin can look
    /// like a carrier to one subsystem and not to another, but a ladder either colours its tiers or
    /// it does not.
    pub fn is_colored(&self) -> bool {
        self.rgb.is_some()
    }

    /// All tier txs in unilateral-exit order: trigger, then (extension, state) for each level.
    pub fn exit_tiers(&self) -> Vec<&TesrTier> {
        let mut v = vec![&self.trigger];
        for l in &self.levels {
            v.push(&l.extension);
            v.push(&l.state);
        }
        v
    }

    /// The ladder's tier txids in [`Self::exit_tiers`] order — the off-chain witness list every
    /// consignment of this ladder must be resolved against. NEVER resolve a coloured ladder with the
    /// plain blockchain resolver: each tier is deliberately un-broadcast, the indexer reports it
    /// `Unresolved`, and rgb-lib maps that to `Archived`, silently and irreversibly invalidating the
    /// whole chain (`docs/utexo/CTESR-GATE.md` §2.3/§3.3).
    pub fn ladder_txids(&self) -> Vec<String> {
        self.exit_tiers().iter().map(|t| t.txid.clone()).collect()
    }

    /// The LEAF consignment — the proof for the final state, the one a receiver validates.
    pub fn leaf_consignment(&self) -> Option<&String> {
        self.rgb.as_ref().and_then(|r| r.consignments.last())
    }

    /// **`(txid, payload_vout, blinding)` for every tier of a COLOURED ladder, DERIVED.**
    ///
    /// This is the receiver's half of [`colored_tier_seal`]: everything it needs for
    /// `RgbWallet::accept_ladder` comes out of the conveyed bundle itself — the statechain id, the
    /// tier's role and its relative timelock — so no blinding is ever transmitted, and a sender
    /// cannot talk a receiver into opening a seal of the sender's choosing.
    ///
    /// Refuses a multi-level (rolled-over) coloured ladder: [`rollover`] cannot produce one (it
    /// refuses a coloured bundle outright), so a bundle that claims to be both coloured and
    /// multi-level did not come from this code, and its per-level renewal counters cannot be
    /// reconstructed. Fail CLOSED rather than derive a blinding that opens nothing.
    pub fn colored_tier_seals(&self) -> Result<Vec<(String, u32, u64)>> {
        use crate::rgb::TierRole;
        if !self.is_colored() {
            return Err(anyhow::anyhow!("this ladder is PLAIN — it has no tier seals"));
        }
        if self.levels.len() != 1 {
            return Err(anyhow::anyhow!(
                "a coloured ladder must have exactly one level (found {}) — coloured rollover does \
                 not exist yet, so a multi-level coloured bundle has no derivable seal schedule",
                self.levels.len()
            ));
        }
        let sid = &self.statechain_id;
        let ext = &self.current().extension;
        let state = &self.current().state;
        Ok(vec![
            (
                self.trigger.txid.clone(),
                self.trigger.payload_vout,
                colored_tier_seal(sid, TierRole::Trigger, 0, 0, None).blinding(),
            ),
            (
                ext.txid.clone(),
                ext.payload_vout,
                colored_tier_seal(sid, TierRole::Extension, 0, self.m, ext.csv).blinding(),
            ),
            (
                state.txid.clone(),
                state.payload_vout,
                colored_tier_seal(sid, TierRole::State, 0, self.m, state.csv).blinding(),
            ),
        ])
    }
}

/// **The seal identity of ONE tier of a coloured ladder. Derived by BOTH parties, stored by neither.**
///
/// Rival tiers over the SAME parent output are the NORMAL case in CTES-R — a renewal replaces `X_m`
/// over `T`'s payload output, a transfer replaces `S_k` over `X_m`'s — and two rivals sharing a
/// blinding collapse to one `OpId`/`BundleId`, after which rgb-lib keeps whichever witness has the
/// numerically smallest INTERNAL txid (an arbitrary hash lottery) and the loser's consignment is
/// unvalidatable by anyone (`docs/utexo/CTESR-GATE.md` §2.2). So the derivation must separate rivals,
/// and the receiver must reproduce it exactly from what the transfer message already carries.
///
/// The inputs, and why each one is available to both sides:
///
/// * `statechain_id` — separates coins; the receiver is acting on it.
/// * `role` — separates an extension from a state over the same parent.
/// * `level` — the rollover depth. Always `0` today: [`rollover`] refuses a coloured bundle, so a
///   coloured ladder is single-level by construction and [`TesrBundle::colored_tier_seals`] enforces it.
/// * `m` ‖ `csv`, packed into the `rung` — **this is the pair that actually separates rivals**, and
///   it is not a convention but an invariant the PLAIN protocol already enforces: `verify_bundle`'s
///   per-prevout race check requires the live tier over an outpoint to sit at a strictly LOWER CSV
///   than every superseded rival over that same outpoint (otherwise the stale tier could win the
///   maturity race). Two rivals over one parent output therefore can NEVER share a CSV in a bundle
///   that verifies. The renewal counter `m` is folded in as a second, independent separator.
///
/// Both are cross-checked against transaction CONTENT downstream — `csv` against the tier's nSequence
/// in `verify_bundle_ex`, the whole ladder against the coin in `verify_bundle_bound` — so a bundle
/// that lies about either is rejected before any seal is derived from it.
///
/// `build_colored_tier` additionally asserts at BUILD time that the consignment it just produced
/// carries the tier's OWN witness, so a derivation collision fails loudly at the sender rather than
/// opaquely at the receiver.
pub fn colored_tier_seal(
    statechain_id: &str,
    role: crate::rgb::TierRole,
    level: u32,
    m: u32,
    csv: Option<u16>,
) -> crate::rgb::TierSeal {
    let rung = ((m & 0xFFFF) << 16) | csv.unwrap_or(0) as u32;
    crate::rgb::TierSeal::new(statechain_id, role, level, rung)
}

/// Establish a confirmed coin's TES-R ladder: build + blind-co-sign T → X_0 → S_0 over the funding
/// UTXO `F`. `coin` must be CONFIRMED with utxo/amount/aggregated_address populated.
pub async fn establish(
    cc: &ClientConfig,
    coin: &mut Coin,
    owner_exit_address: &str,
    csv_e: u16,
    csv_d: u16,
    fee_rate: f64,
    network: &str,
) -> Result<TesrBundle> {
    let statechain_id = coin.statechain_id.clone().ok_or_else(|| anyhow::anyhow!("no statechain_id"))?;
    let f_txid = coin.utxo_txid.clone().ok_or_else(|| anyhow::anyhow!("no utxo_txid"))?;
    let f_vout = coin.utxo_vout.ok_or_else(|| anyhow::anyhow!("no utxo_vout"))?;
    let f_value = coin.amount.ok_or_else(|| anyhow::anyhow!("no amount"))? as u64;
    let agg = coin.aggregated_address.clone().ok_or_else(|| anyhow::anyhow!("no aggregated_address"))?;

    let t = mercurylib::tesr::build_trigger(&f_txid, f_vout, f_value, &agg, network, fee_rate)?;
    let t_signed = cosign_tier(cc, coin, t.tx_hex.clone(), f_value, network).await?;
    let x = mercurylib::tesr::build_extension(&t.txid, t.out_value, &agg, network, csv_e, fee_rate)?;
    let x_signed = cosign_tier(cc, coin, x.tx_hex.clone(), t.out_value, network).await?;
    let s = mercurylib::tesr::build_state(&x.txid, x.out_value, owner_exit_address, network, csv_d, fee_rate)?;
    let s_signed = cosign_tier(cc, coin, s.tx_hex.clone(), x.out_value, network).await?;

    Ok(TesrBundle {
        version: TESR_BUNDLE_VERSION,
        statechain_id,
        network: network.to_string(),
        fee_rate,
        agg_address: agg,
        owner_exit_address: owner_exit_address.to_string(),
        f_txid,
        f_vout,
        f_value,
        trigger: TesrTier { txid: t.txid, signed_tx: t_signed, out_value: t.out_value, csv: None, payload_vout: t.payload_vout },
        levels: vec![TesrLevel {
            extension: TesrTier { txid: x.txid, signed_tx: x_signed, out_value: x.out_value, csv: Some(csv_e), payload_vout: x.payload_vout },
            state: TesrTier { txid: s.txid, signed_tx: s_signed, out_value: s.out_value, csv: Some(csv_d), payload_vout: s.payload_vout },
        }],
        m: 0,
        superseded_states: Vec::new(),
        superseded_extensions: Vec::new(),
        params: mercurylib::tesr::TesrParams::for_network(network),
        rgb: None,
    })
}

// =================================================================================================
// CTES-R — colour the ladder.
// =================================================================================================

/// The smallest funding value `F` that can carry a full COLOURED three-tier ladder.
///
/// **This must be checked BEFORE the first `cosign_tier`, and it is the first thing that bites.** A
/// coloured rung costs `colored_committed_fee(1, rate) + P2A_VALUE` — 576 sat at 2 sat/vB versus 490
/// uncoloured ([D4]-corrected on both halves; was 574 vs 488 when both vsize models understated the
/// explicit `SIGHASH_ALL` byte), because the RGB `opret` output serialises to exactly
/// `P2TR_OUT_VBYTES` (43 B) and the
/// fee is `committed_fee_for_outputs(n_payload + 1, rate)` (`docs/utexo/CTESR-GATE.md` §3.4). Three
/// rungs plus a final state output that still clears dust is the floor.
///
/// Discovering this at rung 3 instead would be unrecoverable: `T` and `X_0` would already have burned
/// two IRREVERSIBLE SE co-signs, leaving the coin's `num_sigs` permanently ahead of any bundle that
/// can be persisted — i.e. a coin whose census no receiver can ever balance. Hence a pre-flight gate,
/// not an error at the third `build_colored_tier`.
pub fn colored_ladder_floor(fee_rate_sats_per_vb: f64, dust_limit: u64) -> u64 {
    3 * (crate::rgb::colored_committed_fee(1, fee_rate_sats_per_vb) + mercurylib::tesr::P2A_VALUE)
        + dust_limit
}

/// Dust floor used by [`colored_ladder_floor`] — the final state output must be spendable.
pub const COLORED_LADDER_DUST: u64 = 330;

/// One coloured tier, built and coloured but NOT yet co-signed.
#[derive(Clone, Debug)]
pub struct ColoredTierDraft {
    pub tx_hex: String,
    pub txid: String,
    /// Post-colouring payload index — 1 for the standard one-payload tier, but READ from the
    /// builder's return value, never assumed.
    pub payload_vout: u32,
    pub payload_value: u64,
    pub payload_spk_hex: String,
    pub consignment: String,
}

/// A complete COLOURED ladder, built and coloured, awaiting its three SE co-signs.
///
/// **Why the build and the co-sign are separate phases.** Colouring needs the RGB engine; co-signing
/// needs the network. Interleaving them would mean holding the engine handle across an `await`, and
/// the engine's resolver is `!Sync`, so the resulting future is not `Send` and cannot live in the
/// SDK's background task. Splitting is also free: a tier's txid is stable across signing (a taproot
/// key-spend adds only witness data), so the whole chain can be built before any of it is signed.
#[derive(Clone, Debug)]
pub struct ColoredLadderDraft {
    pub statechain_id: String,
    pub network: String,
    pub fee_rate: f64,
    pub agg_address: String,
    pub owner_exit_address: String,
    pub f_txid: String,
    pub f_vout: u32,
    pub f_value: u64,
    pub csv_e: u16,
    pub csv_d: u16,
    pub contract_id: String,
    pub amount: u64,
    pub trigger: ColoredTierDraft,
    pub extension: ColoredTierDraft,
    pub state: ColoredTierDraft,
}

/// **Build a COLOURED (CTES-R) ladder over an RGB carrier** — `T`, `X_0` and `S_0` each carrying a
/// valid RGB state transition, so laddering the carrier MOVES the allocation instead of destroying
/// it. Synchronous, engine-only, and it co-signs NOTHING.
///
/// The shape differs from [`establish`] in exactly three ways, and in nothing else:
///
/// 1. every tier is built by [`crate::rgb::build_colored_tier`] rather than
///    `mercurylib::tesr::build_{trigger,extension,state}`, so it carries an `opret` commitment and a
///    per-tier seal blinding derived from [`crate::rgb::TierSeal`];
/// 2. the payload therefore sits at **vout 1**, not 0 — and that value is taken from the builder's
///    RETURNED vout, never assumed (`opreturn_first` is triggered by the P2TR payload output, not by
///    the P2A anchor, so "the opret is index 0" is a consequence, not a rule);
/// 3. the committed fee is `committed_fee_for_outputs(n_payload + 1, rate)`.
///
/// Everything else — the co-sign flow, the tier CSVs, the census, `verify_bundle_bound` — is byte
/// for byte the plain path. The SE stays BLIND: a coloured tier is still one input, one sighash, one
/// [`cosign_tier`], so colouring adds **zero** SE co-signs and the census arithmetic is unchanged.
///
/// Rival tiers over one parent output are separated by the per-tier [`crate::rgb::TierSeal`]
/// blinding, and `build_colored_tier` asserts at build time that each consignment carries its own
/// tier's witness — so a derivation collision fails HERE, loudly, rather than at a receiver that can
/// only report "not known to the resolver" (`docs/utexo/CTESR-GATE.md` §3.1).
///
/// `f_spk_hex` is `tx0.output[f_vout].script_pubkey` read from the chain — the prevout rgb-lib needs
/// to know which allocation is being spent, and the value the trigger's sighash commits to.
#[allow(clippy::too_many_arguments)]
pub fn build_colored_ladder(
    rgb: &mercury_rgb::RgbWallet,
    coin: &Coin,
    owner_exit_address: &str,
    csv_e: u16,
    csv_d: u16,
    fee_rate: f64,
    network: &str,
    contract_id: &str,
    rgb_amount: u64,
    f_spk_hex: &str,
) -> Result<ColoredLadderDraft> {
    use crate::rgb::{
        build_colored_tier, colored_tier_out_value, ColoredTier, ColoredTierSpec, TierRole,
    };

    let statechain_id =
        coin.statechain_id.clone().ok_or_else(|| anyhow::anyhow!("no statechain_id"))?;
    let f_txid = coin.utxo_txid.clone().ok_or_else(|| anyhow::anyhow!("no utxo_txid"))?;
    let f_vout = coin.utxo_vout.ok_or_else(|| anyhow::anyhow!("no utxo_vout"))?;
    let f_value = coin.amount.ok_or_else(|| anyhow::anyhow!("no amount"))? as u64;
    let agg =
        coin.aggregated_address.clone().ok_or_else(|| anyhow::anyhow!("no aggregated_address"))?;
    if rgb_amount == 0 {
        return Err(anyhow::anyhow!(
            "refusing to colour a ladder for a zero allocation — a coloured tier must assign a \
             non-zero amount"
        ));
    }

    // PRE-FLIGHT AFFORDABILITY. Ahead of everything; see `colored_ladder_floor`.
    let floor = colored_ladder_floor(fee_rate, COLORED_LADDER_DUST);
    if f_value < floor {
        return Err(anyhow::anyhow!(
            "carrier {statechain_id} holds {f_value} sat but a COLOURED three-tier ladder needs at \
             least {floor} sat at {fee_rate} sat/vB (each coloured rung costs {} sat: fee {} + \
             anchor {}) — refusing before any SE co-sign, because a ladder abandoned at rung 3 \
             leaves num_sigs permanently ahead of any persistable bundle",
            crate::rgb::colored_committed_fee(1, fee_rate) + mercurylib::tesr::P2A_VALUE,
            crate::rgb::colored_committed_fee(1, fee_rate),
            mercurylib::tesr::P2A_VALUE
        ));
    }

    let draft_of = |t: ColoredTier| ColoredTierDraft {
        tx_hex: t.tx_hex,
        txid: t.txid,
        payload_vout: t.payloads[0].vout,
        payload_value: t.payloads[0].value,
        payload_spk_hex: t.payloads[0].script_pubkey_hex.clone(),
        consignment: t.consignment,
    };

    // ---- TRIGGER: spends F, no relative timelock, pays A, carries the whole allocation. ----------
    let t_value = colored_tier_out_value(f_value, fee_rate)
        .ok_or_else(|| anyhow::anyhow!("F ({f_value} sat) cannot carry a coloured trigger"))?;
    let trigger = draft_of(build_colored_tier(
        rgb,
        &ColoredTierSpec {
            contract_id,
            prev_txid: &f_txid,
            prev_vout: f_vout,
            prev_value: f_value,
            prev_spk_hex: f_spk_hex,
            sequence: mercurylib::tesr::TRIGGER_SEQUENCE.0,
            payloads: &[(agg.clone(), t_value, rgb_amount)],
            network,
            fee_rate,
            // Belt-and-braces second separator (CTESR-GATE §3.1). The TierSeal is what the receiver
            // re-derives; the nonce only makes a collision even less reachable.
            nonce: Some(0),
        },
        &colored_tier_seal(&statechain_id, TierRole::Trigger, 0, 0, None),
    )?);

    // ---- EXTENSION X_0: spends T's PAYLOAD output (vout 1, as returned), pays A. -----------------
    let x_value = colored_tier_out_value(trigger.payload_value, fee_rate)
        .ok_or_else(|| anyhow::anyhow!("the coloured trigger cannot carry an extension"))?;
    let extension = draft_of(build_colored_tier(
        rgb,
        &ColoredTierSpec {
            contract_id,
            prev_txid: &trigger.txid,
            prev_vout: trigger.payload_vout,
            prev_value: trigger.payload_value,
            prev_spk_hex: &trigger.payload_spk_hex,
            sequence: mercurylib::tesr::csv_blocks(csv_e).0,
            payloads: &[(agg.clone(), x_value, rgb_amount)],
            network,
            fee_rate,
            nonce: Some(csv_e as u64),
        },
        &colored_tier_seal(&statechain_id, TierRole::Extension, 0, 0, Some(csv_e)),
    )?);

    // ---- STATE S_0: spends X_0's PAYLOAD output, pays the OWNER's exit key. ----------------------
    let s_value = colored_tier_out_value(extension.payload_value, fee_rate)
        .ok_or_else(|| anyhow::anyhow!("the coloured extension cannot carry a state"))?;
    let state = draft_of(build_colored_tier(
        rgb,
        &ColoredTierSpec {
            contract_id,
            prev_txid: &extension.txid,
            prev_vout: extension.payload_vout,
            prev_value: extension.payload_value,
            prev_spk_hex: &extension.payload_spk_hex,
            sequence: mercurylib::tesr::csv_blocks(csv_d).0,
            payloads: &[(owner_exit_address.to_string(), s_value, rgb_amount)],
            network,
            fee_rate,
            nonce: Some(csv_d as u64),
        },
        &colored_tier_seal(&statechain_id, TierRole::State, 0, 0, Some(csv_d)),
    )?);

    Ok(ColoredLadderDraft {
        statechain_id,
        network: network.to_string(),
        fee_rate,
        agg_address: agg,
        owner_exit_address: owner_exit_address.to_string(),
        f_txid,
        f_vout,
        f_value,
        csv_e,
        csv_d,
        contract_id: contract_id.to_string(),
        amount: rgb_amount,
        trigger,
        extension,
        state,
    })
}

/// [`build_colored_ladder`] on the network's canonical schedule — the coloured sibling of
/// [`establish_auto`]'s parameter choice.
pub fn build_colored_ladder_auto(
    rgb: &mercury_rgb::RgbWallet,
    coin: &Coin,
    owner_exit_address: &str,
    network: &str,
    contract_id: &str,
    rgb_amount: u64,
    f_spk_hex: &str,
) -> Result<ColoredLadderDraft> {
    let p = mercurylib::tesr::TesrParams::for_network(network);
    build_colored_ladder(
        rgb,
        coin,
        owner_exit_address,
        p.ext_csv(0),
        p.state_csv(0),
        p.committed_fee_rate,
        network,
        contract_id,
        rgb_amount,
        f_spk_hex,
    )
}

/// Blind-co-sign the three tiers of a [`ColoredLadderDraft`] and assemble the persistable
/// [`TesrBundle`]. Exactly three `cosign_tier` round-trips — the same number, in the same order, as
/// the plain [`establish`]. The SE never learns that anything is coloured.
///
/// ## The census, spelled out (it is the thing most likely to be got wrong)
///
/// `verify_bundle_bound` enforces `se_num_sigs == flat_backups + tiers + superseded`. For a coloured
/// coin every term is IDENTICAL to a plain one:
///
/// * `flat_backups` STAYS the deposit-anchored chain length. It is tempting to reason "a coloured
///   coin's flat backup is an RGB-unaware spend of `F`, so a coloured ladder must not carry one" and
///   pass 0 — but `tx1` was co-signed at deposit-init, before the coin had any RGB on it, and that
///   co-sign is permanent and un-retractable. Passing 0 makes `expected = 3` against a live
///   `num_sigs` of 4, and EVERY coloured coin then dies at claim with "num_sigs mismatch". There is
///   no way to un-count `tx1`.
/// * `tiers` is 3, because colouring adds no co-sign: one input, one sighash, one `cosign_tier`.
/// * `superseded` is 0 at establish.
///
/// So: deposit `1 = 1 + 0 + 0`; after this call `4 = 1 + 3 + 0`. Balanced, and identical to plain.
///
/// The retained flat backups are still a live, allocation-destroying spend of `F` that the owner
/// holds — they must never be the recommended exit for a coloured coin, and `unilateral_exit`'s
/// non-laddered fallback must never reach one. That is a SAFETY property of the exit path, not an
/// arithmetic one, and it is why the coloured coin's other spend lanes are refused (see
/// [`refuse_uncolored_over_colored`] and the SDK's colored-split interlock).
pub async fn cosign_colored_ladder(
    cc: &ClientConfig,
    coin: &mut Coin,
    draft: ColoredLadderDraft,
) -> Result<TesrBundle> {
    let t_signed =
        cosign_tier(cc, coin, draft.trigger.tx_hex.clone(), draft.f_value, &draft.network).await?;
    let x_signed = cosign_tier(
        cc,
        coin,
        draft.extension.tx_hex.clone(),
        draft.trigger.payload_value,
        &draft.network,
    )
    .await?;
    let s_signed = cosign_tier(
        cc,
        coin,
        draft.state.tx_hex.clone(),
        draft.extension.payload_value,
        &draft.network,
    )
    .await?;

    let bundle = TesrBundle {
        version: TESR_BUNDLE_VERSION,
        statechain_id: draft.statechain_id,
        network: draft.network.clone(),
        fee_rate: draft.fee_rate,
        agg_address: draft.agg_address,
        owner_exit_address: draft.owner_exit_address,
        f_txid: draft.f_txid,
        f_vout: draft.f_vout,
        f_value: draft.f_value,
        trigger: TesrTier {
            txid: draft.trigger.txid,
            signed_tx: t_signed,
            out_value: draft.trigger.payload_value,
            csv: None,
            payload_vout: draft.trigger.payload_vout,
        },
        levels: vec![TesrLevel {
            extension: TesrTier {
                txid: draft.extension.txid,
                signed_tx: x_signed,
                out_value: draft.extension.payload_value,
                csv: Some(draft.csv_e),
                payload_vout: draft.extension.payload_vout,
            },
            state: TesrTier {
                txid: draft.state.txid,
                signed_tx: s_signed,
                out_value: draft.state.payload_value,
                csv: Some(draft.csv_d),
                payload_vout: draft.state.payload_vout,
            },
        }],
        m: 0,
        superseded_states: Vec::new(),
        superseded_extensions: Vec::new(),
        params: mercurylib::tesr::TesrParams::for_network(&draft.network),
        rgb: Some(ColoredLadder {
            contract_id: draft.contract_id,
            amount: draft.amount,
            // exit-tier order: [trigger, ext_0, state_0]
            consignments: vec![
                draft.trigger.consignment,
                draft.extension.consignment,
                draft.state.consignment,
            ],
        }),
    };
    // Self-check with the SAME predicate a receiver runs. Colouring shifts every payload vout, so a
    // mis-threaded index would only surface at the far end; catch it here, while the co-signs are
    // still ours to explain. `flat_backups = 1` (the deposit-anchored tx1) + 3 tiers + 0 superseded.
    verify_bundle(&bundle, 4, 1).map_err(|e| {
        anyhow::anyhow!("the coloured ladder just built does not verify as an exit chain: {e}")
    })?;
    Ok(bundle)
}

/// **The CTES-R interlock.** Refuse an operation that would build an UNCOLOURED tier over a coloured
/// ladder — the shape that destroys the allocation on first exit.
///
/// Every renewal/rollover/transfer/split path replaces a tier over an existing parent output. On a
/// plain ladder that is routine. On a COLOURED ladder an uncoloured replacement is an
/// allocation-destroying spend of a sealed UTXO, and it would be **silent**: the transaction is
/// perfectly valid Bitcoin and every existing check passes. There is no coloured builder for those
/// paths yet, so they refuse.
pub fn refuse_uncolored_over_colored(bundle: &TesrBundle, what: &str) -> Result<()> {
    if bundle.is_colored() {
        return Err(anyhow::anyhow!(
            "{what}: this coin's ladder is COLOURED (CTES-R) and {what} would build an UNCOLOURED \
             tier over a sealed output, destroying the RGB allocation. Refusing — use the coloured \
             replacement path instead: `build_colored_in_ladder_split` + \
             `cosign_colored_in_ladder_split` to pay part of the allocation (SDK: \
             `transfer_tokens`), `build_colored_receiver_state` to convey the whole carrier, or \
             `build_colored_renewal` to renew."
        ));
    }
    Ok(())
}

// =================================================================================================
// CTES-R — colour the RIVAL-TIER paths: renewal and transfer.
//
// Renewal replaces `X_m` over `T`'s payload output; a transfer co-signs a fresh `S_k` one delta
// LOWER over `X_m`'s payload output and discloses the replaced state as superseded. Both produce
// RIVAL transitions over ONE parent outpoint — the case CTESR-GATE §2.2 proved collapses without
// per-tier blinding. `colored_tier_seal` is what separates them, and the rung it derives from
// (`m ‖ csv`) is exactly the pair the plain protocol already forces to differ between rivals.
//
// Every builder here is SYNCHRONOUS and engine-only; every co-signer is async and network-only. The
// split is not cosmetic: the RGB engine's resolver is `!Sync`, so holding its guard across an
// `await` makes the whole future non-`Send` and it cannot live in the SDK's background task. It is
// free, because a tier's txid is stable across signing.
// =================================================================================================

// `tier_payload_prevout` — the builders' payload accessor — used to live here. It moved into `mod
// linked` beside `TesrTier::payload_out`, so that the raw accessor has ZERO users outside that
// module and the encapsulation is a fact about the code rather than a convention. Its contract and
// the trusted-vs-untrusted argument are documented on the function itself; the four call sites in
// this section are unchanged.

/// A COLOURED replacement STATE, built but NOT co-signed — the receiver-paying `S'` of a coloured
/// transfer, or any other state that rivals the current one over the extension's payload output.
#[derive(Clone, Debug)]
pub struct ColoredStateDraft {
    pub statechain_id: String,
    /// The plain address this state pays, already resolved from the recipient's transfer address.
    pub payee: String,
    /// The new state's relative timelock — strictly LOWER than the state it replaces, which is both
    /// the protocol's race rule and (via the seal rung) what keeps the two transitions apart.
    pub csv: u16,
    /// The extension whose payload output this state spends, for the co-signer's fail-closed recheck.
    pub parent_txid: String,
    pub parent_vout: u32,
    pub parent_value: u64,
    pub tier: ColoredTierDraft,
}

/// **Colour the receiver-paying state `S'` of a transfer.** The coloured sibling of
/// [`presign_receiver_state`]'s build half: engine-only, synchronous, co-signs nothing.
///
/// `S'` is a RIVAL of the sender's own current state over `X_m`'s payload output. It is separated
/// from it by the seal rung, which folds in the new CSV — and the new CSV is strictly lower by
/// construction (`cur − δ`), because the protocol already requires the receiver's state to mature
/// FIRST or the sender's retained state could win the race.
pub fn build_colored_receiver_state(
    rgb: &mercury_rgb::RgbWallet,
    bundle: &TesrBundle,
    recipient_address: &str,
) -> Result<ColoredStateDraft> {
    use crate::rgb::{build_colored_tier, colored_tier_out_value, ColoredTierSpec, TierRole};

    if !bundle.is_colored() {
        return Err(anyhow::anyhow!(
            "build_colored_receiver_state: this coin's ladder is PLAIN — use presign_receiver_state"
        ));
    }
    // Reuses the coloured seal schedule's single-level invariant (and its reasoning).
    let _ = bundle.colored_tier_seals()?;
    let rgb_half = bundle.rgb.as_ref().expect("is_colored");
    let p = bundle.params;
    let cur_csv = bundle
        .current()
        .state
        .csv
        .ok_or_else(|| anyhow::anyhow!("current state has no CSV"))?;
    let new_csv = cur_csv
        .checked_sub(p.delta)
        .filter(|c| *c >= p.d_floor)
        .ok_or_else(|| {
            anyhow::anyhow!("state CSV at the floor — renew before transferring this carrier")
        })?;
    let payee = mercurylib::tesr::payee_address(recipient_address, &bundle.network)?;

    let ext = bundle.current().extension.clone();
    let (parent_value, parent_spk) = tier_payload_prevout(&ext, "coloured transfer parent")?;
    let s_value = colored_tier_out_value(parent_value, bundle.fee_rate).ok_or_else(|| {
        anyhow::anyhow!("the coloured extension ({parent_value} sat) cannot carry another state")
    })?;
    let seal = colored_tier_seal(
        &bundle.statechain_id,
        TierRole::State,
        0,
        bundle.m,
        Some(new_csv),
    );
    let tier = build_colored_tier(
        rgb,
        &ColoredTierSpec {
            contract_id: &rgb_half.contract_id,
            prev_txid: &ext.txid,
            prev_vout: ext.payload_vout,
            prev_value: parent_value,
            prev_spk_hex: &parent_spk,
            sequence: mercurylib::tesr::csv_blocks(new_csv).0,
            payloads: &[(payee.clone(), s_value, rgb_half.amount)],
            network: &bundle.network,
            fee_rate: bundle.fee_rate,
            nonce: Some(seal.rung as u64),
        },
        &seal,
    )?;
    Ok(ColoredStateDraft {
        statechain_id: bundle.statechain_id.clone(),
        payee,
        csv: new_csv,
        parent_txid: ext.txid,
        parent_vout: ext.payload_vout,
        parent_value,
        tier: ColoredTierDraft {
            tx_hex: tier.tx_hex,
            txid: tier.txid,
            payload_vout: tier.payloads[0].vout,
            payload_value: tier.payloads[0].value,
            payload_spk_hex: tier.payloads[0].script_pubkey_hex.clone(),
            consignment: tier.consignment,
        },
    })
}

/// Blind-co-sign a [`ColoredStateDraft`] and return the AUGMENTED bundle to convey — the coloured
/// sibling of [`presign_receiver_state`]'s co-sign half. Exactly one `cosign_tier` round-trip, the
/// same as the plain path; the SE never learns that anything is coloured.
///
/// Everything the draft asserts is RE-CHECKED here against the bundle and the recipient address the
/// caller actually asked for, because a draft is built in one place (the SDK, holding the engine) and
/// consumed in another. A mismatch is a refusal, never a rebuild.
pub async fn cosign_colored_receiver_state(
    cc: &ClientConfig,
    coin: &Coin,
    bundle: &TesrBundle,
    draft: ColoredStateDraft,
    recipient_address: &str,
) -> Result<TesrBundle> {
    let rgb_half = bundle
        .rgb
        .clone()
        .ok_or_else(|| anyhow::anyhow!("cosign_colored_receiver_state on a PLAIN ladder"))?;
    if draft.statechain_id != bundle.statechain_id {
        return Err(anyhow::anyhow!(
            "coloured state draft is for {} but the bundle is {}",
            draft.statechain_id,
            bundle.statechain_id
        ));
    }
    let want_payee = mercurylib::tesr::payee_address(recipient_address, &bundle.network)?;
    if draft.payee != want_payee {
        return Err(anyhow::anyhow!(
            "coloured state draft pays {} but this transfer is to {want_payee} — refusing",
            draft.payee
        ));
    }
    let ext = bundle.current().extension.clone();
    if draft.parent_txid != ext.txid || draft.parent_vout != ext.payload_vout {
        return Err(anyhow::anyhow!(
            "coloured state draft spends {}:{} but the ladder's current extension output is {}:{}",
            draft.parent_txid,
            draft.parent_vout,
            ext.txid,
            ext.payload_vout
        ));
    }
    let cur_csv = bundle
        .current()
        .state
        .csv
        .ok_or_else(|| anyhow::anyhow!("current state has no CSV"))?;
    if draft.csv >= cur_csv {
        return Err(anyhow::anyhow!(
            "coloured state draft's CSV {} does not out-race the state it replaces ({cur_csv})",
            draft.csv
        ));
    }

    let mut c = coin.clone();
    let s_signed =
        cosign_tier(cc, &mut c, draft.tier.tx_hex.clone(), draft.parent_value, &bundle.network)
            .await?;

    let mut b = bundle.clone();
    b.owner_exit_address = draft.payee;
    let last = b.levels.len() - 1;
    // Full disclosure, exactly as the plain path: the sender's own (now stale) state was co-signed,
    // so it must stay counted — and it sits at a HIGHER CSV than S', so it loses the maturity race.
    b.superseded_states.push(b.levels[last].state.clone());
    b.levels[last].state = TesrTier {
        txid: draft.tier.txid,
        signed_tx: s_signed,
        out_value: draft.tier.payload_value,
        csv: Some(draft.csv),
        payload_vout: draft.tier.payload_vout,
    };
    // The leaf consignment is replaced, not appended: `consignments` is indexed by `exit_tiers()`.
    let mut consignments = rgb_half.consignments.clone();
    let n = consignments.len();
    if n == 0 {
        return Err(anyhow::anyhow!("coloured ladder carries no consignments"));
    }
    consignments[n - 1] = draft.tier.consignment;
    b.rgb = Some(ColoredLadder { consignments, ..rgb_half });
    Ok(b)
}

/// A COLOURED renewal — a fresh extension `X_{m+1}` over `T`'s payload output plus the state that
/// hangs off it, built but NOT co-signed.
#[derive(Clone, Debug)]
pub struct ColoredRenewalDraft {
    pub statechain_id: String,
    pub csv_e: u16,
    pub csv_d: u16,
    /// The renewal counter the seals were derived at (`bundle.m + 1`).
    pub m: u32,
    pub parent_txid: String,
    pub parent_vout: u32,
    pub parent_value: u64,
    pub extension: ColoredTierDraft,
    pub state: ColoredTierDraft,
}

/// **Colour an off-chain RENEWAL.** The new extension `X_{m+1}` is a RIVAL of `X_m` over `T`'s
/// payload output — the textbook case CTESR-GATE §2.2 measured collapsing under a shared blinding.
/// Two independent things separate them here: the renewal counter and the (strictly lower) CSV, both
/// folded into the seal rung, and both re-derivable by a receiver from the bundle it is handed.
pub fn build_colored_renewal(
    rgb: &mercury_rgb::RgbWallet,
    bundle: &TesrBundle,
    csv_e_new: u16,
    csv_d: u16,
) -> Result<ColoredRenewalDraft> {
    use crate::rgb::{build_colored_tier, colored_tier_out_value, ColoredTierSpec, TierRole};

    if !bundle.is_colored() {
        return Err(anyhow::anyhow!("build_colored_renewal: this coin's ladder is PLAIN"));
    }
    let _ = bundle.colored_tier_seals()?;
    let rgb_half = bundle.rgb.as_ref().expect("is_colored");
    let cur_csv_e = bundle
        .current()
        .extension
        .csv
        .ok_or_else(|| anyhow::anyhow!("current extension has no CSV"))?;
    if csv_e_new >= cur_csv_e {
        return Err(anyhow::anyhow!(
            "a renewal's extension CSV ({csv_e_new}) must be strictly lower than the one it \
             replaces ({cur_csv_e}) — otherwise the superseded extension can still win the race \
             for T's payload output, and the two transitions would not be separated either"
        ));
    }
    // The renewal's extension rivals X_m over the TRIGGER's payload output (single-level ladder).
    let (parent_value, parent_spk) =
        tier_payload_prevout(&bundle.trigger, "coloured renewal parent")?;
    let m_new = bundle.m + 1;

    let x_value = colored_tier_out_value(parent_value, bundle.fee_rate).ok_or_else(|| {
        anyhow::anyhow!("the coloured trigger ({parent_value} sat) cannot carry another extension")
    })?;
    let x_seal =
        colored_tier_seal(&bundle.statechain_id, TierRole::Extension, 0, m_new, Some(csv_e_new));
    let x = build_colored_tier(
        rgb,
        &ColoredTierSpec {
            contract_id: &rgb_half.contract_id,
            prev_txid: &bundle.trigger.txid,
            prev_vout: bundle.trigger.payload_vout,
            prev_value: parent_value,
            prev_spk_hex: &parent_spk,
            sequence: mercurylib::tesr::csv_blocks(csv_e_new).0,
            payloads: &[(bundle.agg_address.clone(), x_value, rgb_half.amount)],
            network: &bundle.network,
            fee_rate: bundle.fee_rate,
            nonce: Some(x_seal.rung as u64),
        },
        &x_seal,
    )?;

    let s_value = colored_tier_out_value(x.payloads[0].value, bundle.fee_rate)
        .ok_or_else(|| anyhow::anyhow!("the renewed coloured extension cannot carry a state"))?;
    let s_seal = colored_tier_seal(&bundle.statechain_id, TierRole::State, 0, m_new, Some(csv_d));
    let s = build_colored_tier(
        rgb,
        &ColoredTierSpec {
            contract_id: &rgb_half.contract_id,
            prev_txid: &x.txid,
            prev_vout: x.payloads[0].vout,
            prev_value: x.payloads[0].value,
            prev_spk_hex: &x.payloads[0].script_pubkey_hex,
            sequence: mercurylib::tesr::csv_blocks(csv_d).0,
            payloads: &[(bundle.owner_exit_address.clone(), s_value, rgb_half.amount)],
            network: &bundle.network,
            fee_rate: bundle.fee_rate,
            nonce: Some(s_seal.rung as u64),
        },
        &s_seal,
    )?;

    let draft_of = |t: crate::rgb::ColoredTier| ColoredTierDraft {
        tx_hex: t.tx_hex,
        txid: t.txid,
        payload_vout: t.payloads[0].vout,
        payload_value: t.payloads[0].value,
        payload_spk_hex: t.payloads[0].script_pubkey_hex.clone(),
        consignment: t.consignment,
    };
    Ok(ColoredRenewalDraft {
        statechain_id: bundle.statechain_id.clone(),
        csv_e: csv_e_new,
        csv_d,
        m: m_new,
        parent_txid: bundle.trigger.txid.clone(),
        parent_vout: bundle.trigger.payload_vout,
        parent_value,
        extension: draft_of(x),
        state: draft_of(s),
    })
}

/// [`build_colored_renewal`] on the network's canonical schedule — the coloured sibling of
/// [`renew_auto`]'s parameter choice.
pub fn build_colored_renewal_auto(
    rgb: &mercury_rgb::RgbWallet,
    bundle: &TesrBundle,
) -> Result<ColoredRenewalDraft> {
    let p = bundle.params;
    let next_m = (bundle.m + 1) as u16;
    build_colored_renewal(rgb, bundle, p.ext_csv(next_m), p.state_csv(0))
}

/// Blind-co-sign a [`ColoredRenewalDraft`] into `bundle` — two `cosign_tier` round-trips, exactly as
/// the plain [`renew`]. Persist the bundle afterwards.
pub async fn cosign_colored_renewal(
    cc: &ClientConfig,
    coin: &mut Coin,
    bundle: &mut TesrBundle,
    draft: ColoredRenewalDraft,
) -> Result<()> {
    let rgb_half = bundle
        .rgb
        .clone()
        .ok_or_else(|| anyhow::anyhow!("cosign_colored_renewal on a PLAIN ladder"))?;
    if draft.statechain_id != bundle.statechain_id {
        return Err(anyhow::anyhow!(
            "coloured renewal draft is for {} but the bundle is {}",
            draft.statechain_id,
            bundle.statechain_id
        ));
    }
    if draft.parent_txid != bundle.trigger.txid || draft.parent_vout != bundle.trigger.payload_vout {
        return Err(anyhow::anyhow!(
            "coloured renewal draft spends {}:{} but the ladder's trigger output is {}:{}",
            draft.parent_txid,
            draft.parent_vout,
            bundle.trigger.txid,
            bundle.trigger.payload_vout
        ));
    }
    if draft.m != bundle.m + 1 {
        return Err(anyhow::anyhow!(
            "coloured renewal draft derived its seals at m={} but this bundle is at m={} — the \
             receiver would derive a different blinding and could not open the ladder",
            draft.m,
            bundle.m
        ));
    }

    let x_signed = cosign_tier(
        cc,
        coin,
        draft.extension.tx_hex.clone(),
        draft.parent_value,
        &bundle.network,
    )
    .await?;
    let s_signed = cosign_tier(
        cc,
        coin,
        draft.state.tx_hex.clone(),
        draft.extension.payload_value,
        &bundle.network,
    )
    .await?;

    let last = bundle.levels.len() - 1;
    bundle.superseded_extensions.push(bundle.levels[last].extension.clone());
    bundle.superseded_states.push(bundle.levels[last].state.clone());
    bundle.levels[last] = TesrLevel {
        extension: TesrTier {
            txid: draft.extension.txid,
            signed_tx: x_signed,
            out_value: draft.extension.payload_value,
            csv: Some(draft.csv_e),
            payload_vout: draft.extension.payload_vout,
        },
        state: TesrTier {
            txid: draft.state.txid,
            signed_tx: s_signed,
            out_value: draft.state.payload_value,
            csv: Some(draft.csv_d),
            payload_vout: draft.state.payload_vout,
        },
    };
    bundle.m = draft.m;
    // exit-tier order: [trigger, ext, state] — the trigger's consignment is untouched.
    bundle.rgb = Some(ColoredLadder {
        consignments: vec![
            rgb_half
                .consignments
                .first()
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("coloured ladder carries no trigger consignment"))?,
            draft.extension.consignment,
            draft.state.consignment,
        ],
        ..rgb_half
    });
    Ok(())
}

/// Establish a coin's ladder using its network's canonical [`TesrParams`] schedule (initial extension
/// `E0` and state `D0`), instead of hand-picked CSVs — the production entry point.
pub async fn establish_auto(
    cc: &ClientConfig,
    coin: &mut Coin,
    owner_exit_address: &str,
    network: &str,
) -> Result<TesrBundle> {
    let p = mercurylib::tesr::TesrParams::for_network(network);
    establish(cc, coin, owner_exit_address, p.ext_csv(0), p.state_csv(0), p.committed_fee_rate, network).await
}

/// [in-ladder split] A split child's headless ladder: extension spends `SP.out[j]`, state spends the
/// extension's `out[0]` and pays the owner. No trigger — `SP` is un-broadcast, so the child's clock
/// only starts once `SP` confirms on a unilateral exit. Both tiers are co-signed under the CHILD's
/// aggregate (`child_coin.aggregated_address` == `SP.out[j]`'s scriptPubKey).
pub struct ChildLadder {
    pub extension: TesrTier,
    pub state: TesrTier,
}

/// [in-ladder split] Co-sign a split child's headless ladder (extension + owner state) rooting at
/// `SP.out[sp_vout]`. The child coin is a fresh statechain node whose aggregate is `SP.out[j]`'s key;
/// this is the child-segment analogue of [`establish`] with no trigger. Persist/convey afterwards.
pub async fn establish_child(
    cc: &ClientConfig,
    child_coin: &mut Coin,
    sp_txid: &str,
    sp_vout: u32,
    sp_out_value: u64,
    owner_exit_address: &str,
    csv_e: u16,
    csv_d: u16,
    fee_rate: f64,
    network: &str,
) -> Result<ChildLadder> {
    let agg = child_coin
        .aggregated_address
        .clone()
        .ok_or_else(|| anyhow::anyhow!("child coin has no aggregated_address"))?;

    // Extension roots at the assigned SP output, under the child's aggregate.
    let x = mercurylib::tesr::build_extension_from(sp_txid, sp_vout, sp_out_value, &agg, network, csv_e, fee_rate)?;
    let x_signed = cosign_tier(cc, child_coin, x.tx_hex.clone(), sp_out_value, network).await?;

    // Owner state spends the extension's PAYLOAD output, pays the owner-exit key.
    let s = mercurylib::tesr::build_state_from(
        &x.txid, x.payload_vout, x.out_value, owner_exit_address, network, csv_d, fee_rate,
    )?;
    let s_signed = cosign_tier(cc, child_coin, s.tx_hex.clone(), x.out_value, network).await?;

    Ok(ChildLadder {
        extension: TesrTier { txid: x.txid, signed_tx: x_signed, out_value: x.out_value, csv: Some(csv_e), payload_vout: x.payload_vout },
        state: TesrTier { txid: s.txid, signed_tx: s_signed, out_value: s.out_value, csv: Some(csv_d), payload_vout: s.payload_vout },
    })
}

// =================================================================================================
// CTES-R — the COLOURED IN-LADDER SPLIT. The replacement for the legacy coloured-split lane.
//
// The legacy lane (`create_colored_split_tx`) spent the carrier's FUNDING outpoint `F` directly, so
// it was a RIVAL of the coloured trigger `T` over the same `F` carrying a rival RGB transition:
// whoever retained `T` could broadcast it, consume `F`, and void the split. That is why the two
// lanes were mutually exclusive per carrier, and why `colored_ladder` had to default OFF.
//
// The replacement carves the same value out of a DESCENDANT of `T` instead: an `SP` split-state tier
// over `X_m`'s payload output, carrying an RGB transition that assigns each child its share. `SP` is
// not a rival for `F` — it is a rival for the parent's own retained state `S_0`, one rung lower, and
// that is the ordinary CTES-R case the per-tier seal blinding already separates.
// =================================================================================================

/// The smallest `SP` output that can carry a COLOURED child ladder (extension + state, no trigger).
///
/// The coloured sibling of `mercurylib::tesr::min_child_value`: two coloured rungs plus a final
/// output that still clears dust. A coloured rung is dearer than a plain one by exactly
/// `P2TR_OUT_VBYTES * rate`, because the RGB `opret` is a real output the committed fee must cover.
pub fn colored_child_floor(fee_rate_sats_per_vb: f64, dust_limit: u64) -> u64 {
    2 * (crate::rgb::colored_committed_fee(1, fee_rate_sats_per_vb) + mercurylib::tesr::P2A_VALUE)
        + dust_limit
}

/// **[CATS change 2 / V5] The coloured sibling of `mercurylib::tesr::min_spine_tip_value`** — the
/// smallest `SP` output that can carry a COLOURED spine tip: ONE coloured rung plus a final output
/// that still clears dust. **906** sat at 2 sat/vB, against 1 482 for a coloured piece.
///
/// Same warning as the plain one: this is the CHANGE leg's floor and nothing else. A coloured PIECE
/// still builds two rungs and still needs [`colored_child_floor`].
pub fn colored_spine_tip_floor(fee_rate_sats_per_vb: f64, dust_limit: u64) -> u64 {
    crate::rgb::colored_committed_fee(1, fee_rate_sats_per_vb) + mercurylib::tesr::P2A_VALUE
        + dust_limit
}

/// What the caller asks for, per child of a coloured split. The child coin must ALREADY be
/// SE-registered — `SP` pays its aggregate, so the aggregate must exist before `SP` is built.
#[derive(Clone, Debug)]
pub struct ColoredSplitChildSpec {
    pub statechain_id: String,
    /// The child's aggregate address — this is what `SP.out[j]` pays.
    pub agg_address: String,
    /// Where the child's final state pays (Model A: the RECIPIENT's own exit key).
    pub owner_exit_address: String,
    /// The child's share of the sats.
    pub sats: u64,
    /// The child's share of the ALLOCATION. May be 0 for a sats-only child, but `Σ rgb_amount`
    /// across all children must equal the parent's whole allocation — no mint, no burn.
    pub rgb_amount: u64,
}

/// One child of a built-but-unsigned coloured split.
#[derive(Clone, Debug)]
pub struct ColoredSplitChildDraft {
    pub statechain_id: String,
    pub owner_exit_address: String,
    pub sp_vout: u32,
    pub sp_out_value: u64,
    pub rgb_amount: u64,
    pub csv_e: u16,
    pub csv_d: u16,
    pub extension: ColoredTierDraft,
    pub state: ColoredTierDraft,
}

/// A complete coloured in-ladder split, built and coloured, awaiting its `1 + 2N` SE co-signs.
#[derive(Clone, Debug)]
pub struct ColoredSplitDraft {
    pub parent_statechain_id: String,
    pub contract_id: String,
    pub network: String,
    pub fee_rate: f64,
    pub sp_csv: u16,
    pub sp_txid: String,
    pub sp_tx_hex: String,
    pub sp_consignment: String,
    /// `Σ child sats` — what `X_m`'s payload output affords across `N` payloads.
    pub sp_total: u64,
    /// `X_m`'s payload output, which `SP` spends (the co-signer's fail-closed recheck).
    pub parent_prev_txid: String,
    pub parent_prev_vout: u32,
    pub parent_prev_value: u64,
    pub children: Vec<ColoredSplitChildDraft>,
}

/// **Build a COLOURED in-ladder split.** Engine-only, synchronous, co-signs NOTHING — the
/// `!Sync`-resolver rule that governs every other coloured builder in this module.
///
/// Produces `1 + 2N` coloured tiers: the split state `SP` over `X_m`'s payload output assigning each
/// child its share, and per child a headless coloured ladder (`ext_child`, `state_child`) rooted at
/// `SP.out[j]` and paying the recipient's own exit key.
///
/// Three conservation laws are enforced BEFORE any tier is built, because every one of them is
/// unrecoverable once an SE co-sign has been spent:
///
/// 1. **Sats.** `Σ child sats == colored_tier_out_total(X_m.payload, N, rate)` — the coloured fee is
///    dearer than the plain one, so the plain `tier_out_total` would over-pay and the tier would be
///    rejected as a value-conservation failure at build time.
/// 2. **Allocation.** `Σ child rgb_amount == parent allocation`. An allocation must never be
///    destroyed; a split that burns part of one is refused here rather than discovered by a receiver
///    whose consignment assigns less than it was promised.
/// 3. **Child viability.** every child clears [`colored_child_floor`], so it can actually fund its
///    own two coloured rungs. A child below the floor would co-sign an `SP` that no child ladder can
///    hang off — the parent terminalized, the value stranded.
pub fn build_colored_in_ladder_split(
    rgb: &mercury_rgb::RgbWallet,
    bundle: &TesrBundle,
    children: &[ColoredSplitChildSpec],
) -> Result<ColoredSplitDraft> {
    use crate::rgb::{
        build_colored_tier, colored_tier_out_total, colored_tier_out_value, ColoredTierSpec,
        TierRole,
    };

    if !bundle.is_colored() {
        return Err(anyhow::anyhow!(
            "build_colored_in_ladder_split: this coin's ladder is PLAIN — use in_ladder_split"
        ));
    }
    // Reuses the coloured seal schedule's single-level invariant (and its reasoning).
    let _ = bundle.colored_tier_seals()?;
    let rgb_half = bundle.rgb.as_ref().expect("is_colored");
    let n = children.len();
    if n == 0 {
        return Err(anyhow::anyhow!("a coloured in-ladder split needs at least one child"));
    }
    let p = bundle.params;
    let s0_csv = bundle
        .current()
        .state
        .csv
        .ok_or_else(|| anyhow::anyhow!("current state has no CSV"))?;
    // [CATS] SP out-races `S_0` over `X_m`'s payload output at CSV 0 — see `SPINE_CSV`. This is also
    // what separates their SEALS (the CSV folds into `colored_tier_seal`), so the guard below is
    // doing two jobs at once: without a strict inequality the two tiers would share a blinding and
    // their `BundleId`s would collapse into an arbitrary hash lottery.
    //
    // This lane is the one the rung budget hurt most: a coloured carrier never renews, so
    // `s0_csv − δ` gave it exactly ONE partial payment in its whole life
    // (`docs/utexo/PARTIAL-PAYMENT-ECONOMICS.md` §1.3).
    if s0_csv <= SPINE_CSV {
        return Err(anyhow::anyhow!(
            "this carrier's live state has CSV {s0_csv}, which does not exceed the spine CSV \
             {SPINE_CSV} — SP could neither out-race it nor be sealed apart from it"
        ));
    }
    let sp_csv = SPINE_CSV;

    let x_m = bundle.current().extension.clone();
    let (parent_prev_value, parent_prev_spk) =
        tier_payload_prevout(&x_m, "coloured split parent")?;

    // ---- Conservation law 1: SATS. -------------------------------------------------------------
    let total = colored_tier_out_total(parent_prev_value, n, bundle.fee_rate).ok_or_else(|| {
        anyhow::anyhow!(
            "X_m's payload output ({parent_prev_value} sat) cannot carry a coloured {n}-child \
             split at {} sat/vB",
            bundle.fee_rate
        )
    })?;
    let sum: u64 = children.iter().map(|c| c.sats).sum();
    if sum != total {
        return Err(anyhow::anyhow!(
            "coloured split value conservation: children sum to {sum} sat but X_m's payload output \
             affords exactly {total} sat across {n} coloured payloads"
        ));
    }

    // ---- Conservation law 2: THE ALLOCATION. ---------------------------------------------------
    let rgb_sum: u64 = children.iter().map(|c| c.rgb_amount).sum();
    if rgb_sum != rgb_half.amount {
        return Err(anyhow::anyhow!(
            "coloured split ALLOCATION conservation: children sum to {rgb_sum} but the carrier holds \
             {} — refusing to mint or burn an allocation",
            rgb_half.amount
        ));
    }
    if rgb_sum == 0 {
        return Err(anyhow::anyhow!(
            "coloured split would assign nothing to any child — a coloured tier must assign a \
             non-zero amount"
        ));
    }

    // ---- Conservation law 3: CHILD VIABILITY. --------------------------------------------------
    let child_floor = colored_child_floor(bundle.fee_rate, COLORED_LADDER_DUST);
    for c in children {
        if c.sats < child_floor {
            return Err(anyhow::anyhow!(
                "coloured split child {} would hold {} sat but a COLOURED child ladder needs at \
                 least {child_floor} sat at {} sat/vB — refusing before any SE co-sign, because an \
                 SP whose output cannot fund its own ladder strands the value under a terminalized \
                 parent",
                c.statechain_id,
                c.sats,
                bundle.fee_rate
            ));
        }
    }

    // ---- SP: the split state over X_m's payload output. ----------------------------------------
    let sp_seal = colored_tier_seal(
        &bundle.statechain_id,
        TierRole::SplitState,
        0,
        bundle.m,
        Some(sp_csv),
    );
    let payloads: Vec<(String, u64, u64)> = children
        .iter()
        .map(|c| (c.agg_address.clone(), c.sats, c.rgb_amount))
        .collect();
    let sp = build_colored_tier(
        rgb,
        &ColoredTierSpec {
            contract_id: &rgb_half.contract_id,
            prev_txid: &x_m.txid,
            prev_vout: x_m.payload_vout,
            prev_value: parent_prev_value,
            prev_spk_hex: &parent_prev_spk,
            sequence: mercurylib::tesr::csv_blocks(sp_csv).0,
            payloads: &payloads,
            network: &bundle.network,
            fee_rate: bundle.fee_rate,
            nonce: Some(sp_seal.rung as u64),
        },
        &sp_seal,
    )?;
    if sp.payloads.len() != n {
        return Err(anyhow::anyhow!(
            "the coloured split state carries {} payload outputs, expected {n}",
            sp.payloads.len()
        ));
    }

    // ---- Per child: a headless COLOURED ladder off SP.out[j]. ----------------------------------
    let csv_e = p.ext_csv(0);
    let csv_d = p.state_csv(0);
    let mut child_drafts = Vec::with_capacity(n);
    for (j, c) in children.iter().enumerate() {
        // The child's vout is READ from the builder's returned payload list, never assumed: a
        // coloured tier carries the opret at index 0 and shifts every payload by one.
        let out = &sp.payloads[j];
        if out.value != c.sats {
            return Err(anyhow::anyhow!(
                "coloured split child {} was built at {} sat but asked for {}",
                c.statechain_id,
                out.value,
                c.sats
            ));
        }

        // Child extension: spends SP.out[j], stays under the CHILD's aggregate.
        let x_value = colored_tier_out_value(out.value, bundle.fee_rate).ok_or_else(|| {
            anyhow::anyhow!(
                "coloured split child {} ({} sat) cannot carry an extension",
                c.statechain_id,
                out.value
            )
        })?;
        let x_seal =
            colored_tier_seal(&c.statechain_id, TierRole::ChildExtension, 0, 0, Some(csv_e));
        let xc = build_colored_tier(
            rgb,
            &ColoredTierSpec {
                contract_id: &rgb_half.contract_id,
                prev_txid: &sp.txid,
                prev_vout: out.vout,
                prev_value: out.value,
                prev_spk_hex: &out.script_pubkey_hex,
                sequence: mercurylib::tesr::csv_blocks(csv_e).0,
                payloads: &[(c.agg_address.clone(), x_value, c.rgb_amount)],
                network: &bundle.network,
                fee_rate: bundle.fee_rate,
                nonce: Some(x_seal.rung as u64),
            },
            &x_seal,
        )?;

        // Child state: spends the child extension's payload output, pays the RECIPIENT.
        let s_value = colored_tier_out_value(xc.payloads[0].value, bundle.fee_rate)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "coloured split child {}'s extension cannot carry a state",
                    c.statechain_id
                )
            })?;
        let s_seal = colored_tier_seal(&c.statechain_id, TierRole::ChildState, 0, 0, Some(csv_d));
        let sc = build_colored_tier(
            rgb,
            &ColoredTierSpec {
                contract_id: &rgb_half.contract_id,
                prev_txid: &xc.txid,
                prev_vout: xc.payloads[0].vout,
                prev_value: xc.payloads[0].value,
                prev_spk_hex: &xc.payloads[0].script_pubkey_hex,
                sequence: mercurylib::tesr::csv_blocks(csv_d).0,
                payloads: &[(c.owner_exit_address.clone(), s_value, c.rgb_amount)],
                network: &bundle.network,
                fee_rate: bundle.fee_rate,
                nonce: Some(s_seal.rung as u64),
            },
            &s_seal,
        )?;

        let draft_of = |t: crate::rgb::ColoredTier| ColoredTierDraft {
            tx_hex: t.tx_hex,
            txid: t.txid,
            payload_vout: t.payloads[0].vout,
            payload_value: t.payloads[0].value,
            payload_spk_hex: t.payloads[0].script_pubkey_hex.clone(),
            consignment: t.consignment,
        };
        child_drafts.push(ColoredSplitChildDraft {
            statechain_id: c.statechain_id.clone(),
            owner_exit_address: c.owner_exit_address.clone(),
            sp_vout: out.vout,
            sp_out_value: out.value,
            rgb_amount: c.rgb_amount,
            csv_e,
            csv_d,
            extension: draft_of(xc),
            state: draft_of(sc),
        });
    }

    Ok(ColoredSplitDraft {
        parent_statechain_id: bundle.statechain_id.clone(),
        contract_id: rgb_half.contract_id.clone(),
        network: bundle.network.clone(),
        fee_rate: bundle.fee_rate,
        sp_csv,
        sp_txid: sp.txid,
        sp_tx_hex: sp.tx_hex,
        sp_consignment: sp.consignment,
        sp_total: total,
        parent_prev_txid: x_m.txid,
        parent_prev_vout: x_m.payload_vout,
        parent_prev_value: parent_prev_value,
        children: child_drafts,
    })
}

/// **Co-sign a [`ColoredSplitDraft`]** and return one [`ChildTesrBundle`] per child, ready to convey.
/// Network-only and async — the co-sign half of [`build_colored_in_ladder_split`].
///
/// `1 + 2N` `cosign_tier` round-trips: `SP` under the PARENT's aggregate, then each child's two
/// tiers under that CHILD's aggregate. Colouring adds no co-sign — one input, one sighash, one
/// `cosign_tier` — so the census arithmetic is byte for byte the plain in-ladder split's.
///
/// The parent is terminalized (budget 1, consumed by `SP`) BEFORE `SP` is co-signed, exactly as
/// [`in_ladder_split`] does, so no further parent state can be minted behind the children's backs.
///
/// Everything the draft asserts is RE-CHECKED against the live bundle here, because the draft is
/// built where the RGB engine lives and consumed where the network lives.
pub async fn cosign_colored_in_ladder_split(
    cc: &ClientConfig,
    wallet_name: &str,
    parent_coin: &mut Coin,
    bundle: &TesrBundle,
    draft: ColoredSplitDraft,
    child_coins: &mut [Coin],
) -> Result<Vec<ChildTesrBundle>> {
    let rgb_half = bundle
        .rgb
        .clone()
        .ok_or_else(|| anyhow::anyhow!("cosign_colored_in_ladder_split on a PLAIN ladder"))?;
    if draft.parent_statechain_id != bundle.statechain_id {
        return Err(anyhow::anyhow!(
            "coloured split draft is for {} but the bundle is {}",
            draft.parent_statechain_id,
            bundle.statechain_id
        ));
    }
    if draft.contract_id != rgb_half.contract_id {
        return Err(anyhow::anyhow!(
            "coloured split draft names contract {} but the ladder carries {}",
            draft.contract_id,
            rgb_half.contract_id
        ));
    }
    if draft.children.len() != child_coins.len() {
        return Err(anyhow::anyhow!(
            "coloured split draft has {} children but {} child coins were supplied",
            draft.children.len(),
            child_coins.len()
        ));
    }
    let x_m = bundle.current().extension.clone();
    if draft.parent_prev_txid != x_m.txid || draft.parent_prev_vout != x_m.payload_vout {
        return Err(anyhow::anyhow!(
            "coloured split draft spends {}:{} but the ladder's current extension output is {}:{}",
            draft.parent_prev_txid,
            draft.parent_prev_vout,
            x_m.txid,
            x_m.payload_vout
        ));
    }
    let s0_csv = bundle
        .current()
        .state
        .csv
        .ok_or_else(|| anyhow::anyhow!("current state has no CSV"))?;
    if draft.sp_csv >= s0_csv {
        return Err(anyhow::anyhow!(
            "coloured split state's CSV {} does not out-race the state it replaces ({s0_csv})",
            draft.sp_csv
        ));
    }
    // Re-check the ALLOCATION conservation law against the LIVE bundle, not the draft's own copy.
    let rgb_sum: u64 = draft.children.iter().map(|c| c.rgb_amount).sum();
    if rgb_sum != rgb_half.amount {
        return Err(anyhow::anyhow!(
            "coloured split draft assigns {rgb_sum} across its children but the carrier holds {} — \
             refusing to mint or burn an allocation",
            rgb_half.amount
        ));
    }

    let parent_sid = parent_coin
        .statechain_id
        .clone()
        .ok_or_else(|| anyhow::anyhow!("parent coin has no statechain_id"))?;
    if parent_sid != bundle.statechain_id {
        return Err(anyhow::anyhow!(
            "coloured split parent coin is {parent_sid} but the bundle is {}",
            bundle.statechain_id
        ));
    }
    // [D1] CENSUS. The child's receiver runs `verify_child_bundle` over the ancestor segment with
    // `flat_backups = <the parent's REAL flat backup count>`, which it cannot observe on its own —
    // it never owned the parent. So the chain is read here and CONVEYED in the bundle; the receiver
    // re-derives the count from it after validating every entry against the on-chain `F`
    // (`verify_conveyed_child`). See `ChildTesrBundle::parent_flat_backups` for why a conveyed count
    // is not an attacker-controlled term.
    //
    // This replaces the old `len() == PARENT_V2_BASELINE` refusal, which rejected any carrier this
    // wallet had RECEIVED rather than deposited — the constant is `1 + k` short after `k` whole-coin
    // hops, so a split of a received carrier minted a child no receiver could adopt (fail-closed,
    // but only after the parent was terminalized and the piece booked away).
    // Context added HERE, not inside `get_backup_txs`: that function's bare `sqlx::Error` is
    // load-bearing (`tokens::read_backup_rows` downcasts to `RowNotFound` to tell a genuine absence
    // from a failed read), so decorating it at the source breaks absence detection wallet-wide.
    // Decorating it at a call site that treats every failure alike is safe, and is the difference
    // between a diagnosable refusal and a naked "no rows returned by a query".
    let parent_backups =
        crate::sqlite_manager::get_backup_txs(&cc.pool, wallet_name, &parent_sid)
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "in-ladder split refused: the flat backup rows of parent {parent_sid} in \
                     wallet {wallet_name} could not be read ({e}) — the child's census cannot be \
                     balanced without them"
                )
            })?;
    if (parent_backups.len() as u32) < PARENT_V2_BASELINE {
        return Err(anyhow::anyhow!(
            "coloured in-ladder split refused: this carrier holds {} flat backup transaction(s), \
             fewer than the {PARENT_V2_BASELINE} every deposited coin carries — the local backup \
             record is incomplete and the child's census could not be balanced by any receiver",
            parent_backups.len(),
        ));
    }

    // Every child coin is matched to its draft child BEFORE anything irreversible happens. This used
    // to be checked inside the co-signing loop, i.e. after the parent had been terminalized: a
    // mismatched coin then refused a split that had already spent the parent's last budget slot.
    let child_sids = draft
        .children
        .iter()
        .zip(child_coins.iter())
        .map(|(cd, child_coin)| {
            let child_sid = child_coin
                .statechain_id
                .clone()
                .ok_or_else(|| anyhow::anyhow!("child coin has no statechain_id"))?;
            if child_sid != cd.statechain_id {
                return Err(anyhow::anyhow!(
                    "coloured split child coin is {child_sid} but the draft's child is {}",
                    cd.statechain_id
                ));
            }
            Ok(child_sid)
        })
        .collect::<Result<Vec<_>>>()?;

    // The parent segment's leaf consignment is SP's — `consignments` is indexed by `exit_tiers()`.
    // Computed before the terminalization for the same reason: it can fail, and a failure here after
    // the budget was spent would strand the carrier over a bookkeeping error.
    let mut parent_consignments = rgb_half.consignments.clone();
    let pc = parent_consignments.len();
    if pc == 0 {
        return Err(anyhow::anyhow!("coloured ladder carries no consignments"));
    }
    parent_consignments[pc - 1] = draft.sp_consignment.clone();

    // =============================================================================================
    // [B4] WRITE AHEAD, THEN TERMINALIZE — the coloured lane's half of [P0-3].
    //
    // THE DEFECT. This function did everything the plain `in_ladder_split` does — `set_spend_budget`
    // to terminalize the parent, then `1 + 2N` co-signatures the SE will never issue again — and
    // persisted NOTHING. A dropped response, a killed process or a full disk anywhere below returned
    // `Err` with the carrier permanently terminal server-side and not one byte of the material on
    // disk. For a COLOURED carrier that is worse than for a plain one: the tiers carry RGB
    // transitions built by an engine phase that has already been discarded by the time this runs, so
    // "sign it again" is not merely wasteful, it is impossible — the whole allocation's off-chain
    // life ends with the process. The plain lane's journal was written for exactly this failure and
    // this lane simply never got one.
    //
    // The record is the same `splitjrnl-<op_id>` shape, keyed with the same deterministic `op_id`
    // (`split_op_id`), so one recovery reader serves both lanes — plus the two coloured-only fields
    // (`PendingTier`, `SplitJournalChild::rgb`) without which a replay could not reproduce a tier it
    // cannot rebuild or hand back the allocation it carries.
    // =============================================================================================
    let mut journal = SplitJournalRecord {
        op_id: format!("in_ladder_split:{parent_sid}:{}", draft.sp_txid),
        lane: "colored_in_ladder_split".to_string(),
        stage: SplitStage::Planned,
        terminalized_statechain_id: parent_sid.clone(),
        parent: bundle.clone(),
        parent_statechain_id: parent_sid.clone(),
        ancestors: vec![],
        parent_flat_backups: parent_backups.clone(),
        children: draft
            .children
            .iter()
            .zip(child_sids.iter())
            .map(|(cd, sid)| SplitJournalChild {
                statechain_id: sid.clone(),
                owner_exit_address: cd.owner_exit_address.clone(),
                // The sats at `SP.out[j]` — what the child's extension spends.
                value: cd.sp_out_value,
                sp_vout: cd.sp_vout,
                extension: None,
                state: None,
                rgb: Some(ColoredChild {
                    contract_id: draft.contract_id.clone(),
                    amount: cd.rgb_amount,
                    consignments: vec![
                        cd.extension.consignment.clone(),
                        cd.state.consignment.clone(),
                    ],
                }),
                pending_extension: Some(PendingTier {
                    txid: cd.extension.txid.clone(),
                    tx_hex: cd.extension.tx_hex.clone(),
                    prev_value: cd.sp_out_value,
                    out_value: cd.extension.payload_value,
                    csv: cd.csv_e,
                    payload_vout: cd.extension.payload_vout,
                }),
                pending_state: Some(PendingTier {
                    txid: cd.state.txid.clone(),
                    tx_hex: cd.state.tx_hex.clone(),
                    prev_value: cd.extension.payload_value,
                    out_value: cd.state.payload_value,
                    csv: cd.csv_d,
                    payload_vout: cd.state.payload_vout,
                }),
                // The coloured lane carves N two-tier children and no spine tip.
                role: SplitLegRole::Piece,
            })
            .collect(),
        child_ext_csv: draft.children[0].csv_e,
        child_state_csv: draft.children[0].csv_d,
        fee_rate: draft.fee_rate,
        network: draft.network.clone(),
        sp_txid: draft.sp_txid.clone(),
    };
    journal_write(cc, wallet_name, &journal).await?;

    // Terminalize the parent — SP consumes the last budget slot.
    crate::lightning_latch::set_spend_budget(cc, wallet_name, &parent_sid, 1).await?;
    crash_point("after_colored_inladder_terminalize");
    let sp_signed = cosign_tier(
        cc,
        parent_coin,
        draft.sp_tx_hex.clone(),
        draft.parent_prev_value,
        &draft.network,
    )
    .await?;

    // The parent segment shared by every child bundle: SP is the current (terminal) state, and the
    // parent's own retained S_0 is disclosed as superseded — it rivals SP over X_m's payload output
    // and loses the maturity race by sitting one delta HIGHER.
    let mut parent_seg = bundle.clone();
    let last = parent_seg.levels.len() - 1;
    parent_seg.superseded_states.push(parent_seg.levels[last].state.clone());
    parent_seg.levels[last].state = TesrTier {
        txid: draft.sp_txid.clone(),
        signed_tx: sp_signed,
        out_value: draft.sp_total,
        csv: Some(draft.sp_csv),
        payload_vout: draft.children[0].sp_vout,
    };
    parent_seg.rgb = Some(ColoredLadder { consignments: parent_consignments, ..rgb_half.clone() });

    // The `SP` co-signature is the unregenerable one: record it before touching a child.
    journal.parent = parent_seg.clone();
    journal.stage = SplitStage::Signed;
    journal_write(cc, wallet_name, &journal).await?;
    crash_point("after_colored_inladder_sp_sign");

    let mut bundles = Vec::with_capacity(draft.children.len());
    for (j, (cd, child_coin)) in draft.children.iter().zip(child_coins.iter_mut()).enumerate() {
        // Per TIER, not per child, and for the same reason the plain lane journals per tier: a
        // re-run of a co-sign that already happened pushes the child's `num_sigs` past its census
        // (`baseline + 2 + superseded`) and no receiver can ever adopt it.
        let x_signed = cosign_tier(
            cc,
            child_coin,
            cd.extension.tx_hex.clone(),
            cd.sp_out_value,
            &draft.network,
        )
        .await?;
        let child_extension = TesrTier {
            txid: cd.extension.txid.clone(),
            signed_tx: x_signed,
            out_value: cd.extension.payload_value,
            csv: Some(cd.csv_e),
            payload_vout: cd.extension.payload_vout,
        };
        journal.children[j].extension = Some(child_extension.clone());
        journal_write(cc, wallet_name, &journal).await?;
        crash_point("after_colored_inladder_child_extension");

        let s_signed = cosign_tier(
            cc,
            child_coin,
            cd.state.tx_hex.clone(),
            cd.extension.payload_value,
            &draft.network,
        )
        .await?;
        let child_state = TesrTier {
            txid: cd.state.txid.clone(),
            signed_tx: s_signed,
            out_value: cd.state.payload_value,
            csv: Some(cd.csv_d),
            payload_vout: cd.state.payload_vout,
        };
        journal.children[j].state = Some(child_state.clone());
        journal_write(cc, wallet_name, &journal).await?;
        crash_point("after_colored_inladder_child_state");

        bundles.push(ChildTesrBundle {
            parent: parent_seg.clone(),
            parent_statechain_id: parent_sid.clone(),
            sp_vout: cd.sp_vout,
            child_statechain_id: child_sids[j].clone(),
            child_owner_exit_address: cd.owner_exit_address.clone(),
            child_extension,
            child_state,
            child_superseded_states: vec![],
            child_superseded_extensions: vec![],
            ancestors: vec![],
            rgb: Some(ColoredChild {
                contract_id: draft.contract_id.clone(),
                amount: cd.rgb_amount,
                consignments: vec![
                    cd.extension.consignment.clone(),
                    cd.state.consignment.clone(),
                ],
            }),
            parent_flat_backups: parent_backups.clone(),
        });
    }
    journal.stage = SplitStage::Established;
    journal_write(cc, wallet_name, &journal).await?;

    // [B4] CLOSED HERE, and deliberately — the one place this lane's journal differs from the plain
    // lane's. `in_ladder_split` leaves its record OPEN on return and its SDK caller commits it after
    // persisting and conveying, so the reader (`recover_in_ladder_splits`) can replay that window
    // too. This lane's caller is the coloured token path, which does not commit, and a record left
    // open after a SUCCESSFUL split is not a harmless loose end: the reader would replay a completed
    // operation — re-booking the carrier as withdrawn, re-booking the change, and reporting pieces
    // that were in fact conveyed as unconveyed, i.e. inviting a double send. A false replay is a
    // worse failure than the window it would cover, so the record is closed the moment the
    // unregenerable material is complete and handed back.
    //
    // What that leaves uncovered is the caller's own persist/convey step. The material is not lost
    // there — the record is still on disk and `journal_find(op_id)` (which reads EVERY stage, not
    // just open ones) rebuilds every bundle from it via `SplitJournalRecord::bundles`, with the
    // `op_id` derivable from any surviving bundle through `split_op_id`. It is recoverable by an
    // explicit call rather than automatically, which is a narrower and honest claim than the plain
    // lane's, and it is a strict improvement on nothing on disk at all.
    journal.stage = SplitStage::Committed;
    journal_write(cc, wallet_name, &journal).await?;
    Ok(bundles)
}

// =================================================================================================
// [P0-3] THE IN-LADDER SPLIT'S WRITE-AHEAD JOURNAL.
//
// THE DEFECT. `in_ladder_split` used to persist NOTHING. It terminalized the parent at the SE
// (`set_spend_budget(..., 1)`), obtained the `SP` co-signature, then co-signed two tiers per child in
// a loop — all in process memory — and the SDK persisted only on `Ok`. Any failure after the
// terminalization (a dropped SE response, a killed process, a full disk) returned `Err` with the
// parent PERMANENTLY terminal server-side and zero bundles on disk. Those signatures can never be
// regenerated: the parent's budget is spent, so the SE will co-sign nothing further over it, and the
// only surviving path for the whole coin is a unilateral exit of its own flat backup. The value is
// not stolen, but every off-chain property of the coin is destroyed by a transient error.
//
// THE FIX, and why it is this shape. Same pattern as the coloured lane's `structural_spend_journal`
// (`clients/libs/rust-sdk/src/tokens.rs`): a record written BEFORE the irreversible step and
// advanced through named stages as each piece of unregenerable material is produced, plus a
// `crash_point` fault injector so the durability claim can be proved by a real SIGABRT rather than
// argued. Two deliberate differences:
//
//   * the record lives in the wallet's existing raw-backup KV (the same durable store `persist` and
//     `persist_child` use, keyed `splitjrnl-<op_id>`) instead of a second table. This crate cannot
//     see the SDK's table, and adding a table would mean editing `sqlite_manager`; the KV gives the
//     identical durability (one sqlite transaction, `synchronous = FULL`) with no schema change.
//   * it journals PER TIER, not per operation. A child's tiers are co-signed one at a time under the
//     CHILD's key, and re-running a co-sign that already happened would push that child's `num_sigs`
//     past the census (`baseline + 2 + superseded`) and brick it. Recording each tier as it lands is
//     what makes replay exact instead of merely hopeful.
// =================================================================================================

/// Wallet-DB key prefix of an in-ladder split journal record.
const SPLIT_JOURNAL_PREFIX: &str = "splitjrnl-";

/// How far an in-ladder split got before it stopped.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitStage {
    /// Plan written, nothing irreversible done yet: the parent is NOT terminal, so the whole
    /// operation can simply be run again.
    Planned,
    /// The parent is terminal and `SP` is co-signed. From here the material is unregenerable and
    /// recovery must REPLAY, never restart.
    Signed,
    /// Every child's ladder is co-signed; the bundles are complete and ready to be persisted/conveyed.
    Established,
    /// The caller persisted and conveyed the bundles. Terminal stage — nothing left to recover.
    Committed,
    /// Terminal stage for the irreducible window: the SE consumed the budget but this process never
    /// recorded the co-signature, so `SP` can never be produced again. The coin's cooperative path is
    /// gone and only a unilateral exit of its own backup recovers the value. Recorded — never
    /// silently dropped — so the wallet can SAY so instead of looking idle.
    Stranded,
}

impl SplitStage {
    /// A stage that still needs the recovery reader's attention.
    pub fn is_open(self) -> bool {
        !matches!(self, SplitStage::Committed | SplitStage::Stranded)
    }
}

/// A tier that has been BUILT but not yet co-signed, journalled verbatim so a replay can produce
/// exactly it.
///
/// **[B4] Why the coloured lane needs this and the plain one does not.** A plain tier is a pure
/// function of `(prev outpoint, value, address, csv, fee rate)`, all of which the record already
/// holds, so `establish_child_journalled` simply rebuilds it. A COLOURED tier carries an RGB opret
/// committing to a state transition produced by the engine, and the engine is not available on the
/// recovery path (its resolver is `!Sync`, which is the entire reason colouring and co-signing are
/// separate phases). Rebuilding is therefore impossible and the unsigned transaction itself is what
/// must survive: a co-sign is a signature over THIS transaction, and re-deriving a different one
/// would spend a census slot on a tier no receiver can chain to.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PendingTier {
    pub txid: String,
    /// The UNSIGNED transaction, exactly as the builder produced it.
    pub tx_hex: String,
    /// Value of the outpoint this tier spends — the amount its taproot key-spend sighash commits to,
    /// so it must be journalled with the tier rather than re-derived on the recovery path.
    pub prev_value: u64,
    pub out_value: u64,
    pub csv: u16,
    pub payload_vout: u32,
}

/// **[CATS] Which LEG of a split a journalled child is.** The two legs have different SHAPES, and the
/// difference is not expressible any other way in this record.
///
/// **Why an explicit flag and not "`extension.is_none()` means spine".** `SplitJournalChild::extension`
/// ALREADY carries a meaning: `None` = *"this tier has not been co-signed yet"*. That is the entire
/// basis of [`resume_in_ladder_split`], which co-signs exactly the tiers that are still `None`.
/// Overloading it with *"this leg never has one"* makes the two indistinguishable **on the recovery
/// path only** — and the recovery path is where nobody is watching. A replay would read a spine tip's
/// absent extension as unfinished work and co-sign a PHANTOM extension over `SP.out[K]` at the
/// piece schedule's CSV 720, which out-races the sender's own cap `C_i` at 1440 over that very
/// outpoint: a self-inflicted rival for the sender's change, created by the crash-recovery machinery,
/// and only ever after a crash.
///
/// So the role is journalled, and [`resume_in_ladder_split`] checks it BIDIRECTIONALLY: a `Piece` is
/// complete only with both tiers, and a `SpineTip` that somehow holds an extension is a hard refusal
/// rather than a resume.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SplitLegRole {
    /// A payee's piece: extension (CSV `E0`) + state (CSV `D0`), hung off `SP.out[j]`.
    #[default]
    Piece,
    /// The sender's change: ONE cap tier directly over `SP.out[K]`, and no extension.
    SpineTip,
}

impl SplitLegRole {
    /// **The floor a leg of THIS role must clear**, plain lane. One number per shape, derived from
    /// the rungs the builder will actually construct for that shape — never one number for both legs.
    ///
    /// This is the [V5] hazard in one function. `min_spine_tip_value` is 490 sat below
    /// `min_child_value`; a piece admitted at the tip's floor cannot fund its second rung, and
    /// `establish_child` discovers that *after* `set_spend_budget` has terminalized the parent. The
    /// role therefore selects the floor, and the role is not a caller's choice — see
    /// [`change_leg_role`].
    pub fn min_value(self, fee_rate_sats_per_vb: f64, dust_limit: u64) -> u64 {
        match self {
            SplitLegRole::Piece => mercurylib::tesr::min_child_value(fee_rate_sats_per_vb, dust_limit),
            SplitLegRole::SpineTip => {
                mercurylib::tesr::min_spine_tip_value(fee_rate_sats_per_vb, dust_limit)
            }
        }
    }

    /// The COLOURED sibling of [`Self::min_value`].
    pub fn colored_min_value(self, fee_rate_sats_per_vb: f64, dust_limit: u64) -> u64 {
        match self {
            SplitLegRole::Piece => colored_child_floor(fee_rate_sats_per_vb, dust_limit),
            SplitLegRole::SpineTip => colored_spine_tip_floor(fee_rate_sats_per_vb, dust_limit),
        }
    }
}

/// **[CATS change 2] THE SHAPE THE SPLIT BUILDERS ACTUALLY GIVE THE SENDER'S CHANGE LEG.**
///
/// One function, one answer, consulted by every admission guard that needs to price the change leg —
/// so the floor a payment is admitted at and the ladder the builder then constructs can never be two
/// different shapes.
///
/// It reports [`SplitLegRole::Piece`] because that is what the three split builders
/// ([`in_ladder_split`], [`child_in_ladder_split`], [`cosign_colored_in_ladder_split`]) still emit:
/// every leg goes through `establish_child`, which hangs an extension *and* a state off `SP.out[j]`.
/// Change 2 — the change leg becoming a one-cap spine tip — is the producer half, and it is not
/// landed.
///
/// **Flipping this to `SpineTip` is part of change 2 and must land in the same commit as the builder
/// change, never before it.** The direction of the error is the whole reason this is a function and
/// not a comment:
///
/// * flipped EARLY (floor 820, builder still two-tier) the payment is ADMITTED, the parent is
///   terminalized, and `establish_child` then fails to fund the change child's second rung — the
///   coin is stranded to unilateral-exit-only. Fails **open**;
/// * flipped LATE (floor 1 310, builder one-tier) the wallet merely refuses some payments the chain
///   would carry. Fails **closed**, and visibly.
///
/// The verifier half (V1/V2 + the prevout derivation) already admits both shapes, so nothing is
/// blocked by this staying `Piece` — the only cost is the 490 sat of change headroom CATS buys back.
pub fn change_leg_role() -> SplitLegRole {
    SplitLegRole::Piece
}

/// One child of a journalled split. `extension`/`state` are filled in as each tier is co-signed, so a
/// crash between the two is resumed by co-signing only the missing one.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SplitJournalChild {
    pub statechain_id: String,
    /// Model-A payee of this child's final state (the recipient's exit key, or ours for the change).
    pub owner_exit_address: String,
    pub value: u64,
    /// Which `SP`/`CSP` output funds this child.
    pub sp_vout: u32,
    pub extension: Option<TesrTier>,
    pub state: Option<TesrTier>,
    /// **[B4/CTES-R]** This child's coloured half — contract, its share of the allocation, and the
    /// consignments for its own two tiers. `None` on the plain lanes. Known at PLAN time (the draft
    /// is built and validated before anything is co-signed), so a replay reconstructs a coloured
    /// child bundle complete with its proofs rather than a sats-only stub that would be conveyed with
    /// the allocation silently unaccounted for.
    #[serde(default)]
    pub rgb: Option<ColoredChild>,
    /// **[B4]** The child's pre-built, un-co-signed tiers (coloured lane only — see [`PendingTier`]).
    #[serde(default)]
    pub pending_extension: Option<PendingTier>,
    #[serde(default)]
    pub pending_state: Option<PendingTier>,
    /// **[CATS]** Which leg this is — see [`SplitLegRole`].
    ///
    /// A `#[serde(default)]` IS correct here, unlike on `ChildSegment::extension`: this record is
    /// LOCAL (the wallet's own write-ahead journal, never conveyed), every row written before this
    /// field existed is a two-tier piece, and `Piece` is exactly what those rows mean. The conveyed
    /// struct has no such guarantee, which is why it carries no default.
    #[serde(default)]
    pub role: SplitLegRole,
}

/// The durable plan + co-signed material of ONE in-ladder split.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SplitJournalRecord {
    pub op_id: String,
    /// `"in_ladder_split"` (root parent) or `"child_in_ladder_split"` (a received child).
    pub lane: String,
    pub stage: SplitStage,
    /// The coin this split terminalizes — the root parent, or the child being re-split.
    pub terminalized_statechain_id: String,
    /// The ROOT parent segment as the children will see it (`SP` installed as the current state once
    /// the stage is `Signed`).
    pub parent: TesrBundle,
    pub parent_statechain_id: String,
    /// Intermediate segments for the children (empty on the root lane; the just-split child's own
    /// segment on the child lane).
    pub ancestors: Vec<ChildSegment>,
    pub parent_flat_backups: Vec<mercurylib::wallet::BackupTx>,
    pub children: Vec<SplitJournalChild>,
    /// Tier CSVs the children's ladders are built with (so a replay reproduces them exactly).
    pub child_ext_csv: u16,
    pub child_state_csv: u16,
    pub fee_rate: f64,
    pub network: String,
    /// txid of the split state (`SP`/`CSP`) the children are funded by.
    pub sp_txid: String,
}

impl SplitJournalRecord {
    /// Rebuild the split's conveyable bundles. `Err` if any child is still incomplete — a caller must
    /// finish it (see [`resume_in_ladder_split`]) rather than convey a half-built ladder.
    pub fn bundles(&self) -> Result<Vec<ChildTesrBundle>> {
        self.children
            .iter()
            .map(|c| {
                // [CATS] A spine tip is not a conveyable child bundle — it is the sender's own change
                // and gets its own persisted record. Refuse rather than fabricate a `ChildTesrBundle`
                // for it (a `ctesr-` row routes to leaf handling, which is exactly the mis-classing
                // the tip's separate record exists to prevent).
                if c.role != SplitLegRole::Piece {
                    return Err(anyhow::anyhow!(
                        "journalled leg {} is the spine tip, not a conveyable child — it must be \
                         rebuilt as the sender's own tip record, never as a `ctesr-` child bundle",
                        c.statechain_id
                    ));
                }
                let (ext, st) = match (&c.extension, &c.state) {
                    (Some(e), Some(s)) => (e.clone(), s.clone()),
                    _ => {
                        return Err(anyhow::anyhow!(
                            "journalled child {} has no complete ladder yet",
                            c.statechain_id
                        ))
                    }
                };
                Ok(ChildTesrBundle {
                    parent: self.parent.clone(),
                    parent_statechain_id: self.parent_statechain_id.clone(),
                    sp_vout: c.sp_vout,
                    child_statechain_id: c.statechain_id.clone(),
                    child_owner_exit_address: c.owner_exit_address.clone(),
                    child_extension: ext,
                    child_state: st,
                    child_superseded_states: vec![],
                    child_superseded_extensions: vec![],
                    ancestors: self.ancestors.clone(),
                    // [B4] The coloured lane journals its children's allocations and proofs, so a
                    // record replayed from disk rebuilds a COLOURED child when that is what was
                    // carved. `None` on the two plain lanes, where it always was.
                    rgb: c.rgb.clone(),
                    parent_flat_backups: self.parent_flat_backups.clone(),
                })
            })
            .collect()
    }
}

/// FAULT INJECTION for the crash-recovery test, debug builds ONLY.
///
/// A durability claim can only be proved by really killing the process at the instant in question —
/// an early `return` proves nothing about what reached the disk. Fires only when the operator sets
/// `UTEXO_CRASH_POINT` to this exact point name, and is compiled out of release builds entirely.
/// (Deliberately identical to the coloured lane's injector in `tokens.rs` — same contract, same
/// SIGABRT: no unwinding, no `Drop`, no flush.)
#[cfg(debug_assertions)]
fn crash_point(name: &str) {
    if std::env::var("UTEXO_CRASH_POINT").as_deref() == std::result::Result::Ok(name) {
        eprintln!("UTEXO_CRASH_POINT={name}: aborting to exercise in-ladder split recovery");
        std::process::abort();
    }
}
#[cfg(not(debug_assertions))]
fn crash_point(_name: &str) {}

/// Commit a journal record at its current stage. **Durable on return** — that is the entire point of
/// the write-ahead ordering, so every caller must `?` it and none may treat a write failure as a
/// warning.
pub async fn journal_write(
    cc: &ClientConfig,
    wallet_name: &str,
    rec: &SplitJournalRecord,
) -> Result<()> {
    let json = serde_json::to_string(rec)?;
    crate::sqlite_manager::insert_raw_backup_txs(
        &cc.pool,
        wallet_name,
        &format!("{SPLIT_JOURNAL_PREFIX}{}", rec.op_id),
        &json,
    )
    .await
    .map_err(|e| {
        anyhow::anyhow!(
            "in-ladder split journal write failed at stage {:?} ({e}) — refusing to continue, \
             because the next step produces signatures that could never be regenerated",
            rec.stage
        )
    })
}

/// Every in-ladder split of this wallet that stopped before it was committed, i.e. THE recovery
/// reader's input. A row that cannot be decoded is an ERROR, never a silently skipped entry — an
/// unreadable journal is exactly the "failure looks like idle" shape this journal exists to kill.
pub async fn journal_open_splits(
    cc: &ClientConfig,
    wallet_name: &str,
) -> Result<Vec<SplitJournalRecord>> {
    let mut out = Vec::new();
    for (k, json) in crate::sqlite_manager::get_all_backup_txs(&cc.pool, wallet_name).await? {
        if !k.starts_with(SPLIT_JOURNAL_PREFIX) {
            continue;
        }
        let rec: SplitJournalRecord = serde_json::from_str(&json).map_err(|e| {
            anyhow::anyhow!("unreadable in-ladder split journal row {k}: {e}")
        })?;
        if rec.stage.is_open() {
            out.push(rec);
        }
    }
    Ok(out)
}

/// One journal record by `op_id`, at ANY stage (including the terminal ones). Used by a caller that
/// finished recovery and still needs the rebuilt material — e.g. to convey a piece whose payment the
/// crash interrupted.
pub async fn journal_find(
    cc: &ClientConfig,
    wallet_name: &str,
    op_id: &str,
) -> Result<Option<SplitJournalRecord>> {
    let key = format!("{SPLIT_JOURNAL_PREFIX}{op_id}");
    for (k, json) in crate::sqlite_manager::get_all_backup_txs(&cc.pool, wallet_name).await? {
        if k == key {
            return Ok(Some(serde_json::from_str(&json).map_err(|e| {
                anyhow::anyhow!("unreadable in-ladder split journal row {k}: {e}")
            })?));
        }
    }
    Ok(None)
}

/// Every journal record of the splits that consumed `terminalized_statechain_id`, at ANY stage.
/// The audit view: what this wallet did to that coin, and what it produced.
pub async fn journal_records_for(
    cc: &ClientConfig,
    wallet_name: &str,
    terminalized_statechain_id: &str,
) -> Result<Vec<SplitJournalRecord>> {
    let mut out = Vec::new();
    for (k, json) in crate::sqlite_manager::get_all_backup_txs(&cc.pool, wallet_name).await? {
        if !k.starts_with(SPLIT_JOURNAL_PREFIX) {
            continue;
        }
        let rec: SplitJournalRecord = serde_json::from_str(&json)
            .map_err(|e| anyhow::anyhow!("unreadable in-ladder split journal row {k}: {e}"))?;
        if rec.terminalized_statechain_id == terminalized_statechain_id {
            out.push(rec);
        }
    }
    Ok(out)
}

/// Move a journalled split to a terminal stage. `journal_commit` is called by the caller that has
/// PERSISTED and CONVEYED the bundles — never by the split itself: until they are on disk the
/// operation is still recoverable.
pub async fn journal_close(
    cc: &ClientConfig,
    wallet_name: &str,
    op_id: &str,
    stage: SplitStage,
) -> Result<()> {
    let mut open = journal_open_splits(cc, wallet_name).await?;
    if let Some(rec) = open.iter_mut().find(|r| r.op_id == op_id) {
        rec.stage = stage;
        journal_write(cc, wallet_name, rec).await?;
    }
    Ok(())
}

/// [`journal_close`] at [`SplitStage::Committed`].
pub async fn journal_commit(cc: &ClientConfig, wallet_name: &str, op_id: &str) -> Result<()> {
    journal_close(cc, wallet_name, op_id, SplitStage::Committed).await
}

/// Co-sign the one child ladder of a journalled split, RESUMING from whatever already landed.
///
/// The difference from [`establish_child`] is the journal: each tier is written the instant it comes
/// back from the SE, and a tier already recorded is reused rather than re-signed. Re-signing would be
/// silently fatal — the child's `num_sigs` would exceed `CHILD_V2_BASELINE + tiers + superseded` and
/// no receiver could ever adopt it.
async fn establish_child_journalled(
    cc: &ClientConfig,
    wallet_name: &str,
    child_coin: &mut Coin,
    rec: &mut SplitJournalRecord,
    j: usize,
) -> Result<ChildLadder> {
    // [CATS] This function builds a PIECE ladder — extension at `csv_e`, then state at `csv_d`. A
    // spine tip has neither of those; it has ONE cap over its funding outpoint. Rather than let a
    // future caller reach the extension builder with a tip and quietly mint a rival for the sender's
    // own change, refuse by name. Unreachable today (nothing writes `SpineTip`), and it stays
    // fail-closed if the producer half lands without wiring its own path.
    if rec.children[j].role != SplitLegRole::Piece {
        return Err(anyhow::anyhow!(
            "in-ladder split {}: leg {j} ({}) is the spine tip, which has one cap and no extension — \
             this builder only produces a piece's two-tier ladder",
            rec.op_id,
            rec.children[j].statechain_id
        ));
    }
    let (sp_txid, csv_e, csv_d, fee_rate, network) = (
        rec.sp_txid.clone(),
        rec.child_ext_csv,
        rec.child_state_csv,
        rec.fee_rate,
        rec.network.clone(),
    );
    let (sp_vout, value, owner_exit_address) = {
        let c = &rec.children[j];
        (c.sp_vout, c.value, c.owner_exit_address.clone())
    };
    let agg = child_coin
        .aggregated_address
        .clone()
        .ok_or_else(|| anyhow::anyhow!("child coin has no aggregated_address"))?;

    // Extension over SP.out[j], under the child's aggregate.
    //
    // [B4] A journalled PENDING tier wins over rebuilding: on the coloured lane the transaction
    // carries an RGB opret this path cannot reproduce (no engine here), so the record holds the
    // unsigned tx and the co-sign is taken over exactly it. On the plain lanes there is no pending
    // tier and the tier is rebuilt as before — deterministically, from the same inputs.
    let extension = match rec.children[j].extension.clone() {
        Some(t) => t,
        None => {
            let (tx_hex, txid, out_value, csv, payload_vout, prev_value) =
                match rec.children[j].pending_extension.clone() {
                    Some(p) => (p.tx_hex, p.txid, p.out_value, p.csv, p.payload_vout, p.prev_value),
                    None => {
                        let x = mercurylib::tesr::build_extension_from(
                            &sp_txid, sp_vout, value, &agg, &network, csv_e, fee_rate,
                        )?;
                        (x.tx_hex, x.txid, x.out_value, csv_e, x.payload_vout, value)
                    }
                };
            let x_signed = cosign_tier(cc, child_coin, tx_hex, prev_value, &network).await?;
            let tier = TesrTier {
                txid,
                signed_tx: x_signed,
                out_value,
                csv: Some(csv),
                payload_vout,
            };
            rec.children[j].extension = Some(tier.clone());
            journal_write(cc, wallet_name, rec).await?;
            crash_point("after_inladder_child_extension");
            tier
        }
    };

    // Owner state over the extension's payload output, paying the Model-A payee.
    let state = match rec.children[j].state.clone() {
        Some(t) => t,
        None => {
            let (tx_hex, txid, out_value, csv, payload_vout, prev_value) =
                match rec.children[j].pending_state.clone() {
                    Some(p) => (p.tx_hex, p.txid, p.out_value, p.csv, p.payload_vout, p.prev_value),
                    None => {
                        let s = mercurylib::tesr::build_state_from(
                            &extension.txid,
                            extension.payload_vout,
                            extension.out_value,
                            &owner_exit_address,
                            &network,
                            csv_d,
                            fee_rate,
                        )?;
                        (s.tx_hex, s.txid, s.out_value, csv_d, s.payload_vout, extension.out_value)
                    }
                };
            let s_signed = cosign_tier(cc, child_coin, tx_hex, prev_value, &network).await?;
            let tier = TesrTier {
                txid,
                signed_tx: s_signed,
                out_value,
                csv: Some(csv),
                payload_vout,
            };
            rec.children[j].state = Some(tier.clone());
            journal_write(cc, wallet_name, rec).await?;
            crash_point("after_inladder_child_state");
            tier
        }
    };

    Ok(ChildLadder { extension, state })
}

/// **THE RECOVERY READER.** Finish an in-ladder split that a crash interrupted, and return its
/// conveyable bundles.
///
/// `child_coins` supplies the (SE-registered, wallet-owned) coin for every journalled child that
/// still needs a tier; a child whose ladder is already complete needs none. Fails CLOSED: a child
/// that is unfinished and whose coin was not supplied is an error, never a bundle quietly omitted.
///
/// A `Planned` record is NOT replayed here — nothing irreversible had happened when it was written,
/// so the caller re-runs the whole split instead (see [`split_is_retryable`]).
pub async fn resume_in_ladder_split(
    cc: &ClientConfig,
    wallet_name: &str,
    rec: &mut SplitJournalRecord,
    child_coins: &mut [(String, Coin)],
) -> Result<Vec<ChildTesrBundle>> {
    if rec.stage == SplitStage::Planned {
        return Err(anyhow::anyhow!(
            "in-ladder split {} stopped before the parent was terminalized — it must be RE-RUN, not \
             replayed (replaying would produce a second SP)",
            rec.op_id
        ));
    }
    for j in 0..rec.children.len() {
        // [CATS] COMPLETENESS IS ROLE-DEPENDENT, AND THE CHECK RUNS BOTH WAYS.
        //
        // `extension: None` means "not co-signed yet" for a Piece and "never has one" for a SpineTip.
        // Reading a tip as unfinished would make this loop co-sign a phantom extension over
        // `SP.out[K]` at the piece CSV — a tier that out-races the sender's OWN cap over that
        // outpoint. Reading the flag one way only is not enough either: a tip that somehow carries an
        // extension is corruption (or a record written by a build that predates the role), and
        // continuing would convey a leg whose shape contradicts its own journal. Refuse, loudly.
        match rec.children[j].role {
            SplitLegRole::Piece => {
                if rec.children[j].extension.is_some() && rec.children[j].state.is_some() {
                    continue;
                }
            }
            SplitLegRole::SpineTip => {
                if rec.children[j].extension.is_some() {
                    return Err(anyhow::anyhow!(
                        "cannot resume in-ladder split {}: leg {j} ({}) is journalled as the SPINE TIP \
                         but carries an extension tier. A spine tip has exactly one cap over its \
                         funding outpoint; an extension there would be a rival for that outpoint at \
                         the piece schedule's CSV, out-racing the tip's own cap. Refusing to replay a \
                         record whose shape contradicts itself.",
                        rec.op_id,
                        rec.children[j].statechain_id
                    ));
                }
                if rec.children[j].state.is_some() {
                    continue;
                }
            }
        }
        let sid = rec.children[j].statechain_id.clone();
        let coin = child_coins
            .iter_mut()
            .find(|(id, _)| *id == sid)
            .map(|(_, c)| c)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "cannot resume in-ladder split {}: child {sid} has an incomplete ladder and its \
                     coin was not supplied — refusing to return a bundle set that silently omits it",
                    rec.op_id
                )
            })?;
        establish_child_journalled(cc, wallet_name, coin, rec, j).await?;
    }
    rec.stage = SplitStage::Established;
    journal_write(cc, wallet_name, rec).await?;
    rec.bundles()
}

/// The journal `op_id` of the split that produced this bundle — deterministic, so a caller that
/// holds only the returned bundles can commit (or look up) the journal record without the split
/// having to hand back an extra value.
pub fn split_op_id(cb: &ChildTesrBundle) -> String {
    match cb.ancestors.last() {
        None => format!(
            "in_ladder_split:{}:{}",
            cb.parent_statechain_id,
            cb.parent.current().state.txid
        ),
        Some(seg) => format!(
            "child_in_ladder_split:{}:{}",
            seg.statechain_id, seg.state.txid
        ),
    }
}

/// Is a stopped split safe to simply run again? True only for a `Planned` record whose terminalized
/// coin is confirmed NOT terminal at the SE — i.e. the co-signature provably never happened.
/// Mirrors the coloured lane's `classify_prepared`, and fails CLOSED: an SE that cannot be read
/// yields `Err`, never a hopeful `true`.
pub async fn split_is_retryable(cc: &ClientConfig, rec: &SplitJournalRecord) -> Result<bool> {
    if rec.stage != SplitStage::Planned {
        return Ok(false);
    }
    let (_, _, terminal) =
        crate::lightning_latch::get_spend_budget(cc, &rec.terminalized_statechain_id).await?;
    Ok(!terminal)
}

/// [in-ladder split] The production sender for an in-ladder split (B1 fix, PROTOCOL.md §5.4). Builds
/// `SP` — a STATE tier spending `X_m.out[0]` (a DESCENDANT of the trigger, NOT a rival for `F`), paying
/// each child statechain coin's aggregate — terminalizes the parent (budget 1, consumed by `SP`),
/// co-signs `SP` under `A_parent`, discloses the old owner state `S_0` as superseded (out-raced by `SP`
/// one rung lower), and establishes each child's headless ladder off `SP.out[j]` paying its recipient.
/// Returns one [`ChildTesrBundle`] per child for conveyance; the receiver checks each with
/// [`verify_child_bundle`]. The child coins must already be SE-registered (their aggregate is what `SP`
/// pays); `children` is `(child_coin, recipient_owner_exit_address, value_sats)` and is mutated as each
/// child ladder is co-signed. Value is conserved: `Σ value == tier_out_total(X_m.out[0], N)`.
pub async fn in_ladder_split(
    cc: &ClientConfig,
    wallet_name: &str,
    parent_coin: &mut Coin,
    bundle: &TesrBundle,
    children: &mut [(Coin, String, u64)],
) -> Result<Vec<ChildTesrBundle>> {
    refuse_uncolored_over_colored(bundle, "in_ladder_split")?;
    let p = bundle.params;
    let x_m = bundle.current().extension.clone();
    // [CATS] SP must OUT-RACE `S_0` over `X_m.out[0]`, and it does so at CSV 0 — see `SPINE_CSV`.
    // This used to be `s0_csv − δ`, floored, which consumed a state rung per split and refused the
    // split outright once the rung budget ran out ("state CSV at the floor — renew/rollover before
    // splitting"). That refusal is gone: a split no longer spends anything the coin cannot replace.
    // `S_0`'s own CSV is still read, because the superseded battery needs it to be strictly greater
    // than the live one and a state without a CSV is a malformed bundle either way.
    let s0_csv = bundle
        .current()
        .state
        .csv
        .ok_or_else(|| anyhow::anyhow!("current state has no CSV"))?;
    if s0_csv <= SPINE_CSV {
        return Err(anyhow::anyhow!(
            "the state SP would supersede already has CSV {s0_csv}, which does not exceed the spine \
             CSV {SPINE_CSV} — replace-by-lower-timelock cannot be satisfied, refusing to split"
        ));
    }
    let sp_csv = SPINE_CSV;

    let n = children.len();
    if n == 0 {
        return Err(anyhow::anyhow!("in-ladder split needs at least one child"));
    }
    // [P0-2] A root split mints depth-1 children. Normally admissible, but not if the deployed epoch
    // is too short for even one level — check rather than assume.
    enforce_split_depth_cap(cc, p, 1).await?;
    // Value conservation — no mint, no burn (build_split_state re-checks, but fail early with context).
    let total = mercurylib::tesr::tier_out_total(x_m.out_value, n, bundle.fee_rate)
        .ok_or_else(|| anyhow::anyhow!("committed fee too high for {n} children"))?;
    let sum: u64 = children.iter().map(|(_, _, v)| *v).sum();
    if sum != total {
        return Err(anyhow::anyhow!(
            "child values sum to {sum} but must equal {total} (= X_m.out[0] − committed fee)"
        ));
    }

    // SP pays each child's aggregate address, in order (SP.out[j] == children[j]).
    let payees: Vec<(String, u64)> = children
        .iter()
        .map(|(c, _, v)| {
            c.aggregated_address
                .clone()
                .ok_or_else(|| anyhow::anyhow!("child coin has no aggregated_address"))
                .map(|a| (a, *v))
        })
        .collect::<Result<_>>()?;
    let sp = mercurylib::tesr::build_split_state(
        &x_m.txid, x_m.out_value, &payees, &bundle.network, sp_csv, bundle.fee_rate,
    )?;

    // Terminalize the parent (SP consumes the last budget slot) and co-sign SP under A_parent.
    let parent_sid = parent_coin
        .statechain_id
        .clone()
        .ok_or_else(|| anyhow::anyhow!("parent coin has no statechain_id"))?;

    // [D1] CENSUS — see the identical read in `cosign_colored_in_ladder_split`.
    //
    // The parent's flat backup chain is a fact about its history the child's receiver cannot observe,
    // so it is CONVEYED (`ChildTesrBundle::parent_flat_backups`) and the receiver counts it after
    // structurally validating it against the on-chain `F`. This used to be a hard refusal of any
    // coin `len() != PARENT_V2_BASELINE`, i.e. of every RECEIVED coin: the constant is `k` short
    // after `k` whole-coin hops, and splitting anyway minted a child no receiver could adopt — after
    // the parent had been terminalized and the piece booked away.
    //
    // Read BEFORE `set_spend_budget`, which is the point of no return.
    // Context added HERE, not inside `get_backup_txs`: that function's bare `sqlx::Error` is
    // load-bearing (`tokens::read_backup_rows` downcasts to `RowNotFound` to tell a genuine absence
    // from a failed read), so decorating it at the source breaks absence detection wallet-wide.
    // Decorating it at a call site that treats every failure alike is safe, and is the difference
    // between a diagnosable refusal and a naked "no rows returned by a query".
    let parent_backups =
        crate::sqlite_manager::get_backup_txs(&cc.pool, wallet_name, &parent_sid)
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "in-ladder split refused: the flat backup rows of parent {parent_sid} in \
                     wallet {wallet_name} could not be read ({e}) — the child's census cannot be \
                     balanced without them"
                )
            })?;
    if (parent_backups.len() as u32) < PARENT_V2_BASELINE {
        return Err(anyhow::anyhow!(
            "in-ladder split refused: this coin holds {} flat backup transaction(s), fewer than the \
             {PARENT_V2_BASELINE} every deposited coin carries — the local backup record is \
             incomplete and the child's census could not be balanced by any receiver",
            parent_backups.len(),
        ));
    }

    // [P0-3] WRITE AHEAD, THEN TERMINALIZE. Everything below `set_spend_budget` produces material the
    // SE will never re-issue; if this record is not on disk first, a crash anywhere in the rest of
    // this function leaves the parent terminal with nothing to show for it and the coin exit-only
    // forever. The plan is complete here: `SP` is fully determined (its txid is fixed before the
    // signature), and each child's tiers are appended as they land.
    let mut journal = SplitJournalRecord {
        op_id: format!("in_ladder_split:{parent_sid}:{}", sp.txid),
        lane: "in_ladder_split".to_string(),
        stage: SplitStage::Planned,
        terminalized_statechain_id: parent_sid.clone(),
        parent: bundle.clone(),
        parent_statechain_id: parent_sid.clone(),
        ancestors: vec![],
        parent_flat_backups: parent_backups.clone(),
        children: children
            .iter()
            .enumerate()
            .map(|(j, (c, recipient, value))| {
                Ok(SplitJournalChild {
                    statechain_id: c
                        .statechain_id
                        .clone()
                        .ok_or_else(|| anyhow::anyhow!("child coin has no statechain_id"))?,
                    owner_exit_address: recipient.clone(),
                    value: *value,
                    // Child `j` lives at SP's j-th PAYLOAD output, not positional `j` (a coloured SP
                    // carries the opret at index 0 and shifts every child by one).
                    sp_vout: sp.payload_vout + j as u32,
                    extension: None,
                    state: None,
                    // The PLAIN lane: no allocation, and every tier is rebuildable from this record
                    // without an engine, so nothing pre-built needs to be carried.
                    rgb: None,
                    pending_extension: None,
                    pending_state: None,
                    // [CATS] Every leg is still a two-tier PIECE on this lane. Change 2 — the change
                    // leg becoming a one-cap spine tip — is the producer half and lands with the
                    // tip's own persisted record; this flag exists now so that when it does, the
                    // recovery reader can already tell the two legs apart.
                    role: SplitLegRole::Piece,
                })
            })
            .collect::<Result<Vec<_>>>()?,
        child_ext_csv: p.ext_csv(0),
        child_state_csv: p.state_csv(0),
        fee_rate: bundle.fee_rate,
        network: bundle.network.clone(),
        sp_txid: sp.txid.clone(),
    };
    journal_write(cc, wallet_name, &journal).await?;

    crate::lightning_latch::set_spend_budget(cc, wallet_name, &parent_sid, 1).await?;
    crash_point("after_inladder_terminalize");
    let sp_signed = cosign_tier(cc, parent_coin, sp.tx_hex.clone(), x_m.out_value, &bundle.network).await?;

    // Parent segment shared by every child bundle: SP is the current (terminal) state; S_0 superseded.
    let mut parent_seg = bundle.clone();
    let last = parent_seg.levels.len() - 1;
    parent_seg.superseded_states.push(parent_seg.levels[last].state.clone());
    parent_seg.levels[last].state = TesrTier {
        txid: sp.txid.clone(),
        signed_tx: sp_signed,
        out_value: total,
        csv: Some(sp_csv),
        payload_vout: sp.payload_vout,
    };
    // The `SP` co-signature is the unregenerable one: record it before touching a child.
    journal.parent = parent_seg.clone();
    journal.stage = SplitStage::Signed;
    journal_write(cc, wallet_name, &journal).await?;
    crash_point("after_inladder_sp_sign");

    // Each child: headless ladder off SP.out[j], paying its recipient (Model A).
    let mut bundles = Vec::with_capacity(n);
    for (j, (child_coin, recipient, value)) in children.iter_mut().enumerate() {
        let child_vout = journal.children[j].sp_vout;
        debug_assert_eq!(*value, journal.children[j].value); // journalled above, and read from the record
        let ladder = establish_child_journalled(cc, wallet_name, child_coin, &mut journal, j).await?;
        let child_sid = child_coin
            .statechain_id
            .clone()
            .ok_or_else(|| anyhow::anyhow!("child coin has no statechain_id"))?;
        // [F1] The child is deliberately left NON-terminal, so the receiver can complete the standard
        // key handover and hold a first-class, re-transferable coin (CHILDREN.md). A child
        // is not defenceless without terminality — two mechanisms cover the two windows:
        //   * PRE-conveyance: a rival the sender co-signed over SP.out[j] before conveying is caught by
        //     the exact-equality census (`child_num_sigs == CHILD_V2_BASELINE + tiers + superseded`),
        //     which the receiver checks against the SE's authoritative count.
        //   * POST-conveyance: `convey_child_bundle`'s `get_new_x1` opens a transfer on the child, and
        //     the coordinator's pending-transfer lock then refuses every further sender co-sign until
        //     the transfer completes — at which point the receiver's auth rotation makes the lockout
        //     PERMANENT. (The temporary lock is what bridges census → key_updated.)
        // The LATCHED lane is the exception and re-applies terminality in `in_ladder_pay`: there the
        // piece sits unclaimed until an LN preimage lands, i.e. deliberately past the lock's window.
        bundles.push(ChildTesrBundle {
            parent: parent_seg.clone(),
            parent_statechain_id: parent_sid.clone(),
            sp_vout: child_vout,
            child_statechain_id: child_sid,
            child_owner_exit_address: recipient.clone(),
            child_extension: ladder.extension,
            child_state: ladder.state,
            child_superseded_states: vec![],
            child_superseded_extensions: vec![],
            ancestors: vec![],
            // PLAIN split: a coloured parent never reaches here (`refuse_uncolored_over_colored`
            // is this function's first statement), so a child carved here carries no allocation.
            rgb: None,
            parent_flat_backups: parent_backups.clone(),
        });
    }
    // Complete: the bundles exist and are rebuildable from disk. The caller marks the record
    // COMMITTED (`journal_commit`) only once it has persisted/conveyed them — a crash in the window
    // between this return and that persistence is still recoverable, which is the whole point.
    journal.stage = SplitStage::Established;
    journal_write(cc, wallet_name, &journal).await?;
    Ok(bundles)
}

/// Off-chain renewal at the schedule cadence: the new extension takes CSV `E0 − (m+1)·δE` and the
/// fresh state `D0`, both from the bundle's [`TesrParams`]. Returns `true` if a rollover is due
/// afterwards (`m` reached `m_max`), so the caller rolls over before the extension floor.
pub async fn renew_auto(cc: &ClientConfig, coin: &mut Coin, bundle: &mut TesrBundle) -> Result<bool> {
    let p = bundle.params;
    let next_m = (bundle.m + 1) as u16;
    renew(cc, coin, bundle, p.ext_csv(next_m), p.state_csv(0)).await?;
    Ok(p.needs_rollover(bundle.m as u16))
}

/// Off-chain rollover at the schedule cadence: a fresh level with extension `E0` and state `D0`.
pub async fn rollover_auto(cc: &ClientConfig, coin: &mut Coin, bundle: &mut TesrBundle) -> Result<()> {
    let p = bundle.params;
    rollover(cc, coin, bundle, p.ext_csv(0), p.state_csv(0)).await
}

/// Persist a bundle under `tesr-<statechain_id>` (replaces any prior bundle for the coin).
pub async fn persist(cc: &ClientConfig, wallet_name: &str, bundle: &TesrBundle) -> Result<()> {
    let json = serde_json::to_string(bundle)?;
    crate::sqlite_manager::insert_raw_backup_txs(
        &cc.pool,
        wallet_name,
        &format!("tesr-{}", bundle.statechain_id),
        &json,
    )
    .await
}

/// Load a coin's persisted TES-R bundle from the wallet DB, if any.
pub async fn load(cc: &ClientConfig, wallet_name: &str, statechain_id: &str) -> Result<Option<TesrBundle>> {
    let key = format!("tesr-{statechain_id}");
    for (k, json) in crate::sqlite_manager::get_all_backup_txs(&cc.pool, wallet_name).await? {
        if k == key {
            return Ok(Some(serde_json::from_str(&json)?));
        }
    }
    Ok(None)
}

/// **[P0-2] THE DEPTH CAP.** Refuse a split that would mint a child too deep to survive one funding
/// epoch, derived from the LIVE schedule and the SE's live `lockheight_init` — never a literal.
///
/// Depth grows by exactly +1 per in-ladder payment and never resets (a child cannot be re-anchored:
/// `refresh()` routes a `ctesr-` coin to `unilateral_exit`, because `SP.out[j]` is un-broadcast and
/// there is no confirmed outpoint to co-operatively spend). Exit latency compounds with it —
/// `2124·d + 2160` blocks on the mainnet schedule — so past some depth the exit no longer fits in
/// the `lockheight_init`-block epoch at all: the coin can never be parked across an epoch boundary,
/// and every receiver's [`verify_conveyed_child`] would refuse it for the whole of every epoch. That
/// coin must not be minted in the first place, while the parent is still whole and spendable.
///
/// `new_depth` is the depth of the child the split is about to create (a root split child is 1).
async fn enforce_split_depth_cap(
    cc: &ClientConfig,
    p: mercurylib::tesr::TesrParams,
    new_depth: u32,
) -> Result<()> {
    // Fail CLOSED: an unreadable epoch is a refusal, not an assumed-generous default.
    let info = crate::utils::info_config(cc).await.map_err(|e| {
        anyhow::anyhow!(
            "in-ladder split refused: the SE's funding-epoch length (`lockheight_init`) could not be \
             read ({e}), so the exit-latency cap on split depth cannot be evaluated — refusing rather \
             than minting a child that may be unmaterialisable"
        )
    })?;
    let epoch_blocks = info.initlock;
    // A depth-1 exit chain and the cost of one further level, both from the live schedule:
    //   T (no timelock) | X_m E0 | SP 0 | ext_child E0 | state_child D0
    // and each extra level adds its own extension + split state.
    //
    // [CATS] The split states are `SPINE_CSV`, not `D0 − δ`. Keeping the old `state_csv(1)` here
    // would not have been "conservative" in any useful sense: this function is what decides how deep
    // a payment chain may go, so a model that charges ~2 124 blocks for a tier that actually waits
    // zero refuses payments the chain would carry perfectly well — a silent economic cap enforced by
    // a stale constant. The value must track the builders, which is why both read `SPINE_CSV`.
    let base = vec![
        None,
        Some(p.ext_csv(0)),
        Some(SPINE_CSV),
        Some(p.ext_csv(0)),
        Some(p.state_csv(0)),
    ];
    let per_level = vec![Some(p.ext_csv(0)), Some(SPINE_CSV)];
    let max_depth = mercurylib::transfer::receiver::max_split_depth(&base, &per_level, epoch_blocks);
    if new_depth > max_depth {
        let mut chain = base.clone();
        for _ in 1..new_depth {
            chain.splice(3..3, per_level.iter().cloned());
        }
        return Err(anyhow::anyhow!(
            "{}",
            mercurylib::transfer::receiver::SplitDepthCapExceeded {
                depth: new_depth,
                max_depth,
                required: mercurylib::transfer::receiver::exit_wait_blocks(&chain),
                epoch_blocks,
            }
        ));
    }
    Ok(())
}

/// Flat-backup **MINIMUM** of a natively-laddered on-chain PARENT coin. `sig_count` starts at 0
/// (`generated_public_key DEFAULT 0`); the coin's on-chain deposit confirmation co-signs exactly ONE
/// signed-once backup tx (`coin_status::check_deposit` → `create_tx1` for a non-single-use coin) before
/// the ladder is established.
///
/// ⚠️ It is a MINIMUM, not the count. This constant used to be fed to `verify_child_bundle` as the
/// parent's `flat_backups`, on the reasoning that a split-child receiver cannot observe the parent's
/// history. That was only true for a parent the SENDER had deposited: every whole-coin hop co-signs
/// one further flat backup (`transfer_sender::create_backup_tx_to_receiver`), so a parent received
/// `k` times carries `1 + k` and the exact-equality census came up `k` short — an in-ladder split of
/// a RECEIVED laddered coin produced a child NO receiver could adopt, after the sender had already
/// terminalized the parent and booked the piece away.
///
/// The real count now travels in `ChildTesrBundle::parent_flat_backups` and is re-derived by the
/// receiver from the validated chain (`verify_conveyed_child`). What remains of this constant is the
/// floor both sides still check: a conveyed chain SHORTER than this did not come from a deposited
/// coin and is refused.
pub const PARENT_V2_BASELINE: u32 = 1;

/// Flat-backup baseline of a split **CHILD** slot: it is an SE-registered key that is NEVER funded
/// on-chain (its funding is the un-broadcast `SP.out[j]`), so `check_deposit`/`create_tx1` never runs
/// for it and `num_sigs` counts ONLY the two child tiers co-signed at split time. Baseline `0`.
pub const CHILD_V2_BASELINE: u32 = 0;

/// **[CATS] The relative timelock of a SPLIT STATE (`SP` / `CSP`) — zero, always.**
///
/// A split state is a *spine* tier, and a spine tier is a different KIND from the `state` tiers
/// whose CSV walks down the `[d_floor, d0]` schedule. Two facts make zero right rather than merely
/// cheap:
///
/// * **It wins by the largest possible margin.** Over the outpoint it spends, the only competing
///   transaction is the state it replaces (`S_0`, or the child's previous state), whose CSV is
///   necessarily ≥ `d_floor`. Replace-by-lower-timelock is the invariant; `0` is its extreme, and
///   the superseded battery's `sup.csv <= live_csv → reject` passes with the whole schedule to spare.
/// * **The [B1] asymmetry does not arise.** An un-timelocked tier is dangerous when a party can
///   retain it and void someone else's material — `T` over `F`. `SP` is signed by the sole current
///   owner of the outpoint it spends, on the outpoint it is simultaneously giving up: the voiding
///   party and the victim are the same entity.
///
/// What this REPLACES is `s0_csv − δ`, which consumed one state rung per split and produced the
/// defect that made the whole design uneconomic: a coin could be partially paid from only as many
/// times as it had rungs left, and a coloured carrier — which never renews — exactly **once, ever**
/// (`docs/utexo/PARTIAL-PAYMENT-ECONOMICS.md` §1.3). At zero a split consumes no rung at all, and
/// the SP contributes one block (its parent's confirmation) to the exit walk instead of ~2 124.
///
/// The liveness trade this buys into is real and is stated in §4.8, not hidden: zero-CSV tiers
/// accelerate the honest exit and a theft identically.
pub const SPINE_CSV: u16 = 0;

/// Persist a split child bundle under `ctesr-<child_statechain_id>` (replaces any prior).
pub async fn persist_child(cc: &ClientConfig, wallet_name: &str, cb: &ChildTesrBundle) -> Result<()> {
    let json = serde_json::to_string(cb)?;
    crate::sqlite_manager::insert_raw_backup_txs(
        &cc.pool,
        wallet_name,
        &format!("ctesr-{}", cb.child_statechain_id),
        &json,
    )
    .await
}

/// Load a coin's persisted split child bundle from the wallet DB, if any.
pub async fn load_child(cc: &ClientConfig, wallet_name: &str, child_statechain_id: &str) -> Result<Option<ChildTesrBundle>> {
    let key = format!("ctesr-{child_statechain_id}");
    for (k, json) in crate::sqlite_manager::get_all_backup_txs(&cc.pool, wallet_name).await? {
        if k == key {
            return Ok(Some(serde_json::from_str(&json)?));
        }
    }
    Ok(None)
}

/// Statechain ids of all coins backed by a RECEIVED in-ladder split child bundle (a persisted
/// `ctesr-<id>`). These are FIRST-CLASS coins — the claim completed the SE key handover, so they can be
/// paid onward off-chain via [`child_retransfer`] — but their funding `SP.out[j]` is un-broadcast, so
/// callers use this set to treat them specially: spendable only WHOLE (never chosen as the coin to
/// split), and withdrawn by unilateral exit rather than a cooperative on-chain withdrawal.
/// One wallet-DB read.
pub async fn child_claim_sids(cc: &ClientConfig, wallet_name: &str) -> Result<std::collections::HashSet<String>> {
    let mut set = std::collections::HashSet::new();
    for (k, _json) in crate::sqlite_manager::get_all_backup_txs(&cc.pool, wallet_name).await? {
        if let Some(sid) = k.strip_prefix("ctesr-") {
            set.insert(sid.to_string());
        }
    }
    Ok(set)
}

/// **[B1] The relative timelock a SIGNED tier actually carries**, read off its `nSequence` under
/// BIP-68 — the only copy of that number Bitcoin will ever enforce, and therefore the only one a
/// verifier may compute with.
///
/// `Ok(None)` means the relative lock is DISABLED (bit 31 set — the trigger's
/// `mercurylib::tesr::TRIGGER_SEQUENCE`). A time-denominated lock (bit 22 set) is a refusal, not a
/// zero: this schedule is expressed entirely in blocks, so a tier that waits in 512-second units
/// cannot be placed on the exit timeline at all and must never be silently counted as costing
/// nothing.
fn signed_relative_csv(
    tx: &electrum_client::bitcoin::Transaction,
    what: &str,
) -> Result<Option<u16>> {
    let seq = tx
        .input
        .first()
        .ok_or_else(|| anyhow::anyhow!("{what}: has no input, so it carries no timelock to read"))?
        .sequence
        .0;
    if seq & (1 << 31) != 0 {
        return Ok(None);
    }
    if seq & (1 << 22) != 0 {
        return Err(anyhow::anyhow!(
            "{what}: nSequence encodes a 512-second BIP-68 lock, which this block-denominated exit \
             schedule cannot express — refusing rather than counting it as no wait at all"
        ));
    }
    Ok(Some(seq as u16))
}

/// Human names for [`child_exit_chain`]'s entries, in the same order, so a refusal can say WHICH tier
/// lied. Derived from the same three loops the chain itself is built from; a length disagreement is a
/// programming error and is caught by the caller rather than papered over with an index.
fn child_exit_labels(cb: &ChildTesrBundle) -> Vec<String> {
    let mut v = vec!["parent trigger".to_string()];
    for l in 0..cb.parent.levels.len() {
        v.push(format!("parent level {l} extension"));
        v.push(format!("parent level {l} state"));
    }
    // [CATS] ONE entry per tier the segment actually has. `child_exit_chain` below is guarded by the
    // IDENTICAL condition, in the same commit and for the same reason: these two loops are reconciled
    // only by a length check in `child_exit_chain_bound`, so guarding one and not the other either
    // refuses every CATS bundle as an internal error, or — if the lengths happen to agree — silently
    // MIS-PAIRS label with tier and makes `bind_declared_csv` compare a state's declared CSV against
    // an extension's signed one. Edit them together or not at all.
    for (i, seg) in cb.ancestors.iter().enumerate() {
        if seg.extension.is_some() {
            v.push(format!("ancestor {i} extension"));
        }
        v.push(format!("ancestor {i} state"));
    }
    v.push("child extension".to_string());
    v.push("child state".to_string());
    v
}

/// **[B1] The child's exit chain with every timelock BOUND to the signature that enforces it** — the
/// only form of the chain an admission decision may be computed from.
///
/// [`child_exit_chain`] reports each tier's `csv` FIELD, which on a conveyed bundle is plain
/// attacker-supplied serde. This parses every tier instead, reads the timelock from `nSequence`, and
/// refuses (`DeclaredCsvMismatch`) if the two disagree — so the exit-headroom gate can no longer be
/// handed a schedule of the sender's choosing, and a sender who declares `csv: 1` on a tier the SE
/// co-signed at 2 124 is refused by name instead of admitted at 1.
///
/// The returned timelocks are the SIGNED ones.
pub fn child_exit_chain_bound(cb: &ChildTesrBundle) -> Result<Vec<(String, Option<u16>)>> {
    use electrum_client::bitcoin::{consensus::deserialize, Transaction};
    let declared = child_exit_chain(cb);
    let labels = child_exit_labels(cb);
    if labels.len() != declared.len() {
        return Err(anyhow::anyhow!(
            "internal: exit chain has {} tiers but {} labels — refusing to verify a chain this code \
             cannot describe",
            declared.len(),
            labels.len()
        ));
    }
    let mut bound = Vec::with_capacity(declared.len());
    for (i, (signed_hex, declared_csv)) in declared.into_iter().enumerate() {
        let what = &labels[i];
        let raw = hex::decode(&signed_hex)
            .map_err(|e| anyhow::anyhow!("{what}: signed tx hex does not decode ({e})"))?;
        let tx: Transaction = deserialize(&raw)
            .map_err(|e| anyhow::anyhow!("{what}: signed tx does not parse ({e})"))?;
        let signed_csv = signed_relative_csv(&tx, what)?;
        let csv = mercurylib::transfer::receiver::bind_declared_csv(
            i,
            what,
            declared_csv,
            signed_csv,
        )?;
        bound.push((signed_hex, csv));
    }
    Ok(bound)
}

/// The full unilateral-exit chain of a split child, in broadcast order:
/// `T -> X_m -> SP` (parent segment) then `ext_child -> state_child`. Each entry is
/// `(signed_tx_hex, relative_csv)` — the trigger has no CSV.
///
/// ⚠️ **[B1] The `csv` here is the bundle's DECLARED field**, which on conveyed material is
/// attacker-supplied. It is safe for the two broadcast-side callers below (they either ignore it or
/// use it as a wait-time hint about the caller's OWN persisted bundle) and it is NOT safe for any
/// admission decision. Anything that computes a requirement, a deadline or a cap from these
/// timelocks must use [`child_exit_chain_bound`], which reads them from the signed transactions.
pub fn child_exit_chain(cb: &ChildTesrBundle) -> Vec<(String, Option<u16>)> {
    let mut chain: Vec<(String, Option<u16>)> =
        cb.parent.exit_tiers().iter().map(|t| (t.signed_tx.clone(), t.csv)).collect();
    // Splice EVERY intermediate segment, root→leaf, before the leaf's own tiers. Omitting these is
    // not a mere verification gap — the leaf's funding outpoint would never be created on-chain, so
    // the exit would stall forever and the value would be UNRECOVERABLE. Order is the broadcast
    // order: each segment's extension, then its state (which funds the next level down).
    //
    // [CATS] A SPINE segment (`extension: None`) contributes exactly ONE entry: its state re-anchors
    // on the segment's own funding outpoint, so there is no extension to broadcast between them.
    // Kept in lock-step with `child_exit_labels` — see the note there.
    for seg in cb.ancestors.iter() {
        if let Some(ext) = &seg.extension {
            chain.push((ext.signed_tx.clone(), ext.csv));
        }
        chain.push((seg.state.signed_tx.clone(), seg.state.csv));
    }
    chain.push((cb.child_extension.signed_tx.clone(), cb.child_extension.csv));
    chain.push((cb.child_state.signed_tx.clone(), cb.child_state.csv));
    chain
}

/// **Owner-initiated unilateral exit of a split CHILD coin.** Broadcasts the child's full pre-co-signed
/// chain (`T -> X_m -> SP -> ext_child -> state_child`) in order, each tier once its relative-CSV is met,
/// stopping at the first not-yet-mature tier. Keyless (the receiver never co-signs — every tx is already
/// signed and `state_child` pays the receiver's own key). Idempotent: call once per block; already-known
/// tiers are skipped. Returns a typed [`ExitProgress`]; **`Err` = blind** (external review F4) — a
/// backend that could not be read, or stored child material that could not be decoded, is never
/// reported as an ordinary "nothing broadcast this pass, keep waiting".
pub fn exit_child_pass(electrum: &electrum_client::Client, cb: &ChildTesrBundle) -> Result<ExitProgress> {
    let mut broadcast = Vec::new();
    let mut stalled = None;
    for (signed, _csv) in child_exit_chain(cb) {
        let raw = hex::decode(&signed)
            .map_err(|e| anyhow::anyhow!("child exit chain carries unusable signed tx hex: {e}"))?;
        // Derive the txid to skip already-known tiers without re-broadcasting.
        let txid = {
            use electrum_client::bitcoin::{consensus::deserialize, Transaction};
            deserialize::<Transaction>(&raw)
                .map_err(|e| anyhow::anyhow!("child exit chain tx did not deserialize: {e}"))?
                .txid()
                .to_string()
        };
        if tx_known(electrum, &txid)? {
            continue;
        }
        match electrum.transaction_broadcast_raw(&raw) {
            Ok(_) => broadcast.push(txid),
            Err(e) => {
                // CSV not met / parent unconfirmed — retry next pass, but SAY SO.
                stalled = Some(format!("{txid}: {e}"));
                break;
            }
        }
    }
    let complete = tx_known(electrum, &cb.child_state.txid)?;
    Ok(ExitProgress { broadcast, complete, stalled })
}

/// The relative-CSV of the first child-exit tier not yet on-chain (a wait-time hint), or `Ok(None)`
/// once the child exit is complete. Mirrors [`next_exit_tier`] for a split child chain, including
/// its fail-closed contract: **`Err` = blind**, never a fabricated wait time.
///
/// [B1] The hint is the SIGNED timelock ([`child_exit_chain_bound`]), not the declared one: a wait
/// time a watchtower schedules against is exactly the kind of number that must not be quotable by
/// whoever sent the bundle, and a stored row whose two copies disagree is corruption to report, not
/// to average over.
pub fn next_child_exit_tier(electrum: &electrum_client::Client, cb: &ChildTesrBundle) -> Result<Option<u16>> {
    use electrum_client::bitcoin::{consensus::deserialize, Transaction};
    for (signed, csv) in child_exit_chain_bound(cb)? {
        let raw = hex::decode(&signed)
            .map_err(|e| anyhow::anyhow!("child exit chain carries unusable signed tx hex: {e}"))?;
        let txid = deserialize::<Transaction>(&raw)
            .map_err(|e| anyhow::anyhow!("child exit chain tx did not deserialize: {e}"))?
            .txid()
            .to_string();
        if !tx_known(electrum, &txid)? {
            return Ok(Some(csv.unwrap_or(0)));
        }
    }
    Ok(None)
}

/// Fetch the authoritative inputs a split-child receiver needs and run [`verify_child_bundle`] — the
/// verify-ONLY core (no persistence). Reads `F.spk` from chain, the parent+child
/// `num_sigs`/`aggregate_pubkey` from `/info/statechain`, and both terminality flags from
/// `/statechain/spend_budget` (fail-closed), and checks the child pays `receiver_backup_address`
/// (Model A). Returns the child's exit value on success. Used by claim()'s validation pass.
pub async fn verify_conveyed_child(
    cc: &ClientConfig,
    receiver_backup_address: &str,
    cb: &ChildTesrBundle,
) -> Result<u64> {
    use electrum_client::bitcoin::Txid;
    // F.spk from chain (also proves F is known to the chain; unspent/confirmed is enforced by the
    // terminality of the parent — a terminal parent's only live spend is the disclosed T).
    let f_txid = Txid::from_str(&cb.parent.f_txid)
        .map_err(|_| anyhow::anyhow!("bad parent F txid"))?;
    let f_tx = cc
        .electrum_client
        .transaction_get(&f_txid)
        .map_err(|_| anyhow::anyhow!("parent F {} not found on chain", cb.parent.f_txid))?;
    let f_out = f_tx
        .output
        .get(cb.parent.f_vout as usize)
        .ok_or_else(|| anyhow::anyhow!("parent F has no output {}", cb.parent.f_vout))?;
    let f_spk_hex = hex::encode(f_out.script_pubkey.as_bytes());
    // ═══ [VALUE-CONSERVATION] THE ONE NUMBER BITCOIN ALREADY AGREED TO ═══
    //
    // The value laws bind each tier to the one above it, so the structure conserves value RELATIVE to
    // the parent trigger's payload output — and that output floats free unless something ties it to
    // `F`. A relative chain anchored to a free value is a free chain: a sender whose trigger pays far
    // less than `F` holds gets a ladder that conserves perfectly against a fiction, while the receiver
    // books the real on-chain value.
    //
    // `37d8bba` tied it here, in this async wrapper, because `verify_bundle_ex` had no chain access.
    // It is now a REQUIRED PARAMETER of `verify_child_bundle` (below) and an explicit `Some(..)` on
    // `verify_bundle_ex`, so the anchor lives where the laws live and every caller of the synchronous
    // verifier inherits it. This is the only place that fetches it; the checks it feeds are two levels
    // down. Do not re-add a second copy here — one law, one site.
    let f_onchain_value = f_out.value;

    // [D1] THE PARENT'S FLAT-BACKUP COUNT, VALIDATED THEN COUNTED — the child-lane analogue of the
    // whole-coin path's `transfer_msg.backup_transactions.len()` (transfer_receiver.rs [S2]).
    //
    // The census term must be the parent's REAL flat-backup count (`1 + k` after `k` whole-coin
    // hops), which no constant can express; it is conveyed. Conveyed is sender-supplied, so it is
    // validated to exactly the standard the whole-coin lane holds its own chain to, plus one
    // strictly stronger binding this lane can afford:
    //
    //   * every entry is a taproot key-spend of the ON-CHAIN `F` we just fetched, signature-verified
    //     under `F.spk`'s key (= `A_parent`, itself bound to the server's recorded parent aggregate
    //     below) — so each entry cost the attacker a real SE co-sign and cannot pad the count for
    //     free;
    //   * INV-5 (`ladder_decrements_by_interval`): consecutive locktimes fall by EXACTLY `interval`,
    //     which rejects duplicate padding (decrement 0) and chain inversion alike;
    //   * [stronger than the whole-coin lane] each entry's prevout is pinned to `(F.txid, F.vout)`.
    //     `verify_transaction_signature` only indexes `tx0.output[prevout.vout]`, so without this a
    //     backup naming a foreign txid would still verify; here the funding outpoint is a field of
    //     the bundle already bound to the chain, so there is no reason to leave it loose.
    //
    // NOTE ON AGEING, since this is the obvious worry: `validate_backup_chain_v2` refuses a chain
    // whose locktimes have run out or whose committed fee has fallen below the live rate, and a
    // laddered coin's flat backups DO age while the ladder itself does not. That does not make this
    // lane stricter than the alternative: the whole-coin receive path validates the very same
    // entries at claim time and additionally appends one at `lowest − interval`, so it hits
    // `LocktimeTooLow` STRICTLY EARLIER than this does. A parent whose flat chain has aged out is
    // already untransferable as a whole coin; it is not newly unsplittable.
    let parent_backups = &cb.parent_flat_backups;
    if (parent_backups.len() as u32) < PARENT_V2_BASELINE {
        return Err(anyhow::anyhow!(
            "conveyed child discloses {} parent flat backup transaction(s), fewer than the \
             {PARENT_V2_BASELINE} every deposited coin carries — the ancestor census cannot be \
             balanced (fail-closed)",
            parent_backups.len()
        ));
    }
    let (epoch_expiry_height, tip) = {
        use electrum_client::bitcoin::consensus::{deserialize, serialize};
        for (i, b) in parent_backups.iter().enumerate() {
            let tx: electrum_client::bitcoin::Transaction =
                deserialize(&hex::decode(&b.tx).map_err(|_| {
                    anyhow::anyhow!("parent flat backup {i}: not hex")
                })?)
                .map_err(|_| anyhow::anyhow!("parent flat backup {i}: not a transaction"))?;
            let inp = tx
                .input
                .first()
                .ok_or_else(|| anyhow::anyhow!("parent flat backup {i}: no input"))?;
            if tx.input.len() != 1
                || inp.previous_output.txid != f_txid
                || inp.previous_output.vout != cb.parent.f_vout
            {
                return Err(anyhow::anyhow!(
                    "parent flat backup {i} does not spend the parent's funding outpoint \
                     {}:{} — refusing to count it toward the ancestor census",
                    cb.parent.f_txid,
                    cb.parent.f_vout
                ));
            }
        }
        let info_config = crate::utils::info_config(cc).await?;
        let blockheight = cc
            .electrum_client
            .block_headers_subscribe_raw()
            .map_err(|e| anyhow::anyhow!("cannot read the chain tip: {e}"))?
            .height as u32;
        let current_fee_rate = if info_config.fee_rate_sats_per_byte > cc.max_fee_rate {
            cc.max_fee_rate
        } else {
            info_config.fee_rate_sats_per_byte
        };
        // ═══ [VALUE-CONSERVATION] IS THE YARDSTICK OURS? ═══
        //
        // The conservation laws in `verify_child_bundle` and `verify_bundle_ex` all compute
        // `expect = prev − committed_fee(rate) − P2A`, and `rate` is `cb.parent.fee_rate` — a plain
        // `f64` on the CONVEYED bundle. `expect` DECREASES as `rate` rises, so a sender who declares
        // a large enough rate makes a tier that forwards almost nothing satisfy the equality exactly.
        // Unbounded, that is not a rounding concern: it is a complete bypass of every law landed in
        // 4e165e6, deed25c and d692c07, restoring the original skim through the measuring stick
        // instead of through the outputs.
        //
        // The yardstick is a CONSTANT, not a market rate: every establish path builds at
        // `TesrParams::committed_fee_rate` (`establish_auto` -> `p.committed_fee_rate`), which is 2.0
        // on every shipped preset. So this is exact equality against the RECEIVER's own preset,
        // derived from its network — never `cb.parent.params`, which is conveyed alongside the rate
        // and would let the sender move both ends of the comparison together.
        //
        // Note what the ceiling must NOT be: `cc.max_fee_rate` caps the flat BACKUP fee and is `1` on
        // the regtest profile, below the 2.0 every honest ladder carries — using it here would refuse
        // all legitimate traffic. Two different quantities with similar names; the first draft of this
        // check used the wrong one.
        let expected_rate = mercurylib::tesr::TesrParams::for_network(&cc.network.to_string())
            .committed_fee_rate;
        if cb.parent.fee_rate != expected_rate {
            return Err(anyhow::anyhow!(
                "conveyed bundle declares a committed fee rate of {} sat/vB but this network builds \
                 ladders at {expected_rate} — that number is the yardstick every value-conservation \
                 check measures against, and an inflated one lets a tier forwarding almost nothing \
                 satisfy them all",
                cb.parent.fee_rate
            ));
        }
        // The RETURN VALUE is load-bearing, not a formality: it is the LOWEST locktime of the
        // validated chain, i.e. the first height at which the parent's current owner (the sender of
        // this child) can broadcast a flat backup that spends `F` and voids the entire tree. That is
        // this coin's epoch expiry, and it is the only absolute clock anywhere in the structure.
        let lowest_locktime = mercurylib::transfer::receiver::validate_backup_chain_v2(
            parent_backups,
            &hex::encode(serialize(&f_tx)),
            blockheight,
            cc.fee_rate_tolerance,
            current_fee_rate,
            info_config.initlock,
            info_config.interval,
        )
        .map_err(|e| {
            anyhow::anyhow!("conveyed parent flat backup chain is invalid ({e}) — the ancestor census term is unusable")
        })?;
        (lowest_locktime, blockheight)
    };

    // [P0-1] EXIT-HEADROOM ADMISSION GATE. Until this existed, the only bound on a conveyed child
    // was `lock_time > tip` inside `validate_backup_chain_v2` above — so a sender could hand over a
    // coin whose exit provably could not complete inside the epoch it was minted in, and for the
    // last `WAIT(d)` blocks of every epoch (43% of it at mainnet depth 1) that was every coin they
    // sent. The census balanced, Model A held, and the coin was worthless.
    //
    // The requirement is read off THIS bundle's own exit chain — its real depth (`ancestors`) and the
    // CSVs actually co-signed into its tiers — so it tracks the live schedule with no constant to go
    // stale. See `mercurylib::transfer::receiver`'s module note for why the whole chain must fit and
    // not merely the trigger.
    //
    // [B1] EVERY TERM IS RECEIVER-DERIVED. The gate was bypassable for as long as it read the CSVs
    // from `TesrTier::csv`, a plain serde field on the conveyed bundle: declare `csv: 1` everywhere
    // and the requirement collapses to a handful of blocks while the chain still enforces thousands.
    // `child_exit_chain_bound` reads each timelock from the SIGNED transaction's `nSequence` and
    // refuses any bundle whose declared schedule contradicts its own signatures. The other two terms
    // were already ours: `tip` comes from this wallet's chain backend, and `epoch_expiry_height` is
    // the lowest locktime of a flat backup chain just validated against the on-chain `F` — every
    // entry signature-verified under `F`'s key, prevout-pinned, INV-5 strictly decrementing, capped
    // at `tip + lockheight_init` by `verify_if_locktime_is_reasonable_tx_version_and_output_size`
    // (so it cannot be inflated past one epoch) and its COUNT pinned by the exact-equality census
    // below (so low entries cannot be dropped to raise the minimum). The chain's LENGTH is likewise
    // not free: `verify_child_bundle` links every tier to its parent's outpoint, so a segment cannot
    // be omitted to shorten the walk without breaking the funding chain outright.
    let exit_csvs: Vec<Option<u16>> =
        child_exit_chain_bound(cb)?.into_iter().map(|(_, csv)| csv).collect();
    mercurylib::transfer::receiver::check_exit_headroom(&exit_csvs, tip, epoch_expiry_height)
        .map_err(|e| {
            anyhow::anyhow!(
                "conveyed child refused at depth {}: {e}",
                cb.ancestors.len() + 1
            )
        })?;

    let p_info = crate::utils::get_statechain_info(&cb.parent_statechain_id, cc)
        .await?
        .ok_or_else(|| anyhow::anyhow!("no statechain info for parent sid"))?;
    let c_info = crate::utils::get_statechain_info(&cb.child_statechain_id, cc)
        .await?
        .ok_or_else(|| anyhow::anyhow!("no statechain info for child sid"))?;

    // Only the PARENT's terminality is fetched: the child's census is made durable by the handover the
    // receiver completes in this same claim, not by terminality (see verify_child_bundle's [F2]).
    let (_, _, parent_terminal) =
        crate::lightning_latch::get_spend_budget(cc, &cb.parent_statechain_id).await?;

    // Each INTERMEDIATE segment is an ancestor (not handed over here), so it must be terminal and its
    // census must balance — fetch the SE's authoritative facts for each, in root→leaf order.
    let mut ancestor_facts: Vec<AncestorFacts> = Vec::with_capacity(cb.ancestors.len());
    for seg in cb.ancestors.iter() {
        let info = crate::utils::get_statechain_info(&seg.statechain_id, cc)
            .await?
            .ok_or_else(|| anyhow::anyhow!("no statechain info for ancestor {}", seg.statechain_id))?;
        let (_, _, terminal) = crate::lightning_latch::get_spend_budget(cc, &seg.statechain_id).await?;
        ancestor_facts.push(AncestorFacts {
            num_sigs: info.num_sigs,
            aggregate_pubkey: info.aggregate_pubkey.clone(),
            terminal,
        });
    }

    verify_child_bundle(
        cb,
        &f_spk_hex,
        f_onchain_value,
        p_info.num_sigs,
        // The REAL count, re-derived from the chain validated above — never the baseline constant.
        parent_backups.len() as u32,
        p_info.aggregate_pubkey.as_deref(),
        parent_terminal,
        c_info.num_sigs,
        CHILD_V2_BASELINE,
        c_info.aggregate_pubkey.as_deref(),
        &ancestor_facts,
        receiver_backup_address,
    )?;
    Ok(cb.child_state.out_value)
}

/// [`verify_conveyed_child`] + persist the bundle so the receiver can [`exit_child_pass`] it. The
/// RECEIVER side of an in-ladder split payment (the analogue of adopting a conveyed `tesr_ladder`).
pub async fn adopt_child_bundle(
    cc: &ClientConfig,
    wallet_name: &str,
    receiver_backup_address: &str,
    cb: &ChildTesrBundle,
) -> Result<u64> {
    let value = verify_conveyed_child(cc, receiver_backup_address, cb).await?;
    persist_child(cc, wallet_name, cb).await?;
    Ok(value)
}

/// CHILD-LEVEL IN-LADDER SPLIT — pay a NON-EXACT amount out of a received child.
///
/// The child analogue of `in_ladder_split`. The child's own state is replaced by a SPLIT state `CSP`
/// (one δ lower, so it out-races the state it replaces over `ext_child.out[0]`) paying N grandchildren;
/// each grandchild then gets its own headless ladder off `CSP.out[j]`. The child itself becomes an
/// INTERMEDIATE segment in each grandchild's bundle: it is terminalized (it is an ancestor now, not a
/// coin being handed over), and the receiver's `verify_child_bundle` walks it via `cb.ancestors`.
///
/// Returns one bundle per grandchild, in `children` order.
pub async fn child_in_ladder_split(
    cc: &ClientConfig,
    wallet_name: &str,
    child_coin: &mut Coin,
    cb: &ChildTesrBundle,
    children: &mut [(Coin, String, u64)],
) -> Result<Vec<ChildTesrBundle>> {
    refuse_uncolored_over_colored_child(cb, "child_in_ladder_split")?;
    let p = cb.parent.params;
    let n = children.len();
    if n == 0 {
        return Err(anyhow::anyhow!("a child split needs at least one grandchild"));
    }
    // [P0-2] THE DEPTH CAP. `cb` sits at depth `ancestors.len() + 1`; splitting it pushes `cb`'s own
    // segment into `ancestors`, so every grandchild lands one level deeper. Checked BEFORE anything
    // irreversible (the child's terminalization is two statements below): a refusal here leaves the
    // child whole, spendable and re-transferable.
    enforce_split_depth_cap(cc, p, cb.ancestors.len() as u32 + 2).await?;
    let old_csv = cb
        .child_state
        .csv
        .ok_or_else(|| anyhow::anyhow!("child state has no CSV — cannot split"))?;
    // [CATS] CSP out-races the state it replaces over `ext_child.out[0]` at CSV 0 — see `SPINE_CSV`.
    // A received child cannot renew (`renew`/`rollover` take `&mut TesrBundle` and there is no
    // `ChildTesrBundle` analogue), so `old_csv − δ` gave a child a hard, unreplenishable budget of
    // onward partial payments. At zero there is no budget to run out of.
    if old_csv <= SPINE_CSV {
        return Err(anyhow::anyhow!(
            "this child's state has CSV {old_csv}, which does not exceed the spine CSV {SPINE_CSV} \
             — CSP could not out-race it, so the split is refused"
        ));
    }
    let csp_csv = SPINE_CSV;

    // Value conservation: the grandchildren share the split total exactly (no mint, no burn).
    let total = mercurylib::tesr::tier_out_total(cb.child_extension.out_value, n, cb.parent.fee_rate)
        .ok_or_else(|| anyhow::anyhow!("committed fee too high to split this child into {n}"))?;
    let sum: u64 = children.iter().map(|(_, _, v)| *v).sum();
    if sum != total {
        return Err(anyhow::anyhow!(
            "grandchild values sum to {sum} but must equal {total} (= ext_child.out[0] − committed fee)"
        ));
    }

    // CSP pays each grandchild's aggregate address, in order (CSP.out[j] == children[j]).
    let payees: Vec<(String, u64)> = children
        .iter()
        .map(|(c, _, v)| {
            c.aggregated_address
                .clone()
                .ok_or_else(|| anyhow::anyhow!("grandchild coin has no aggregated_address"))
                .map(|a| (a, *v))
        })
        .collect::<Result<_>>()?;
    let csp = mercurylib::tesr::build_split_state(
        &cb.child_extension.txid,
        cb.child_extension.out_value,
        &payees,
        &cb.parent.network,
        csp_csv,
        cb.parent.fee_rate,
    )?;

    // Terminalize the CHILD (it becomes an ancestor segment) and co-sign CSP under A_child.
    let child_sid = child_coin
        .statechain_id
        .clone()
        .ok_or_else(|| anyhow::anyhow!("child coin has no statechain_id"))?;

    // [P0-3] WRITE AHEAD, THEN TERMINALIZE — identical contract to `in_ladder_split`; this lane has
    // the same defect and now shares the same journal. The record is complete before the child's
    // budget is consumed, so a crash anywhere below is replayed rather than lost.
    let mut journal = SplitJournalRecord {
        op_id: format!("child_in_ladder_split:{child_sid}:{}", csp.txid),
        lane: "child_in_ladder_split".to_string(),
        stage: SplitStage::Planned,
        terminalized_statechain_id: child_sid.clone(),
        parent: cb.parent.clone(),
        parent_statechain_id: cb.parent_statechain_id.clone(),
        // Filled in once CSP is co-signed (the child's own segment carries CSP as its state).
        ancestors: cb.ancestors.clone(),
        parent_flat_backups: cb.parent_flat_backups.clone(),
        children: children
            .iter()
            .enumerate()
            .map(|(j, (c, recipient, value))| {
                Ok(SplitJournalChild {
                    statechain_id: c
                        .statechain_id
                        .clone()
                        .ok_or_else(|| anyhow::anyhow!("grandchild coin has no statechain_id"))?,
                    owner_exit_address: recipient.clone(),
                    value: *value,
                    sp_vout: csp.payload_vout + j as u32,
                    extension: None,
                    state: None,
                    // The PLAIN child lane — see the root lane's note.
                    rgb: None,
                    pending_extension: None,
                    pending_state: None,
                    role: SplitLegRole::Piece,
                })
            })
            .collect::<Result<Vec<_>>>()?,
        child_ext_csv: p.ext_csv(0),
        child_state_csv: p.state_csv(0),
        fee_rate: cb.parent.fee_rate,
        network: cb.parent.network.clone(),
        sp_txid: csp.txid.clone(),
    };
    journal_write(cc, wallet_name, &journal).await?;

    crate::lightning_latch::set_spend_budget(cc, wallet_name, &child_sid, 1).await?;
    crash_point("after_inladder_terminalize");
    let csp_signed = cosign_tier(
        cc,
        child_coin,
        csp.tx_hex.clone(),
        cb.child_extension.out_value,
        &cb.parent.network,
    )
    .await?;

    // The child segment as the grandchildren will see it: CSP is its current state, and the state CSP
    // replaced is disclosed as superseded (it loses the race for ext_child.out[0]).
    let mut seg_superseded = cb.child_superseded_states.clone();
    seg_superseded.push(cb.child_state.clone());
    let child_segment = ChildSegment {
        statechain_id: child_sid.clone(),
        funding_vout: cb.sp_vout,
        // A received PIECE is strictly two-tier — `ext_child` then `CSP` — so this segment is
        // `Some`. `None` is reserved for the sender's own spine tip, which is never conveyed and
        // never reaches this lane (see `ChildSegment::extension`).
        extension: Some(cb.child_extension.clone()),
        state: TesrTier {
            txid: csp.txid.clone(),
            signed_tx: csp_signed,
            out_value: total,
            csv: Some(csp_csv),
            payload_vout: csp.payload_vout,
        },
        superseded_states: seg_superseded,
        superseded_extensions: cb.child_superseded_extensions.clone(),
    };
    // CSP is the unregenerable co-signature on this lane: record the completed ancestor segment
    // (which carries it) before touching a grandchild.
    journal.ancestors.push(child_segment.clone());
    journal.stage = SplitStage::Signed;
    journal_write(cc, wallet_name, &journal).await?;
    crash_point("after_inladder_sp_sign");

    let mut bundles = Vec::with_capacity(n);
    for (j, (gc_coin, recipient, value)) in children.iter_mut().enumerate() {
        let gc_vout = journal.children[j].sp_vout;
        debug_assert_eq!(*value, journal.children[j].value); // journalled above, and read from the record
        let ladder = establish_child_journalled(cc, wallet_name, gc_coin, &mut journal, j).await?;
        let gc_sid = gc_coin
            .statechain_id
            .clone()
            .ok_or_else(|| anyhow::anyhow!("grandchild coin has no statechain_id"))?;
        // Grandchildren are left NON-terminal for the same reason children are: the receiver completes
        // the key handover and takes first-class ownership (see in_ladder_split's [F1]).
        let mut ancestors = cb.ancestors.clone();
        ancestors.push(child_segment.clone());
        bundles.push(ChildTesrBundle {
            parent: cb.parent.clone(),
            parent_statechain_id: cb.parent_statechain_id.clone(),
            sp_vout: gc_vout,
            child_statechain_id: gc_sid,
            child_owner_exit_address: recipient.clone(),
            child_extension: ladder.extension,
            child_state: ladder.state,
            child_superseded_states: vec![],
            child_superseded_extensions: vec![],
            ancestors,
            // Child-level PLAIN split. `child_in_ladder_split` refuses a COLOURED child outright
            // (see its `refuse_uncolored_over_colored_child` guard), so this is never an allocation.
            rgb: None,
            // The ancestor PARENT segment is unchanged by a child-level split, so its conveyed flat
            // backup chain travels forward verbatim — the grandchild's receiver censuses the very
            // same parent this child's receiver already censused.
            parent_flat_backups: cb.parent_flat_backups.clone(),
        });
    }
    journal.stage = SplitStage::Established;
    journal_write(cc, wallet_name, &journal).await?;
    Ok(bundles)
}

/// ONWARD HOP — re-transfer a whole received CHILD off-chain to a new owner (Spark parity).
///
/// A received child cannot go through `transfer_sender::execute`: it has no `tesr-` bundle (only
/// `ctesr-`), so that path would fall through to the B1-unsafe plain split, and it has no un-laddered
/// backup chain to hand over. This is the child's own Model-A transfer instead:
///
///   * build a NEW child state over `ext_child.out[0]` one δ lower (replace-by-lower-timelock), paying
///     the NEW recipient's exit key — so the fresh state matures BEFORE the one it replaces and wins
///     the race for that outpoint;
///   * co-sign it under `A_child` — possible only because the receiver completed the key handover when
///     it adopted the child (Commit A); the sender of THIS hop is the current owner;
///   * disclose the state it replaced in `child_superseded_states` (full-disclosure counting), which the
///     receiver's census then counts and proves non-confirmable;
///   * convey the updated bundle with the standard handover, exactly like the first hop.
///
/// Net effect per hop: EXACTLY +1 `num_sigs` and +1 superseded entry, which is what
/// `verify_child_bundle`'s child census expects (`baseline + 2 + superseded`).
pub async fn child_retransfer(
    cc: &ClientConfig,
    wallet_name: &str,
    child_coin: &mut Coin,
    cb: &ChildTesrBundle,
    recipient_address: &str,
) -> Result<ChildTesrBundle> {
    refuse_uncolored_over_colored_child(cb, "child_retransfer")?;
    let p = cb.parent.params;
    // Replace-by-lower-timelock: the new state must mature strictly before the one it supersedes, and
    // must not sink below the schedule floor (at the floor the coin must be re-anchored, not re-sent).
    let old_csv = cb
        .child_state
        .csv
        .ok_or_else(|| anyhow::anyhow!("child state has no CSV — cannot re-transfer"))?;
    let new_csv = old_csv
        .checked_sub(p.delta)
        .filter(|c| *c >= p.d_floor)
        .ok_or_else(|| anyhow::anyhow!(
            "child state CSV {old_csv} is at the floor ({}) — exit or re-anchor it instead of re-sending",
            p.d_floor
        ))?;

    // The new state spends the SAME outpoint as the one it replaces: ext_child.out[0].
    let payee = mercurylib::tesr::payee_address(recipient_address, &cb.parent.network)?;
    let st = mercurylib::tesr::build_state_from(
        &cb.child_extension.txid,
        cb.child_extension.payload_vout,
        cb.child_extension.out_value,
        &payee,
        &cb.parent.network,
        new_csv,
        cb.parent.fee_rate,
    )?;
    // [D1 / A2] ARM DOWN DURABLY BEFORE THE SUPERSEDING STATE EXISTS — see the note on
    // `cosign_colored_child_retransfer`, which carries the full argument. Same window, same filter,
    // same remedy; only the stakes differ (sats rather than an allocation).
    let child_sid = cb.child_statechain_id.clone();
    let status_before_conveyance = crate::transfer_sender::persist_coin_status(
        cc,
        wallet_name,
        &child_sid,
        mercurylib::wallet::CoinStatus::IN_TRANSFER,
    )
    .await
    .map_err(|e| {
        anyhow::anyhow!(
            "refusing to re-transfer child {child_sid}: its coin could not be durably marked \
             IN_TRANSFER before the co-sign ({e}). Proceeding would leave this wallet's watchtower \
             driving the state about to be superseded, which rivals the recipient's S'_child over \
             ext_child's payload output. Nothing has been co-signed and the child is unchanged."
        )
    })?;

    let staged = async {
    let signed = cosign_tier(
        cc,
        child_coin,
        st.tx_hex.clone(),
        cb.child_extension.out_value,
        &cb.parent.network,
    )
    .await?;

    let mut next = cb.clone();
    // Full disclosure: the state we just replaced is now superseded (it loses the race for
    // ext_child.out[0] to the lower-CSV state above) and must be counted by the receiver's census.
    next.child_superseded_states.push(next.child_state.clone());
    next.child_state = TesrTier {
        txid: st.txid.clone(),
        signed_tx: signed,
        out_value: st.out_value,
        csv: Some(new_csv),
        payload_vout: st.payload_vout,
    };
    next.child_owner_exit_address = payee;

    // [D1] STORE BEFORE CONVEYING — see the note on `cosign_colored_child_retransfer`, which carries
    // the full argument. The shape is identical here and the ordering rule is the same; only the
    // stakes differ (a plain child's superseded state races the recipient's sats rather than an RGB
    // allocation, so the loss is a double-spend of the piece rather than a burn).
    persist_child(cc, wallet_name, &next).await.map_err(|e| {
        anyhow::anyhow!(
            "child {} is re-transferred and its replacement state S'_child is co-signed, but the \
             superseding bundle could not be stored ({e}). Refusing to convey it: this wallet's \
             `ctesr-` row would go on naming the SUPERSEDED state live, and `defend_ladders` drives \
             that row — it would race the recipient over ext_child's payload output instead of \
             defending them. Nothing has been conveyed, so retry the store and re-send.",
            cb.child_statechain_id
        )
    })?;
    Ok::<_, anyhow::Error>(next)
    }
    .await;

    // Nothing conveyed yet — restore, exactly as the coloured lane does.
    let next = match staged {
        Ok(n) => n,
        Err(e) => {
            return Err(
                match crate::transfer_sender::persist_coin_status(
                    cc,
                    wallet_name,
                    &child_sid,
                    status_before_conveyance,
                )
                .await
                {
                    Ok(_) => e,
                    Err(restore_err) => anyhow::anyhow!(
                        "{e}\n\nAND the child's status could not be restored afterwards \
                         ({restore_err}): child {child_sid} is left marked IN_TRANSFER even though \
                         nothing was conveyed, so `defend_ladders` will not drive its chain — \
                         repair the status or exit the child."
                    ),
                },
            );
        }
    };

    // A conveyance failure now leaves the superseding bundle ON DISK with nothing sent. That is the
    // safe side of the trade — the alternative ordering hands out rival material against a tower
    // that is still armed — and it is RECOVERABLE, not a loss: the child is still ours (IN_TRANSFER,
    // so the tower stands down rather than driving a state nobody was given), and re-running the
    // re-transfer supersedes `S'_child` in turn with a state one further δ down, paying whoever is
    // named then. Say so, because a bare transport error here reads like "nothing happened".
    convey_child_bundle(cc, recipient_address, child_coin, &next, None)
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "child {child_sid}'s replacement state S'_child is co-signed and stored but could \
                 not be conveyed ({e}). The child has NOT been given away and is not lost: it is \
                 marked IN_TRANSFER, so this wallet's watchtower stands down instead of driving a \
                 state no one holds. Re-run the re-transfer to supersede S'_child with a state one \
                 further delta down, paying whoever you name then; the CSV floor is the only budget \
                 this consumes."
            )
        })?;
    Ok(next)
}

/// A COLOURED replacement child STATE, built but NOT co-signed — the new-recipient-paying
/// `S'_child` of a coloured child re-transfer.
#[derive(Clone, Debug)]
pub struct ColoredChildStateDraft {
    pub child_statechain_id: String,
    /// The plain address this state pays, already resolved from the recipient's transfer address.
    pub payee: String,
    /// Strictly LOWER than the state it replaces — the race rule, and (via the seal rung) what keeps
    /// the two rival transitions over `ext_child`'s payload output apart.
    pub csv: u16,
    /// `ext_child`'s payload output, for the co-signer's fail-closed recheck.
    pub parent_txid: String,
    pub parent_vout: u32,
    pub parent_value: u64,
    /// The child's whole allocation — a re-transfer MOVES it, it never splits it.
    pub rgb_amount: u64,
    pub tier: ColoredTierDraft,
}

/// **[CTES-R] Colour the new owner-paying state of a COLOURED child re-transfer.** Engine-only,
/// synchronous, co-signs nothing — the `!Sync`-resolver rule every coloured builder here follows.
///
/// This is what [`child_retransfer`] cannot do and [`refuse_uncolored_over_colored_child`] therefore
/// refuses: `ext_child`'s payload output is a SEALED output, so replacing the child's state with a
/// plain tier would spend it with an RGB-unaware transaction and burn the allocation. The
/// replacement carries a real transition assigning the child's whole allocation to the new owner.
///
/// `S'_child` is a RIVAL of the child's current state over the SAME outpoint. They are separated by
/// the seal rung, which folds in the CSV — and the new CSV is strictly lower by construction
/// (`cur − δ`), so the seals cannot collide (equal rung ⟹ equal blinding ⟹ one `OpId` and a hash
/// lottery, `docs/utexo/CTESR-GATE.md` §2.2).
pub fn build_colored_child_retransfer(
    rgb: &mercury_rgb::RgbWallet,
    cb: &ChildTesrBundle,
    recipient_address: &str,
) -> Result<ColoredChildStateDraft> {
    use crate::rgb::{build_colored_tier, colored_tier_out_value, ColoredTierSpec, TierRole};

    if !cb.is_colored() {
        return Err(anyhow::anyhow!(
            "build_colored_child_retransfer: this child is PLAIN — use child_retransfer"
        ));
    }
    // Depth-1 + single-level parent + a derivable seal schedule, all in one.
    let _ = cb.colored_child_seals()?;
    let rgb_half = cb.rgb.as_ref().expect("is_colored");
    let p = cb.parent.params;
    let old_csv = cb
        .child_state
        .csv
        .ok_or_else(|| anyhow::anyhow!("child state has no CSV — cannot re-transfer"))?;
    let new_csv = old_csv
        .checked_sub(p.delta)
        .filter(|c| *c >= p.d_floor)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "child state CSV {old_csv} is at the floor ({}) — exit or re-anchor it instead of \
                 re-sending",
                p.d_floor
            )
        })?;

    let (parent_value, parent_spk) =
        tier_payload_prevout(&cb.child_extension, "coloured child re-transfer parent")?;
    let s_value = colored_tier_out_value(parent_value, cb.parent.fee_rate).ok_or_else(|| {
        anyhow::anyhow!(
            "the child extension's payload output ({parent_value} sat) cannot carry another \
             coloured state at {} sat/vB",
            cb.parent.fee_rate
        )
    })?;
    let payee = mercurylib::tesr::payee_address(recipient_address, &cb.parent.network)?;
    let seal = colored_tier_seal(
        &cb.child_statechain_id,
        TierRole::ChildState,
        0,
        0,
        Some(new_csv),
    );
    let tier = build_colored_tier(
        rgb,
        &ColoredTierSpec {
            contract_id: &rgb_half.contract_id,
            prev_txid: &cb.child_extension.txid,
            prev_vout: cb.child_extension.payload_vout,
            prev_value: parent_value,
            prev_spk_hex: &parent_spk,
            sequence: mercurylib::tesr::csv_blocks(new_csv).0,
            payloads: &[(payee.clone(), s_value, rgb_half.amount)],
            network: &cb.parent.network,
            fee_rate: cb.parent.fee_rate,
            nonce: Some(seal.rung as u64),
        },
        &seal,
    )?;

    Ok(ColoredChildStateDraft {
        child_statechain_id: cb.child_statechain_id.clone(),
        payee,
        csv: new_csv,
        parent_txid: cb.child_extension.txid.clone(),
        parent_vout: cb.child_extension.payload_vout,
        parent_value,
        rgb_amount: rgb_half.amount,
        tier: ColoredTierDraft {
            tx_hex: tier.tx_hex,
            txid: tier.txid,
            payload_vout: tier.payloads[0].vout,
            payload_value: tier.payloads[0].value,
            payload_spk_hex: tier.payloads[0].script_pubkey_hex.clone(),
            consignment: tier.consignment,
        },
    })
}

/// **[CTES-R] Co-sign, convey and book a COLOURED child re-transfer.** The coloured sibling of
/// [`child_retransfer`], and byte for byte the same census arithmetic: exactly one `cosign_tier`
/// round-trip and exactly one new `child_superseded_states` entry, which is what
/// `verify_child_bundle`'s child census (`baseline + 2 + superseded`) expects. Colouring adds no
/// co-sign, so the SE never learns anything changed.
///
/// Everything the draft asserts is RE-CHECKED against the live bundle and the recipient the caller
/// actually named, because the draft is built where the RGB engine lives and consumed where the
/// network lives.
pub async fn cosign_colored_child_retransfer(
    cc: &ClientConfig,
    wallet_name: &str,
    child_coin: &mut Coin,
    cb: &ChildTesrBundle,
    draft: ColoredChildStateDraft,
    recipient_address: &str,
) -> Result<ChildTesrBundle> {
    let rgb_half = cb
        .rgb
        .clone()
        .ok_or_else(|| anyhow::anyhow!("cosign_colored_child_retransfer on a PLAIN child"))?;
    if draft.child_statechain_id != cb.child_statechain_id {
        return Err(anyhow::anyhow!(
            "coloured child state draft is for {} but the bundle is {}",
            draft.child_statechain_id,
            cb.child_statechain_id
        ));
    }
    if draft.rgb_amount != rgb_half.amount {
        return Err(anyhow::anyhow!(
            "coloured child re-transfer would move {} but the child carries {} — a re-transfer \
             moves the whole allocation, it never mints or burns part of one",
            draft.rgb_amount,
            rgb_half.amount
        ));
    }
    let want_payee = mercurylib::tesr::payee_address(recipient_address, &cb.parent.network)?;
    if draft.payee != want_payee {
        return Err(anyhow::anyhow!(
            "coloured child state draft pays {} but this transfer is to {want_payee} — refusing",
            draft.payee
        ));
    }
    if draft.parent_txid != cb.child_extension.txid
        || draft.parent_vout != cb.child_extension.payload_vout
    {
        return Err(anyhow::anyhow!(
            "coloured child state draft spends {}:{} but the child extension's payload output is \
             {}:{}",
            draft.parent_txid,
            draft.parent_vout,
            cb.child_extension.txid,
            cb.child_extension.payload_vout
        ));
    }
    let old_csv = cb
        .child_state
        .csv
        .ok_or_else(|| anyhow::anyhow!("child state has no CSV"))?;
    if draft.csv >= old_csv {
        return Err(anyhow::anyhow!(
            "coloured child state draft's CSV {} does not out-race the state it replaces ({old_csv})",
            draft.csv
        ));
    }
    if rgb_half.consignments.len() != 2 {
        return Err(anyhow::anyhow!(
            "coloured child carries {} consignments for its 2 own tiers — refusing to re-transfer a \
             bundle whose proofs are not indexed by exit order",
            rgb_half.consignments.len()
        ));
    }

    // ---- [D1 / A2] ARM THE WATCHTOWER DOWN, DURABLY, BEFORE THE SUPERSEDING STATE EXISTS. ------
    //
    // `defend_ladders`' child loop drives `ctesr-<child>` for any child whose coin this wallet still
    // holds CONFIRMED, and `transfer_colored_child` does not mark the coin WITHDRAWN until AFTER
    // this function returns. So for the whole duration of the co-sign and the conveyance the
    // liveness allowlist admits this child, and the only thing deciding what gets broadcast is the
    // row's content — which still names the state we are about to supersede. A pass landing in that
    // window broadcasts `state_child(us)`, which rivals `S'_child(them)` over `ext_child`'s payload
    // output and, on the coloured lane, burns the allocation being paid.
    //
    // Same remedy as the whole-coin lane in `transfer_sender::execute_ex`, for the same reason
    // (durable local evidence beats asking the coordinator: no network on a per-block broadcast
    // path, no outage-induced blindness, no trust in the counterparty's answer). Restored below if
    // we abort before conveying — nothing has escaped at that point, so a still-ours child must not
    // be left without a tower.
    let child_sid = cb.child_statechain_id.clone();
    let status_before_conveyance = crate::transfer_sender::persist_coin_status(
        cc,
        wallet_name,
        &child_sid,
        mercurylib::wallet::CoinStatus::IN_TRANSFER,
    )
    .await
    .map_err(|e| {
        anyhow::anyhow!(
            "refusing to re-transfer coloured child {child_sid}: its coin could not be durably \
             marked IN_TRANSFER before the co-sign ({e}). Proceeding would leave this wallet's \
             watchtower driving the state that is about to be superseded, which rivals the \
             recipient's S'_child over ext_child's payload output and destroys their allocation. \
             Nothing has been co-signed and the child is unchanged."
        )
    })?;

    let staged = async {
    let signed = cosign_tier(
        cc,
        child_coin,
        draft.tier.tx_hex.clone(),
        draft.parent_value,
        &cb.parent.network,
    )
    .await?;

    let mut next = cb.clone();
    // Full disclosure, exactly as the plain path: the state we just replaced was co-signed, so it
    // stays counted — and it sits one δ HIGHER, so it loses the race for ext_child's payload output.
    next.child_superseded_states.push(next.child_state.clone());
    next.child_state = TesrTier {
        txid: draft.tier.txid.clone(),
        signed_tx: signed,
        out_value: draft.tier.payload_value,
        csv: Some(draft.csv),
        payload_vout: draft.tier.payload_vout,
    };
    next.child_owner_exit_address = draft.payee;
    // The leaf consignment is REPLACED, not appended: `ColoredChild::consignments` is indexed by the
    // child's own exit order `[ext_child, state_child]`.
    let mut consignments = rgb_half.consignments.clone();
    consignments[1] = draft.tier.consignment;
    next.rgb = Some(ColoredChild { consignments, ..rgb_half });

    // ---- [D1] STORE THE SUPERSEDING BUNDLE **BEFORE** CONVEYING IT. --------------------------
    //
    // This call used to convey first and store second. Between the two, the recipient held a
    // co-signed `S'_child` while this wallet's `ctesr-<child>` row still named the state it
    // REPLACES as live — and `defend_ladders`' child loop drives exactly that row. The two states
    // spend the same outpoint (`ext_child`'s payload output), and on the coloured lane the
    // recipient's whole allocation lives on `S'_child`, so a watchtower pass landing in that window
    // would not defend anything: it would race the recipient we just paid and BURN their asset.
    //
    // The sender's coin is not what closes this. `transfer_colored_child` leaves the child coin
    // CONFIRMED until it returns, so the liveness allowlist (L1) ADMITS the row for the whole
    // duration of the conveyance — by design, because a child re-transfer has no on-chain step and
    // the coin is genuinely still ours until the handover completes. What decides the outcome is
    // therefore the row's CONTENT, and storing first makes the sender's tower an ALLY of the
    // recipient rather than a rival: it now drives `T -> X_m -> SP -> ext_child -> S'_child`, which
    // is precisely the chain the recipient needs underneath them.
    //
    // A failure here is FATAL and conveys nothing. That is the safe direction: continuing would
    // hand out an allocation this wallet is armed to destroy — a third party's loss rather than our
    // own — whereas aborting leaves the child exactly where it was, still ours, still exitable from
    // the bundle already on disk. `S'_child` is co-signed and unrecoverable, but it is also
    // unreachable by anyone else, so the only cost of the abort is one wasted SE signature.
    persist_child(cc, wallet_name, &next).await.map_err(|e| {
        anyhow::anyhow!(
            "coloured child {} is re-transferred and S'_child is co-signed, but the superseding \
             bundle could not be stored ({e}). Refusing to convey it: this wallet's `ctesr-` row \
             would go on naming the SUPERSEDED state live, and `defend_ladders` drives that row — \
             broadcasting it would race the recipient over ext_child's payload output and DESTROY \
             the allocation being paid. Nothing has been conveyed and the child is unchanged; \
             retry the store, then re-send.",
            cb.child_statechain_id
        )
    })?;
    Ok::<_, anyhow::Error>(next)
    }
    .await;

    // Nothing has been conveyed yet, so an abort leaves the child wholly ours: put its status back
    // rather than stranding a live child without a watchtower. A failed restore is surfaced, never
    // swallowed — it is the only way the owner learns the child is undefended.
    let next = match staged {
        Ok(n) => n,
        Err(e) => {
            return Err(
                match crate::transfer_sender::persist_coin_status(
                    cc,
                    wallet_name,
                    &child_sid,
                    status_before_conveyance,
                )
                .await
                {
                    Ok(_) => e,
                    Err(restore_err) => anyhow::anyhow!(
                        "{e}\n\nAND the child's status could not be restored afterwards \
                         ({restore_err}): child {child_sid} is left marked IN_TRANSFER even though \
                         nothing was conveyed, so `defend_ladders` will not drive its chain. \
                         Nothing was given away, but the child is UNDEFENDED until the status is \
                         repaired — restore it, or exit the child."
                    ),
                },
            );
        }
    };

    // A conveyance failure now leaves the superseding bundle ON DISK with nothing sent. That is the
    // safe side of the trade — the alternative ordering hands out rival material against a tower
    // that is still armed — and it is RECOVERABLE, not a loss: the child is still ours (IN_TRANSFER,
    // so the tower stands down rather than driving a state nobody was given), and re-running the
    // re-transfer supersedes `S'_child` in turn with a state one further δ down, paying whoever is
    // named then. Say so, because a bare transport error here reads like "nothing happened".
    convey_child_bundle(cc, recipient_address, child_coin, &next, None)
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "child {child_sid}'s replacement state S'_child is co-signed and stored but could \
                 not be conveyed ({e}). The child has NOT been given away and is not lost: it is \
                 marked IN_TRANSFER, so this wallet's watchtower stands down instead of driving a \
                 state no one holds. Re-run the re-transfer to supersede S'_child with a state one \
                 further delta down, paying whoever you name then; the CSV floor is the only budget \
                 this consumes."
            )
        })?;
    Ok(next)
}

/// Conveys a split **child** bundle to `recipient_address` by posting an encrypted mailbox message
/// (`child_tesr_bundle` set, `protocol_version = 4`) via `transfer/update_msg`, together with the
/// STANDARD key-handover material (`transfer_signature` + blinded `t1`). The `child_coin` is the
/// sender-owned piece slot whose `signed_statechain_id` authorises the post. The receiver picks it up
/// in claim(), runs [`verify_child_bundle`] and then COMPLETES the handover, so the child becomes a
/// first-class coin and the sender is locked out (`docs/utexo/CHILDREN.md`).
pub async fn convey_child_bundle(
    cc: &ClientConfig,
    recipient_address: &str,
    child_coin: &Coin,
    cb: &ChildTesrBundle,
    batch_id: Option<String>,
) -> Result<()> {
    let statechain_id = child_coin
        .statechain_id
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("piece child coin has no statechain_id"))?;
    let signed_statechain_id = child_coin
        .signed_statechain_id
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("piece child coin has no signed_statechain_id"))?;
    let (_, _, recipient_auth_pubkey) =
        mercurylib::decode_transfer_address(recipient_address)?;
    // The bundle must describe the very slot we are conveying, or the receiver would census one coin
    // and complete the handover on another.
    if cb.child_statechain_id != *statechain_id {
        return Err(anyhow::anyhow!(
            "child bundle statechain id {} does not match the conveyed slot {statechain_id}",
            cb.child_statechain_id
        ));
    }

    // The child's funding outpoint is `SP.out[sp_vout]` of the UN-BROADCAST split state — the child
    // slot's own `utxo_txid`/`utxo_vout` are None (a derived slot is never funded on-chain), so derive
    // it from the bundle. This is the outpoint the transfer signature commits to, and the same one the
    // receiver reconstructs when it validates.
    let sp_txid = {
        use bitcoin::consensus::encode::deserialize;
        // The child's funding tx is the segment DIRECTLY above it: the parent's SP for a depth-1
        // child, else the deepest intermediate segment's state. This must match exactly what the
        // receiver reconstructs when it verifies the transfer signature.
        let funding_hex = cb
            .ancestors
            .last()
            .map(|a| a.state.signed_tx.clone())
            .unwrap_or_else(|| cb.parent.current().state.signed_tx.clone());
        let sp: bitcoin::Transaction = deserialize(&hex::decode(&funding_hex)?)?;
        sp.txid().to_string()
    };

    // Sender-side binding, built BEFORE the transfer is opened: once `get_new_x1` runs, the coordinator's
    // pending-transfer lock refuses further co-signs on this slot, so every sender-side signature must
    // already exist (the same discipline `transfer_sender::execute` follows).
    let transfer_signature = mercurylib::transfer::sender::create_transfer_signature(
        recipient_address,
        &sp_txid,
        cb.sp_vout,
        &child_coin.user_privkey,
    )?;

    // `transfer/update_msg` is an UPDATE keyed on (statechain_id, new_user_auth_key); the row is
    // created by this `transfer/sender` init call — without it the update_msg silently no-ops (0 rows)
    // and the receiver never sees the message. It also returns `x1`, the SE's blinding factor for the
    // key handover, and ARMS the coordinator's pending-transfer lock on this slot (which is what closes
    // the post-conveyance rival window for the child).
    //
    // [non-exact LN latch, LIGHTNING.md Step 1] `batch_id = Some` makes the child mailbox row born
    // batch-locked: `insert_new_transfer` sets locked=true and locked2=is_lightning_latch, so the
    // receiver (SSP) must not adopt the piece until the LN preimage flips locked2 via
    // `unlock_by_preimage`. The piece sid must already carry an external-hash latch (registered by the
    // latched `in_ladder_pay`) for the server to mark the row a lightning latch.
    let x1 = crate::transfer_sender::get_new_x1(
        cc,
        statechain_id,
        signed_statechain_id,
        &recipient_auth_pubkey.to_string(),
        batch_id,
    )
    .await?;

    let json = serde_json::to_string(cb)?;
    let payload = mercurylib::transfer::sender::create_child_conveyance_update_msg(
        &x1,
        recipient_address,
        child_coin,
        &transfer_signature,
        &json,
    )?;
    let endpoint = cc.statechain_entity.clone();
    let client = cc.get_reqwest_client()?;
    let status = client
        .post(&format!("{}/transfer/update_msg", endpoint))
        .json(&payload)
        .send()
        .await?
        .status();
    if !status.is_success() {
        return Err(anyhow::anyhow!("failed to convey child bundle (update_msg {status})"));
    }
    Ok(())
}

/// Off-chain RENEWAL: co-sign a new extension `X_{m+1}` with a strictly LOWER CSV
/// (Decker-Wattenhofer replace-by-lower-timelock) plus a fresh state `S'_0`, replacing the current
/// tiers in `bundle`. Zero on-chain bytes; once the trigger is broadcast, the superseded extension
/// can never win the race for `T.out[0]`. Persist the updated bundle afterwards.
pub async fn renew(
    cc: &ClientConfig,
    coin: &mut Coin,
    bundle: &mut TesrBundle,
    csv_e_new: u16,
    csv_d: u16,
) -> Result<()> {
    refuse_uncolored_over_colored(bundle, "renew")?;
    let (parent_txid, parent_val) = bundle.current_parent();
    let x = mercurylib::tesr::build_extension(&parent_txid, parent_val, &bundle.agg_address, &bundle.network, csv_e_new, bundle.fee_rate)?;
    let x_signed = cosign_tier(cc, coin, x.tx_hex.clone(), parent_val, &bundle.network).await?;
    let s = mercurylib::tesr::build_state(&x.txid, x.out_value, &bundle.owner_exit_address, &bundle.network, csv_d, bundle.fee_rate)?;
    let s_signed = cosign_tier(cc, coin, s.tx_hex.clone(), x.out_value, &bundle.network).await?;

    let last = bundle.levels.len() - 1;
    // Full-disclosure: the replaced extension + state were co-signed; keep them so verify_bundle counts them.
    bundle.superseded_extensions.push(bundle.levels[last].extension.clone());
    bundle.superseded_states.push(bundle.levels[last].state.clone());
    bundle.levels[last] = TesrLevel {
        extension: TesrTier { txid: x.txid, signed_tx: x_signed, out_value: x.out_value, csv: Some(csv_e_new), payload_vout: x.payload_vout },
        state: TesrTier { txid: s.txid, signed_tx: s_signed, out_value: s.out_value, csv: Some(csv_d), payload_vout: s.payload_vout },
    };
    bundle.m += 1;
    Ok(())
}

/// Off-chain ROLLOVER at epoch exhaustion: instead of an on-chain compaction, convert the current
/// level's state into a SELF-SPLIT paying `A` and hang a FRESH level (extension + owner state) off it
/// — a new renewal budget, unbounded off-chain depth, ZERO on-chain bytes. Persist afterwards. The
/// only cost is +2 pre-signed txs (~248 vB) of *contingent* exit weight and +1 unilateral-exit level.
pub async fn rollover(
    cc: &ClientConfig,
    coin: &mut Coin,
    bundle: &mut TesrBundle,
    csv_e: u16,
    csv_d: u16,
) -> Result<()> {
    refuse_uncolored_over_colored(bundle, "rollover")?;
    let p = bundle.params;
    let cur_ext = bundle.current().extension.clone();
    let cur_state_csv = bundle
        .current()
        .state
        .csv
        .ok_or_else(|| anyhow::anyhow!("current state has no CSV"))?;

    // 1. Re-sign the current level's state as a self-split paying A (so a child level can hang off it).
    //
    // [S3] The self-split spends the SAME prevout as the current owner-paying state (both spend X_L:0),
    // so it is a RIVAL of that state, not an independent tier. It can only supersede it by maturing
    // FIRST — i.e. at a strictly LOWER CSV — exactly as presign_receiver_state does for S'. Building it
    // at the current (undecremented) CSV made it an EQUAL-CSV twin of the retained old state, which
    // verify_bundle's per-prevout race check rejects (`(csv as u32) <= live_csv`), so every rolled-over
    // coin failed verification. It went unnoticed because sdk43 never called verify_bundle. Since
    // rollover is mandatory at m_max, that was the terminal state of every long-lived coin.
    //
    // Consequence: rollover now CONSUMES one state rung (see PROTOCOL.md footprint note). If the state
    // CSV is already at the floor, a self-split rollover is impossible and the coin must exit or
    // on-chain re-anchor.
    let roll_csv = cur_state_csv
        .checked_sub(p.delta)
        .filter(|c| *c >= p.d_floor)
        .ok_or_else(|| anyhow::anyhow!(
            "state CSV at the floor — cannot self-split rollover; exit or on-chain re-anchor"
        ))?;
    let s_roll = mercurylib::tesr::build_state(&cur_ext.txid, cur_ext.out_value, &bundle.agg_address, &bundle.network, roll_csv, bundle.fee_rate)?;
    let s_roll_signed = cosign_tier(cc, coin, s_roll.tx_hex.clone(), cur_ext.out_value, &bundle.network).await?;

    // 2. Fresh level off the self-split output: new extension + owner-exit state.
    let x2 = mercurylib::tesr::build_extension(&s_roll.txid, s_roll.out_value, &bundle.agg_address, &bundle.network, csv_e, bundle.fee_rate)?;
    let x2_signed = cosign_tier(cc, coin, x2.tx_hex.clone(), s_roll.out_value, &bundle.network).await?;
    let s2 = mercurylib::tesr::build_state(&x2.txid, x2.out_value, &bundle.owner_exit_address, &bundle.network, csv_d, bundle.fee_rate)?;
    let s2_signed = cosign_tier(cc, coin, s2.tx_hex.clone(), x2.out_value, &bundle.network).await?;

    let last = bundle.levels.len() - 1;
    // Full-disclosure: the old owner-paying state is replaced by the self-split; keep it for counting.
    bundle.superseded_states.push(bundle.levels[last].state.clone());
    bundle.levels[last].state = TesrTier { txid: s_roll.txid, signed_tx: s_roll_signed, out_value: s_roll.out_value, csv: Some(roll_csv), payload_vout: s_roll.payload_vout };
    bundle.levels.push(TesrLevel {
        extension: TesrTier { txid: x2.txid, signed_tx: x2_signed, out_value: x2.out_value, csv: Some(csv_e), payload_vout: x2.payload_vout },
        state: TesrTier { txid: s2.txid, signed_tx: s2_signed, out_value: s2.out_value, csv: Some(csv_d), payload_vout: s2.payload_vout },
    });
    bundle.m = 0; // fresh renewal budget at the new level
    Ok(())
}

/// Model A (history/MIGRATION.md §"receiver ladder adoption"): while still owning the coin, pre-sign the
/// RECEIVER-paying state `S'` so the receiver gets a complete, verifiable exit chain paying THEM.
/// `S'` spends the deepest extension's output, pays the recipient's derived P2TR (from a Mercury
/// transfer address), and carries CSV `= current_state_csv − δ` (one lower, so it matures before the
/// sender's retained state). Returns the augmented bundle to convey (final state = `S'`,
/// `owner_exit_address` = the recipient's key). Co-signs on a CLONE so the caller's coin is untouched
/// for the rest of the transfer. Errors if the state CSV is at the floor (renew/rollover first).
pub async fn presign_receiver_state(
    cc: &ClientConfig,
    coin: &Coin,
    bundle: &TesrBundle,
    recipient_address: &str,
) -> Result<TesrBundle> {
    refuse_uncolored_over_colored(bundle, "presign_receiver_state")?;
    let p = bundle.params;
    let cur_csv = bundle.current().state.csv.ok_or_else(|| anyhow::anyhow!("current state has no CSV"))?;
    let new_csv = cur_csv
        .checked_sub(p.delta)
        .filter(|c| *c >= p.d_floor)
        .ok_or_else(|| anyhow::anyhow!("state CSV at the floor — renew/rollover before transferring"))?;

    let payee = mercurylib::tesr::payee_address(recipient_address, &bundle.network)?;
    let ext = bundle.current().extension.clone();
    let s = mercurylib::tesr::build_state(&ext.txid, ext.out_value, &payee, &bundle.network, new_csv, bundle.fee_rate)?;

    let mut c = coin.clone();
    let s_signed = cosign_tier(cc, &mut c, s.tx_hex.clone(), ext.out_value, &bundle.network).await?;

    let mut b = bundle.clone();
    b.owner_exit_address = payee;
    let last = b.levels.len() - 1;
    // Full-disclosure: the sender's own (now stale) state was co-signed; keep it so verify_bundle
    // counts it — and it sits at a HIGHER CSV than S', so it loses the maturity race to the receiver.
    b.superseded_states.push(b.levels[last].state.clone());
    b.levels[last].state = TesrTier { txid: s.txid, signed_tx: s_signed, out_value: s.out_value, csv: Some(new_csv), payload_vout: s.payload_vout };
    Ok(b)
}

/// Cooperative DE-TRIGGER: blind-co-sign a fresh no-timelock spend of `T.out[0]` paying `to_address`,
/// returning the signed tx. Broadcasting it in response to a hostile trigger confirms before any
/// pre-signed extension can mature, collapsing griefing to a priced nuisance and killing the ladder.
pub async fn cosign_detrigger(
    cc: &ClientConfig,
    coin: &mut Coin,
    bundle: &TesrBundle,
    to_address: &str,
) -> Result<String> {
    // A coloured trigger carries the opret at index 0, so `build_detrigger`'s uncoloured
    // `UNCOLORED_PAYLOAD_VOUT` prevout would name an OP_RETURN — an unspendable, dead transaction
    // that nonetheless BURNS an irreversible SE co-sign and unbalances the census. Refuse.
    refuse_uncolored_over_colored(bundle, "cosign_detrigger")?;
    let de = mercurylib::tesr::build_detrigger(&bundle.trigger.txid, bundle.trigger.out_value, to_address, &bundle.network, bundle.fee_rate)?;
    cosign_tier(cc, coin, de.tx_hex.clone(), bundle.trigger.out_value, &bundle.network).await
}

/// **Chain-visibility vocabulary (external review F4).**
///
/// Every watch/exit pass in this module decides what to do by READING the chain through a backend
/// that can fail. A failed read is *not* a negative answer: if a tower cannot see whether `F` was
/// spent, "no action taken" means nothing at all, and reporting it as "nothing to do" is exactly
/// how a contested exit is lost — the tower looks idle while doing nothing, during the one window
/// where being blind is fatal.
///
/// So every backend call below returns `Ok` **only when the backend actually answered**, and every
/// caller turns a failure into a typed [`WatchState::Blind`] / an `Err`, never into an empty vector.
///
/// The one subtlety worth stating: an electrum server replying *"I have no such transaction"* IS an
/// answer, and a trustworthy one. [`is_missing_tx_error`] is what separates that answer from a
/// backend that did not answer, and it fails CLOSED — anything it does not positively recognise as
/// "absent" counts as blindness.
///
/// True iff `err` is the server's own, unambiguous "no such transaction". `Error::Protocol` is the
/// only variant `electrum_client` returns un-retried and un-wrapped for a JSON-RPC error object,
/// i.e. the only case where the server demonstrably reached us; every transport error, retry
/// exhaustion (`AllAttemptsErrored`), malformed response, or unrecognised server message is NOT a
/// negative answer and must surface as blindness.
fn is_missing_tx_error(err: &electrum_client::Error) -> bool {
    match err {
        electrum_client::Error::Protocol(v) => {
            let m = v.to_string().to_ascii_lowercase();
            [
                "no such mempool or blockchain transaction", // bitcoind/electrs verbatim
                "no such transaction",
                "transaction not found",
                "unknown transaction",
                "missing transaction",
            ]
            .iter()
            .any(|needle| m.contains(needle))
        }
        _ => false,
    }
}

/// `Ok(true)` iff `txid` is known to the chain backend (confirmed or in mempool), `Ok(false)` iff
/// the backend positively answered that it has no such transaction. `Err` = **blind**: the backend
/// could not be read, or `txid` is unusable material — either way the caller learned nothing and
/// must not treat this as "not on chain yet".
fn tx_known(electrum: &electrum_client::Client, txid: &str) -> Result<bool> {
    let t = electrum_client::bitcoin::Txid::from_str(txid)
        .map_err(|e| anyhow::anyhow!("unusable txid {txid:?} in stored exit material: {e}"))?;
    match electrum.transaction_get_raw(&t) {
        Ok(_) => Ok(true),
        Err(e) if is_missing_tx_error(&e) => Ok(false),
        Err(e) => Err(anyhow::anyhow!(
            "chain backend unreadable while looking up {txid} — cannot tell whether it is on chain: {e}"
        )),
    }
}

/// `Ok(true)` iff `txid:vout` is no longer unspent (its funding UTXO has been consumed), `Ok(false)`
/// iff it is still unspent — including the case where the backend positively answered that `txid`
/// itself is not on chain, which is a laddered SUB-coin's normal steady state (its `F` is an
/// un-broadcast split output): a transaction that does not exist has no spendable output, so it
/// cannot have been spent and there is genuinely nothing to defend.
///
/// `Err` = **blind**. Note also that the out-of-range `vout` that used to PANIC here is now a
/// named error.
/// Public because the keyless bundle tower in the SDK needs the SAME question answered the SAME
/// way. Note the two failure directions it deliberately distinguishes: a funding tx that is simply
/// **absent** is `Ok(false)` (an outpoint that does not exist cannot have been spent), while a
/// backend that could not answer is `Err` — which the caller must surface as blindness and never as
/// "unspent, nothing to do".
pub fn outpoint_spent(electrum: &electrum_client::Client, txid: &str, vout: u32) -> Result<bool> {
    let t = electrum_client::bitcoin::Txid::from_str(txid)
        .map_err(|e| anyhow::anyhow!("unusable funding txid {txid:?} in stored ladder: {e}"))?;
    let raw = match electrum.transaction_get_raw(&t) {
        Ok(r) => r,
        Err(e) if is_missing_tx_error(&e) => return Ok(false),
        Err(e) => {
            return Err(anyhow::anyhow!(
                "chain backend unreadable while fetching funding {txid} — cannot tell whether the \
                 coin has been triggered: {e}"
            ))
        }
    };
    let tx: electrum_client::bitcoin::Transaction =
        electrum_client::bitcoin::consensus::deserialize(&raw)
            .map_err(|e| anyhow::anyhow!("funding tx {txid} did not deserialize: {e}"))?;
    let out = tx
        .output
        .get(vout as usize)
        .ok_or_else(|| anyhow::anyhow!("funding {txid} has no output {vout} ({} outputs)", tx.output.len()))?;
    let listed = electrum.script_list_unspent(&out.script_pubkey).map_err(|e| {
        anyhow::anyhow!(
            "chain backend unreadable while listing unspent outputs of {txid}:{vout} — cannot tell \
             whether the coin has been triggered: {e}"
        )
    })?;
    Ok(!listed.iter().any(|u| u.tx_hash.to_string() == txid && u.tx_pos as u32 == vout))
}

/// **Typed outcome of one watchtower pass** — the single vocabulary shared by the laddered (TES-R)
/// tower here and the un-laddered deadline tower in the SDK (`mercury_utexo_sdk::watch_pass`).
///
/// It exists for one reason (external review F4): a caller MUST be able to tell
/// *"I looked, and there is nothing to do"* ([`Self::Idle`]) from *"I could not look"*
/// ([`Self::Blind`]). Both used to be an empty `Vec<String>`, so a dead electrum backend was
/// indistinguishable from a quiet chain — during precisely the race window where being blind loses
/// the coin. `Blind` is therefore never a degenerate case of the others: it means this pass
/// determined **nothing**, and an app that holds off-chain coins must treat it as an alert.
///
/// `#[must_use]`: dropping this value on the floor is the bug the type was introduced to prevent.
#[must_use = "a watch pass reports whether it could SEE; ignoring it re-introduces the blind-looks-idle bug"]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WatchState {
    /// The backend answered and there is genuinely nothing to do — for the TES-R tower, the coin's
    /// funding `F` is still unspent, so it has not been triggered and an idle laddered coin never
    /// ages. This is the steady state, and it is a POSITIVE observation.
    Idle,
    /// The backend answered and this pass is DEFENDING: the coin is triggered (TES-R tower) or an
    /// entry is inside its deadline margin (deadline tower).
    ///
    /// `ids` are what the pass acted on — tier **txids** for the TES-R tower, **statechain_ids** for
    /// the deadline tower — and may be EMPTY, meaning "already under way: every mature tier is out
    /// and the next one has not matured yet". `failures` carries the rejections that stopped it
    /// (typically `non-BIP68-final`, i.e. a CSV that has simply not matured), so a caller can tell a
    /// waiting exit from a RACED or dead one instead of retrying forever in silence.
    ///
    /// `blind` names the entries the pass could **not evaluate at all** — a per-entry sibling of
    /// [`Self::Blind`], which is whole-pass. A bundle-based tower watches many coins over one
    /// backend, so "I could not answer the question for coin X" must not be averaged away by the
    /// coins it could answer for: without this field an entry whose trigger was unreadable
    /// contributed nothing and the pass reported [`Self::Idle`] — the exact blind-looks-idle bug
    /// this enum exists to prevent, reintroduced one level down.
    ///
    /// Note the widened meaning of the variant itself: `Acted` is **"the pass was engaged"** —
    /// it broadcast something, tried and was rejected, or could not see an entry. It is everything
    /// that is not the positive all-quiet observation. `Idle` still means, exactly, *every* entry was
    /// evaluated and none was due.
    Acted { ids: Vec<String>, failures: Vec<String>, blind: Vec<String> },
    /// 🔴 The chain backend could not be read, or the stored exit material could not be used. This
    /// pass saw NOTHING and defended nothing. Never fold this into [`Self::Idle`].
    Blind { reason: String },
}

impl WatchState {
    /// The pass could not see. The caller must alert and retry — silence here is not safety.
    pub fn is_blind(&self) -> bool {
        matches!(self, Self::Blind { .. })
    }
    /// The pass looked and found nothing to do. False when blind, which is the whole point.
    pub fn is_idle(&self) -> bool {
        matches!(self, Self::Idle)
    }
    /// What the pass acted on (empty for [`Self::Idle`] and [`Self::Blind`] alike — so NEVER use
    /// emptiness to decide whether the pass worked; use [`Self::is_blind`]).
    pub fn ids(&self) -> &[String] {
        match self {
            Self::Acted { ids, .. } => ids,
            _ => &[],
        }
    }
    /// Broadcast rejections seen this pass (usually an immature CSV).
    pub fn failures(&self) -> &[String] {
        match self {
            Self::Acted { failures, .. } => failures,
            _ => &[],
        }
    }
    /// The blindness cause, if any.
    pub fn blind_reason(&self) -> Option<&str> {
        match self {
            Self::Blind { reason } => Some(reason),
            _ => None,
        }
    }
    /// Entries this pass could not evaluate (empty unless the tower watches a bundle of coins).
    pub fn blind_entries(&self) -> &[String] {
        match self {
            Self::Acted { blind, .. } => blind,
            _ => &[],
        }
    }
    /// **The alert predicate a bundle tower must use.** True when the pass saw nothing at all
    /// ([`Self::Blind`]) *or* when it could not evaluate at least one entry. `is_blind` alone is not
    /// enough for a multi-coin tower: it answers only for the pass, not for the coin that went
    /// unwatched inside a pass that otherwise succeeded.
    pub fn any_blindness(&self) -> bool {
        self.is_blind() || !self.blind_entries().is_empty()
    }
}

/// **Typed progress of one unilateral-exit pass.** Only ever produced when the chain was actually
/// readable — a backend failure is an `Err` from the pass, never a quiet "nothing happened"
/// (external review F4).
#[must_use = "an exit pass reports whether the exit is complete and why it stopped"]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExitProgress {
    /// Tier txids broadcast by THIS pass (empty = every mature tier was already out).
    pub broadcast: Vec<String>,
    /// The final exit state is on chain or in the mempool — the value is committed to the owner.
    pub complete: bool,
    /// Why the pass stopped short, if it did: the first broadcast rejection, normally an immature
    /// relative timelock. Retained so a DEAD or RACED exit is not reported forever as "just wait".
    pub stalled: Option<String>,
}

/// **TES-R WatchBundle (keyless watchtower).** One reactive pass: if the coin's funding UTXO `F` has
/// been spent — i.e. someone broadcast the trigger — drive the OWNER's unilateral exit by
/// broadcasting each pre-signed tier in order as its relative-timelock matures. Keyless: it holds
/// only the pre-signed [`TesrBundle`] (every tier pays the owner) and NEVER co-signs, so a delegated
/// tower can defend an offline owner without any key material. It needs ONLY an electrum
/// connection, exactly like the un-laddered tower. Idempotent — call once per new block from a tower
/// loop; already-confirmed tiers are skipped and a not-yet-mature tier just retries next pass.
///
/// Returns a [`WatchState`]: [`WatchState::Idle`] when `F` is verifiably unspent,
/// [`WatchState::Acted`] with the tier txids broadcast this pass, or [`WatchState::Blind`] when the
/// backend could not be read. **`Idle` and `Blind` are different answers** — before F4 both were an
/// empty vector, so a tower with a dead backend reported the same "all quiet" as a tower watching a
/// healthy idle coin.
pub fn watch_pass(electrum: &electrum_client::Client, bundle: &TesrBundle) -> WatchState {
    match watch_pass_seen(electrum, bundle) {
        Ok(state) => state,
        Err(e) => WatchState::Blind { reason: e.to_string() },
    }
}

/// [`watch_pass`]'s body, with every unreadable backend answer as an `Err` (which the caller turns
/// into [`WatchState::Blind`]).
fn watch_pass_seen(electrum: &electrum_client::Client, bundle: &TesrBundle) -> Result<WatchState> {
    // Defend only once the coin has actually been triggered on-chain — an idle un-broadcast coin
    // never ages, so there is nothing to do until F is spent. `?` is load-bearing: a backend that
    // cannot answer "is F spent?" must NOT be read as "F is unspent".
    if !outpoint_spent(electrum, &bundle.f_txid, bundle.f_vout)? {
        return Ok(WatchState::Idle);
    }
    let mut ids = Vec::new();
    let mut failures = Vec::new();
    for tier in bundle.exit_tiers() {
        if tx_known(electrum, &tier.txid)? {
            continue; // already on-chain / in mempool
        }
        let raw = hex::decode(&tier.signed_tx).map_err(|e| {
            anyhow::anyhow!("tier {} carries unusable signed tx hex: {e}", tier.txid)
        })?;
        match electrum.transaction_broadcast_raw(&raw) {
            Ok(_) => ids.push(tier.txid.clone()),
            Err(e) => {
                // CSV not met yet / parent unconfirmed — retry on the next pass, but SAY SO.
                failures.push(format!("{}: {e}", tier.txid));
                break;
            }
        }
    }
    Ok(WatchState::Acted { ids, failures, blind: vec![] })
}

/// **Tower pass for a split CHILD — the child-lane sibling of [`watch_pass`].**
///
/// A child's ladder is not rooted at its own funding output: `SP.out[sp_vout]` is un-broadcast, and
/// the whole chain hangs off the PARENT's on-chain `F`. So "has someone started a contested exit?"
/// is the same question for a child as for its parent — *is `F` spent?* — and the answer decides
/// whether this pass does anything at all.
///
/// It matters because the child's threat model is not the parent's. What a child has to survive is
/// the parent's retained `S_0`, which rivals the child's `SP` over `X_m.out[0]`. The child wins that
/// race by construction (`SP` carries a strictly lower CSV, enforced at adoption by
/// `verify_child_bundle`) — but only if somebody actually BROADCASTS the child's chain while the
/// race is on. Before this existed nothing did: [`watch_pass`] is driven from the `tesr-` rows and a
/// child has none, so an adopted child — including every COLOURED child, whose `SP.out[j]` carries
/// the RGB allocation — sat undefended through exactly the event it was designed to win.
///
/// Keyless and idempotent, on the same contract as [`watch_pass`]: every tx is already co-signed,
/// nothing here can co-sign, `Idle` and `Blind` are different answers, and a not-yet-mature tier
/// reports itself as a `failures` entry rather than as silence.
pub fn watch_child_pass(
    electrum: &electrum_client::Client,
    cb: &ChildTesrBundle,
) -> WatchState {
    match watch_child_pass_seen(electrum, cb) {
        Ok(state) => state,
        Err(e) => WatchState::Blind { reason: e.to_string() },
    }
}

fn watch_child_pass_seen(
    electrum: &electrum_client::Client,
    cb: &ChildTesrBundle,
) -> Result<WatchState> {
    use electrum_client::bitcoin::{consensus::deserialize, Transaction};
    // `?` is load-bearing, exactly as in `watch_pass_seen`: a backend that cannot answer "is F
    // spent?" must never be read as "F is unspent".
    if !outpoint_spent(electrum, &cb.parent.f_txid, cb.parent.f_vout)? {
        return Ok(WatchState::Idle);
    }
    let mut ids = Vec::new();
    let mut failures = Vec::new();
    for (signed, _csv) in child_exit_chain(cb) {
        let raw = hex::decode(&signed)
            .map_err(|e| anyhow::anyhow!("child exit chain carries unusable signed tx hex: {e}"))?;
        let txid = deserialize::<Transaction>(&raw)
            .map_err(|e| anyhow::anyhow!("child exit chain tx did not deserialize: {e}"))?
            .txid()
            .to_string();
        if tx_known(electrum, &txid)? {
            continue; // already on-chain / in mempool (often the racer's own T or X_m)
        }
        match electrum.transaction_broadcast_raw(&raw) {
            Ok(_) => ids.push(txid),
            Err(e) => {
                failures.push(format!("{txid}: {e}"));
                break;
            }
        }
    }
    Ok(WatchState::Acted { ids, failures, blind: vec![] })
}

/// **Owner-initiated unilateral exit of a laddered coin.** Like [`watch_pass`], but this KICKS OFF the exit
/// by spending `F` with the trigger — a tower defends an already-triggered coin and never initiates,
/// whereas an owner walking away must start the clock. Broadcasts the trigger (if `F` is still unspent)
/// and then every subsequent tier whose relative-CSV is now met, in exit order, stopping at the first
/// not-yet-mature tier. Idempotent and incremental: call once per block (already-confirmed/known tiers
/// are skipped). Returns `(txids_broadcast_this_pass, done)` where `done` is true once the final exit
/// state is on-chain or in the mempool — i.e. the funds are committed to the owner's exit address.
///
/// **`Err` = blind** (external review F4): the chain backend could not be read, or a tier's stored
/// hex is unusable. It is never reported as `complete: false` with an empty broadcast list, which
/// would be indistinguishable from a healthy "waiting for the next CSV".
pub fn exit_pass(electrum: &electrum_client::Client, bundle: &TesrBundle) -> Result<ExitProgress> {
    let mut broadcast = Vec::new();
    let mut stalled = None;
    for tier in bundle.exit_tiers() {
        if tx_known(electrum, &tier.txid)? {
            continue; // already on-chain / in mempool
        }
        let raw = hex::decode(&tier.signed_tx).map_err(|e| {
            anyhow::anyhow!("tier {} carries unusable signed tx hex: {e}", tier.txid)
        })?;
        match electrum.transaction_broadcast_raw(&raw) {
            Ok(_) => broadcast.push(tier.txid.clone()),
            Err(e) => {
                // CSV not met yet / parent unconfirmed — retry on the next pass, but SAY SO.
                stalled = Some(format!("{}: {e}", tier.txid));
                break;
            }
        }
    }
    let complete = tx_known(electrum, &bundle.current().state.txid)?;
    Ok(ExitProgress { broadcast, complete, stalled })
}

/// The first tier not yet on-chain in exit order, and its relative-CSV (a wait-time hint).
/// `Ok(None)` once the exit is complete; **`Err` = blind** — a backend that cannot be read must not
/// be reported as a wait time, and must certainly not be reported as "complete".
pub fn next_exit_tier(electrum: &electrum_client::Client, bundle: &TesrBundle) -> Result<Option<u16>> {
    for tier in bundle.exit_tiers() {
        if !tx_known(electrum, &tier.txid)? {
            return Ok(Some(tier.csv.unwrap_or(0)));
        }
    }
    Ok(None)
}

fn net_from_str(network: &str) -> electrum_client::bitcoin::Network {
    use electrum_client::bitcoin::Network;
    match network.to_ascii_lowercase().as_str() {
        "bitcoin" | "mainnet" => Network::Bitcoin,
        "testnet" => Network::Testnet,
        "signet" => Network::Signet,
        _ => Network::Regtest,
    }
}

/// **Receiver R′ verification (PROTOCOL.md §5.11).** Soundly verify a conveyed TES-R ladder before
/// accepting a coin: it must be a valid unilateral-exit chain over the on-chain funding UTXO `F`, and
/// the SE's PUBLIC finalized-signature count must EXACTLY account for its tiers (plus the signed-once
/// backup txs conveyed with the coin — `flat_backups`, which is 1 for an ordinary on-chain root coin,
/// whose deposit co-signs one backup before the ladder is established, and 0 for an un-broadcast split
/// slot). Exact equality is the linchpin — it makes a hidden extra co-signed state (a
/// double-spend the receiver can't see) impossible, and prevents padding the ladder with junk. Checks:
///   1. the trigger spends `F` (no relative-timelock) and pays the aggregate key `A`;
///   2. every later tier spends its parent's `out[0]`, carries a BIP-68 block CSV within the coin's
///      schedule bounds, and pays `A` — except the final state, which pays the owner;
///   3. `se_num_sigs == flat_backups + <number of tiers>`.
/// This is a PURE function (no network) so it is unit-testable and reusable by the transfer receiver.
///
/// ⚠️ **SELF-VERIFICATION ONLY — this entry point is UNBOUND (audit C-1).** Every authority it checks
/// against (`f_txid`/`f_vout`/`f_value`, `agg_address`, `statechain_id`) is a field OF THE BUNDLE, so
/// it proves INTERNAL CONSISTENCY, not that the bundle describes the coin you are about to accept. A
/// sender can convey a self-consistent DECOY ladder over an attacker-controlled outpoint, with
/// `owner_exit_address` set correctly and the tiers padded so the census balances, and keep the REAL
/// trigger — then take the coin back after the receiver accepts. Use it to re-check a ladder you built
/// yourself; to ACCEPT a conveyed one, use [`verify_bundle_bound`].
///
/// ⚠️ **It also has NO TRIGGER-TO-`F` ANCHOR, and cannot be given one.** The value laws bind each
/// tier to the one above it, so a ladder conserves value RELATIVE to what the trigger pays itself.
/// Making that absolute takes the value of the on-chain funding output, and this entry point has no
/// chain access — so it passes `None` and the trigger's hop is skipped. Passing `bundle.f_value`
/// instead would be worse than skipping it: the sender chose that number *and* co-signed the trigger
/// against it, so the check would pass for a skimming ladder exactly as it passes for an honest one,
/// while reading in review like an anchor. A caller that needs the coin's real value checked must
/// fetch `F` and use [`verify_bundle_bound`].
pub fn verify_bundle(bundle: &TesrBundle, se_num_sigs: u32, flat_backups: u32) -> Result<()> {
    // Ordinary bundle: the final state pays the owner. (A split parent uses verify_bundle_ex(true).)
    verify_bundle_ex(bundle, se_num_sigs, flat_backups, false, None)
}

/// The AUTHORITATIVE description of the coin an incoming ladder must describe. Every field is read
/// from a source the SENDER does not control — the funding transaction on chain and the coordinator's
/// `/info/statechain` record — never from the conveyed bundle.
#[derive(Clone, Debug)]
pub struct CoinAuthority {
    /// The statechain id the receiver/payer is acting on (the id whose `num_sigs` feeds the census).
    pub statechain_id: String,
    /// The coin's funding outpoint (`tx0_outpoint`), which the ladder's trigger must spend.
    pub f_txid: String,
    pub f_vout: u32,
    /// `tx0.output[f_vout].value`, read from the funding transaction.
    pub f_value: u64,
    /// `tx0.output[f_vout].script_pubkey` as hex — the on-chain P2TR of the coin's aggregate key `A`.
    pub f_spk_hex: String,
    /// The coordinator's recorded aggregate x-only key for `statechain_id` (`aggregate_pubkey`,
    /// UNIQUE per sid). `None` ⟹ REJECT: without it a rogue-key decomposition lets a sender point at
    /// a decoy sid whose counter happens to balance ([FATAL-B], migration 0009).
    pub se_aggregate_pubkey: Option<String>,
}

/// Build a [`CoinAuthority`] from the coin's FUNDING TRANSACTION plus the coordinator's record.
///
/// `tx0_hex` must be the funding transaction as read from the chain (never a sender-supplied
/// un-broadcast branch tx), and `(tx0_txid, tx0_vout)` the outpoint the coin rests on. The value and
/// the aggregate scriptPubKey are taken FROM that output, so neither can be restated by a sender.
pub fn coin_authority_from_tx0(
    statechain_id: &str,
    tx0_txid: &str,
    tx0_vout: u32,
    tx0_hex: &str,
    se_aggregate_pubkey: Option<String>,
) -> Result<CoinAuthority> {
    use electrum_client::bitcoin::{consensus::deserialize, Transaction};
    let raw = hex::decode(tx0_hex).map_err(|_| anyhow::anyhow!("funding tx is not hex"))?;
    let tx0: Transaction =
        deserialize(&raw).map_err(|_| anyhow::anyhow!("funding tx does not parse"))?;
    if tx0.txid().to_string() != tx0_txid {
        return Err(anyhow::anyhow!(
            "funding tx hex is {} but the coin's funding outpoint names {tx0_txid}",
            tx0.txid()
        ));
    }
    let out = tx0
        .output
        .get(tx0_vout as usize)
        .ok_or_else(|| anyhow::anyhow!("funding tx has no output {tx0_vout}"))?;
    Ok(CoinAuthority {
        statechain_id: statechain_id.to_string(),
        f_txid: tx0_txid.to_string(),
        f_vout: tx0_vout,
        f_value: out.value,
        f_spk_hex: hex::encode(out.script_pubkey.as_bytes()),
        se_aggregate_pubkey,
    })
}

/// The BIP-341-tweaked P2TR output key (x-only hex) of an UNTWEAKED aggregate x-only key. The server
/// records the untweaked aggregate; an on-chain scriptPubKey commits to the tweaked output key, so a
/// comparison between the two must go through this.
fn tweaked_p2tr_key_hex(
    agg_xonly_hex: &str,
    net: electrum_client::bitcoin::Network,
) -> Result<String> {
    use electrum_client::bitcoin::{
        secp256k1::{Secp256k1, XOnlyPublicKey},
        Address,
    };
    let xonly = XOnlyPublicKey::from_str(agg_xonly_hex)
        .map_err(|_| anyhow::anyhow!("bad aggregate x-only hex"))?;
    let spk =
        Address::p2tr(&Secp256k1::verification_only(), xonly, None, net).script_pubkey();
    taproot_key_hex(spk.as_bytes())
}

/// **[R5] ESTABLISH-TIME bindability gate — do not create a ladder no receiver can bind.**
///
/// `verify_bundle_bound` fails CLOSED when the coordinator has no `aggregate_pubkey` on record for the
/// sid. On the ACCEPTANCE path that is the only correct answer (see the rejected alternatives below).
/// But the coordinator's aggregate column was added by migration 0009 and backfilled FORWARD only, so
/// pre-0009 rows are NULL (empirically a closed, contiguous legacy set: every NULL id below a clean
/// cut, none above). A pre-0009 coin is otherwise an ordinary confirmed on-chain root coin, so the
/// default IN-PLACE ladder pass happily ladders it — and the resulting ladder is unclaimable.
///
/// ⚠️ **This gate does NOT make such a coin transferable again, and must not be advertised as if it
/// did.** A legacy no-aggregate coin cannot carry a BOUND ladder — there is no coordinator aggregate
/// to bind against — so this gate keeps it un-laddered rather than minting an unclaimable ladder.
/// It still transfers on the flat lane at `protocol_version 0`: the claim path's unconditional
/// version floor was implemented, shown to break sdk41, and narrowed to a version/payload
/// consistency check, so that door is NOT shut. The only
/// complete fix is coordinator-side: backfill `aggregate_xonly` for the legacy rows from the
/// coordinator's OWN columns (`x_only(user_public_key + server_public_key)`), which is the same value
/// deposit-init records today and involves no client input. Until that runs, a legacy coin's value is
/// still recoverable — on-chain withdrawal and its signed-once backup exit are untouched — but it
/// cannot move off-chain.
///
/// What this gate DOES buy, and why it is still worth having:
///   * establishing a ladder co-signs three tiers through the SE. That is IRREVERSIBLE: it spends
///     signature budget and permanently raises the sid's `num_sigs`, on a coin that gains nothing;
///   * it stops an unclaimable bundle from being persisted and later conveyed as authentic;
///   * it is self-healing — the moment the coordinator backfills, the next `claim()` pass ladders the
///     coin normally, with no client change.
///
/// The predicate is exactly the authority half of `verify_bundle_bound`, hoisted ahead of
/// establishment: the coordinator must have an aggregate on record for the sid, and it must be the key
/// that actually controls the funding output. `f_spk_hex` is `tx0.output[vout].script_pubkey` read FROM
/// THE CHAIN and `se_aggregate_pubkey` the coordinator's `/info/statechain` record — the same two
/// authorities the receiver will use, so this predicate and the acceptance check cannot drift.
///
/// **Rejected alternatives** (both would have re-opened C-1/[FATAL-B]):
///   * *"When the coordinator has no aggregate, derive it from `tx0.output[vout]` instead."* That key
///     is already `a_onchain`, and the ladder is already checked against it — so this is not a second
///     authority, it is the absence of one. `tx0`, its outpoint and the ladder all arrive from the
///     SENDER; the only other thing tying them to the sid is `validate_tx0_output_pubkey`, which tests
///     `enclave_pubkey(sid) + transfer_msg.user_public_key == tx0.out[vout]` — and the sender chooses
///     `user_public_key`, so the rogue-key decomposition `U := D − E_sid` makes ANY attacker-controlled
///     output `D` pass. The coordinator's per-sid, UNIQUE aggregate is the one value in the whole
///     acceptance path that is not restatable by the sender. Making its absence a fallback would also
///     hand the attacker the trigger: he picks which sid to convey, hence whether the record is NULL.
///   * *"Accept a ladder whose tiers carry genuine SE co-signatures under `A`, sid-record or not."* A
///     schnorr co-sign proves the SE signed under `A` for SOME sid, never for THIS one; an attacker
///     who owns a second coin can pad a decoy ladder over it to any tier count, and the census is run
///     against the conveyed sid's `num_sigs`, which he also controls. Not a substitute.
pub fn ladder_binding_precheck(
    statechain_id: &str,
    f_spk_hex: &str,
    se_aggregate_pubkey: Option<&str>,
    network: &str,
) -> Result<()> {
    ladder_binding_precheck_cause(statechain_id, f_spk_hex, se_aggregate_pubkey, network)
        .map_err(anyhow::Error::new)
}

/// **WHY the ladder could not be bound — the typed counterpart of [`ladder_binding_precheck`].**
///
/// The causes are NOT interchangeable, and a caller that collapses them fails OPEN. Only
/// [`BindingRefusal::NoCoordinatorAggregate`] is a structurally PERMANENT property of the coin (a
/// pre-0009 legacy row: nothing about the coin will ever make it bindable until the coordinator
/// backfills), and it is the only cause that may be read as "this coin legitimately has no ladder".
/// Every other cause means the coin's shape is WRONG or unreadable — a non-taproot funding output, a
/// scriptPubKey we could not parse, an aggregate that does not control `F` — and a caller must
/// refuse rather than treat it as a licence.
///
/// The flat-conveyance classifier in `transfer_sender` used to test `ladder_binding_precheck(..)
/// .is_err()` and license the flat lane on ANY error, which silently folded "decoy-shaped coin" and
/// "spk we could not decode" into "harmless legacy coin". This enum exists so it cannot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindingRefusal {
    /// The coordinator has NO `aggregate_pubkey` on record for the sid (pre-migration-0009 legacy
    /// row). PERMANENT for the coin until a coordinator-side backfill.
    NoCoordinatorAggregate,
    /// The funding scriptPubKey is not hex — the caller handed us something unreadable.
    FundingSpkUnparseable,
    /// The funding output is not a v1 taproot output, so it has no aggregate key to bind to.
    FundingNotTaproot,
    /// The coordinator's recorded aggregate could not be tweaked into a P2TR key (unusable key).
    AggregateUnusable,
    /// The coordinator HAS an aggregate for the sid, but it is not the key controlling `F`. A ladder
    /// here would be refused as a decoy at acceptance.
    AggregateMismatch,
}

/// [`BindingRefusal`] plus the human-readable message the untyped
/// [`ladder_binding_precheck`] has always produced (unchanged, so callers that string-match keep
/// working).
#[derive(Clone, Debug)]
pub struct LadderBindingError {
    pub cause: BindingRefusal,
    pub message: String,
}

impl std::fmt::Display for LadderBindingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for LadderBindingError {}

/// [`ladder_binding_precheck`] with the refusal CAUSE preserved. See [`BindingRefusal`].
pub fn ladder_binding_precheck_cause(
    statechain_id: &str,
    f_spk_hex: &str,
    se_aggregate_pubkey: Option<&str>,
    network: &str,
) -> std::result::Result<(), LadderBindingError> {
    let refuse = |cause: BindingRefusal, message: String| LadderBindingError { cause, message };
    let net = net_from_str(network);
    let f_spk = hex::decode(f_spk_hex).map_err(|_| {
        refuse(
            BindingRefusal::FundingSpkUnparseable,
            "funding scriptPubKey is not hex — cannot bind a ladder".to_string(),
        )
    })?;
    let a_onchain = taproot_key_hex(&f_spk).map_err(|e| {
        refuse(
            BindingRefusal::FundingNotTaproot,
            format!("coin funding output is not a v1 taproot output: {e}"),
        )
    })?;
    let se_agg = se_aggregate_pubkey.ok_or_else(|| {
        refuse(
            BindingRefusal::NoCoordinatorAggregate,
            format!(
                "the coordinator recorded no aggregate for statechain id {statechain_id} — a ladder \
                 over this coin could not be bound by any receiver (pre-0009 legacy coin: leave it \
                 un-laddered)"
            ),
        )
    })?;
    let se_agg_spk = tweaked_p2tr_key_hex(se_agg, net).map_err(|e| {
        refuse(
            BindingRefusal::AggregateUnusable,
            format!(
                "the coordinator's aggregate for statechain id {statechain_id} is not a usable \
                 x-only key ({e}) — a ladder over this coin could not be bound"
            ),
        )
    })?;
    if se_agg_spk != a_onchain {
        return Err(refuse(
            BindingRefusal::AggregateMismatch,
            format!(
                "the coordinator's aggregate for statechain id {statechain_id} does not match the \
                 funding output key — a ladder over this coin would be refused as a decoy"
            ),
        ));
    }
    Ok(())
}

/// **[C-1] ACCEPTANCE-PATH verifier — [`verify_bundle`] BOUND TO THE COIN.**
///
/// `verify_bundle` proves a ladder is internally consistent. That is not the question a receiver (or a
/// pre-paying SSP) is asking: it needs to know that THIS ladder describes THE COIN it is accepting.
/// The attack this closes is the default coin shape, not an exotic one — a self-signed decoy ladder
/// over an attacker-controlled outpoint, `owner_exit_address` correctly set to the receiver's key (so
/// the Model-A gate passes) and tiers padded so the census balances, while the sender retains the REAL
/// trigger and spends the coin back afterwards.
///
/// So the authority is derived from the COIN and the ladder is checked against it:
///   * `bundle.statechain_id` == the sid whose `num_sigs` the census is run against — otherwise the
///     count being balanced belongs to a different coin;
///   * `(f_txid, f_vout)` == the coin's funding outpoint, and `f_value` == that output's value read
///     from the funding transaction (the value every tier's co-sign sighash commits to);
///   * `bundle.agg_address` == P2TR of the key in `tx0.output[f_vout].script_pubkey`, so the key the
///     tiers are verified against is the ON-CHAIN one;
///   * that same key == the coordinator's recorded `aggregate_pubkey` for the sid, which is UNIQUE per
///     sid — so a second outpoint paying the same aggregate cannot be substituted under a decoy sid,
///     and (with the exact-equality census) a second ladder over a second UTXO of the SAME sid would
///     need extra SE co-signs and blows the count.
///
/// Fails CLOSED on every absent/unparseable field, including a missing server aggregate.
pub fn verify_bundle_bound(
    bundle: &TesrBundle,
    se_num_sigs: u32,
    flat_backups: u32,
    coin: &CoinAuthority,
) -> Result<()> {
    if bundle.statechain_id != coin.statechain_id {
        return Err(anyhow::anyhow!(
            "conveyed ladder is for statechain id {} but the coin being accepted is {} — the census would balance a different coin",
            bundle.statechain_id,
            coin.statechain_id
        ));
    }
    if bundle.f_txid != coin.f_txid || bundle.f_vout != coin.f_vout {
        return Err(anyhow::anyhow!(
            "conveyed ladder is rooted at {}:{} but the coin's funding outpoint is {}:{} — decoy ladder",
            bundle.f_txid,
            bundle.f_vout,
            coin.f_txid,
            coin.f_vout
        ));
    }
    if bundle.f_value != coin.f_value {
        return Err(anyhow::anyhow!(
            "conveyed ladder declares F value {} but the funding output carries {} — the tier sighashes commit to the real value",
            bundle.f_value,
            coin.f_value
        ));
    }

    let net = net_from_str(&bundle.network);
    let f_spk = hex::decode(&coin.f_spk_hex).map_err(|_| anyhow::anyhow!("bad funding spk hex"))?;
    let a_onchain = taproot_key_hex(&f_spk)
        .map_err(|e| anyhow::anyhow!("coin funding output is not a v1 taproot output: {e}"))?;
    let declared_agg_spk = electrum_client::bitcoin::Address::from_str(&bundle.agg_address)
        .map_err(|_| anyhow::anyhow!("bad ladder agg_address"))?
        .require_network(net)
        .map_err(|_| anyhow::anyhow!("ladder agg_address is on the wrong network"))?
        .script_pubkey();
    if taproot_key_hex(declared_agg_spk.as_bytes())? != a_onchain {
        return Err(anyhow::anyhow!(
            "conveyed ladder's aggregate address does not match the coin's on-chain funding key — decoy ladder"
        ));
    }

    let se_agg = coin.se_aggregate_pubkey.as_deref().ok_or_else(|| {
        anyhow::anyhow!(
            "the coordinator recorded no aggregate for statechain id {} (fail-closed)",
            coin.statechain_id
        )
    })?;
    if tweaked_p2tr_key_hex(se_agg, net)? != a_onchain {
        return Err(anyhow::anyhow!(
            "the coordinator's aggregate for statechain id {} does not match the funding output key — decoy coin",
            coin.statechain_id
        ));
    }

    // `coin.f_value` is `tx0.output[f_vout].value` read from the funding transaction by
    // `coin_authority_from_tx0`, and the equality above has just refused any bundle that disagrees
    // with it. So this is the chain's number, and the trigger is bound to it — which is what turns
    // every relative law below into an absolute one.
    verify_bundle_ex(bundle, se_num_sigs, flat_backups, false, Some(coin.f_value))
}

/// As [`verify_bundle`], but when `final_is_split` the FINAL tier is an in-ladder split state `SP` that
/// pays the children (not the owner), so its output-payee check is skipped here — each child's outputs
/// are verified against its own aggregate by [`verify_child_bundle`]. Everything else (co-signs under
/// `A`, the per-outpoint race, the exact-equality census) is unchanged.
/// Validate + count a segment's DISCLOSED SUPERSEDED tiers, returning how many were accepted.
///
/// Shared by the root ladder (`verify_bundle_ex`) and a split CHILD's own segment, so the two can
/// never drift: a second copy of this battery is exactly how the `[S1]` count-padding class returns.
/// The caller supplies the segment's aggregate scriptPubKey, its schedule params, the running
/// `prevout_value_of` map (this fn INSERTS every superseded output into it before validating, so a
/// superseded tier may legitimately parent another — the renewal/transitive-death case), and the
/// per-outpoint CSV of the LIVE tier spending it.
///
/// Every returned entry has been: parsed, txid-bound, ladder-linked, verified as a genuine co-sign by
/// the aggregate key, CSV-checked within the schedule bounds, and proven NON-CONFIRMABLE — either it
/// directly loses a maturity race to the live tier over the same outpoint, or its prevout is
/// transitively never created. Anything else is an orphan/threat branch and is refused.
fn verify_superseded_segment(
    sup_states: &[TesrTier],
    sup_exts: &[TesrTier],
    agg_spk: &electrum_client::bitcoin::ScriptBuf,
    p: &mercurylib::tesr::TesrParams,
    prevout_value_of: &mut std::collections::HashMap<(electrum_client::bitcoin::Txid, u32), u64>,
    live_csv_by_outpoint: &std::collections::HashMap<(electrum_client::bitcoin::Txid, u32), u32>,
    live_txids: &std::collections::HashSet<electrum_client::bitcoin::Txid>,
) -> Result<u32> {
    use electrum_client::bitcoin::{consensus::deserialize, Transaction, Txid};
    // [C-2] ONE co-sign, ONE census slot. The return value of this function is added to the SE's
    // exact-equality count, so a DUPLICATE disclosure inflates `expected` by one for free and absorbs
    // one hidden co-signed state — the same padding class as `[S1]`, except every padded entry is a
    // GENUINE tier and so passes parse, txid-binding, linkage, signature and race checks unchanged.
    // One set spanning BOTH superseded lists AND the live exit tiers closes it: a tier may appear in
    // the bundle exactly once, whether it is disclosed twice or disclosed once while also being live.
    let mut seen_txids: std::collections::HashSet<Txid> = live_txids.clone();
    // Parsed up-front so their outputs can also serve as parents (e.g. a superseded extension's state
    // after a renewal) and so no unparseable entry reaches the checks below.
    let mut superseded_parsed: Vec<(&'static str, usize, &TesrTier, Transaction)> = Vec::new();
    for (kind, list) in [("state", sup_states), ("extension", sup_exts)] {
        for (j, s) in list.iter().enumerate() {
            let raw = hex::decode(&s.signed_tx)
                .map_err(|_| anyhow::anyhow!("superseded {kind} {j}: bad hex"))?;
            let tx: Transaction = deserialize(&raw)
                .map_err(|_| anyhow::anyhow!("superseded {kind} {j}: not a transaction"))?;
            if tx.input.len() != 1 || tx.output.is_empty() {
                return Err(anyhow::anyhow!("superseded {kind} {j}: malformed tier"));
            }
            if tx.txid().to_string() != s.txid {
                return Err(anyhow::anyhow!("superseded {kind} {j}: txid does not match its tx"));
            }
            let id = tx.txid();
            if !seen_txids.insert(id) {
                return Err(anyhow::anyhow!(
                    "superseded {kind} {j}: tier {id} is disclosed more than once (or is also a live tier) \
                     — one co-sign may be counted only once, and a repeat masks a hidden state"
                ));
            }
            for (vout, o) in tx.output.iter().enumerate() {
                prevout_value_of.insert((id, vout as u32), o.value);
            }
            superseded_parsed.push((kind, j, s, tx));
        }
    }

    // Static per-tier validation (parse/linkage/co-sign/CSV) followed by a NON-CONFIRMABILITY fixpoint.
    //
    // A disclosed superseded tier is safe to count iff it can never CONFIRM and out-race the owner. There
    // are two ways it is proven non-confirmable:
    //   (i)  DIRECT CONTENTION: it spends the same outpoint as a LIVE exit tier, and its CSV is strictly
    //        greater — it loses the maturity race (lower CSV wins). This is the presign/transfer case
    //        (old state and S' both spend the live extension's out[0]).
    //   (ii) TRANSITIVE DEATH: its parent (the tier whose output it spends) is itself a superseded tier
    //        proven non-confirmable — so this tier's prevout is never created on-chain. This is the RENEW
    //        case: renew supersedes BOTH the extension and the state, so the old state spends the OLD
    //        (superseded) extension's out[0], which no LIVE tier spends. The old extension loses its own
    //        race for T.out[0] to the new extension, so it can never confirm, so the old state can never
    //        confirm. Rejecting it as an "orphan" (the earlier check did) wrongly bricked every renewed
    //        coin — uncaught because no test ran renew + verify_bundle together.
    // A tier that is NEITHER out-raced by a live tier NOR rooted (transitively) in such a losing race is a
    // real orphan/threat and is refused. Cycles never root in a live contention, so they are never marked
    // dead ⟹ never accepted.
    struct Sup {
        kind: &'static str,
        j: usize,
        prevout: (Txid, u32),
        csv: u32,
        outputs: Vec<(Txid, u32)>,
    }
    let mut sups: Vec<Sup> = Vec::with_capacity(superseded_parsed.len());
    for (kind, j, s, tx) in superseded_parsed.iter() {
        let (kind, j) = (*kind, *j);
        // (a) parsed + txid-bound already (pre-pass above) — no unparseable entry reaches here.
        // (b) ladder linkage — it must spend an output of a tier of THIS ladder.
        let op = tx.input[0].previous_output;
        let value = *prevout_value_of
            .get(&(op.txid, op.vout))
            .ok_or_else(|| anyhow::anyhow!("superseded {kind} {j}: spends an outpoint outside this ladder"))?;
        // (c) signature — proves it actually consumed an SE co-sign.
        verify_tier_cosigned(tx, value, &agg_spk)
            .map_err(|e| anyhow::anyhow!("superseded {kind} {j} is not co-signed by A: {e}"))?;
        // (d) CSV is REQUIRED (never skippable), a BIP-68 block relative-timelock, in schedule bounds.
        let seq = tx.input[0].sequence.0;
        if seq & (1 << 31) != 0 || seq & (1 << 22) != 0 {
            return Err(anyhow::anyhow!("superseded {kind} {j}: not a BIP-68 block relative-timelock"));
        }
        let csv = seq as u16;
        let declared = s.csv.ok_or_else(|| anyhow::anyhow!("superseded {kind} {j}: no CSV declared"))?;
        if declared != csv {
            return Err(anyhow::anyhow!("superseded {kind} {j}: declared CSV {declared} != tx CSV {csv}"));
        }
        let (lo, hi) = if kind == "extension" { (p.e_floor, p.e0) } else { (p.d_floor, p.d0) };
        if csv < lo || csv > hi {
            return Err(anyhow::anyhow!("superseded {kind} {j}: CSV {csv} outside bounds [{lo},{hi}]"));
        }
        let outputs = tx
            .output
            .iter()
            .enumerate()
            .map(|(v, _)| (tx.txid(), v as u32))
            .collect();
        sups.push(Sup { kind, j, prevout: (op.txid, op.vout), csv: csv as u32, outputs });
    }

    // (e) NON-CONFIRMABILITY. Seed from direct contention with a live tier; propagate transitive death.
    let mut dead = vec![false; sups.len()];
    let mut dead_outputs: std::collections::HashSet<(Txid, u32)> = std::collections::HashSet::new();
    for (idx, sup) in sups.iter().enumerate() {
        if let Some(&live_csv) = live_csv_by_outpoint.get(&sup.prevout) {
            // Directly contends with a live tier over the SAME outpoint — it MUST lose the race, or it
            // could mature first and steal. (This is [S-1/S-2]: extensions and states alike, against the
            // live tier spending the same outpoint, never a global final_csv.)
            if sup.csv <= live_csv {
                return Err(anyhow::anyhow!(
                    "superseded {} {} has CSV {} <= the live tier's {} over the same outpoint — it could out-race the owner",
                    sup.kind, sup.j, sup.csv, live_csv
                ));
            }
            dead[idx] = true;
            for o in &sup.outputs {
                dead_outputs.insert(*o);
            }
        }
    }
    loop {
        let mut changed = false;
        for idx in 0..sups.len() {
            if dead[idx] {
                continue;
            }
            if dead_outputs.contains(&sups[idx].prevout) {
                dead[idx] = true;
                for o in sups[idx].outputs.clone() {
                    dead_outputs.insert(o);
                }
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    for (idx, sup) in sups.iter().enumerate() {
        if !dead[idx] {
            return Err(anyhow::anyhow!(
                "superseded {} {} is neither out-raced by a live tier nor transitively dead (spends {}:{}) — orphan/threat branch",
                sup.kind, sup.j, sup.prevout.0, sup.prevout.1
            ));
        }
    }
    Ok(sups.len() as u32)
}

/// `f_onchain` — **the value of the on-chain funding output `F`, as the CALLER read it from the
/// chain**, and the one term in this function that cannot be derived from the bundle. It is an
/// `Option` on purpose, and the purpose is honesty rather than convenience:
///
///   * `Some(v)` — the caller holds a chain-derived `F` value and the trigger is bound to it (the
///     law below runs at `i == 0`). Every check downstream then measures against a number Bitcoin
///     has already agreed to, instead of one the sender picked.
///   * `None` — the caller has **no chain access at all**, so there is no `F` to anchor to. The
///     trigger's law is skipped and the ladder is checked for internal consistency only.
///
/// The obvious-looking third option — "`None` means fall back to `bundle.f_value`" — is the thing
/// this parameter exists to prevent. `bundle.f_value` is a serde field; measuring the trigger
/// against it proves the sender was self-consistent and nothing more, while producing a check that
/// *reads* like an anchor. Per `docs/utexo/ADMISSION-INPUTS.md`, being present in a struct the
/// sender sent is not provenance, and a term with no provenance must be left out of the calculation
/// rather than dressed up. So the type makes each caller state which of the two it has.
fn verify_bundle_ex(
    bundle: &TesrBundle,
    se_num_sigs: u32,
    flat_backups: u32,
    final_is_split: bool,
    f_onchain: Option<u64>,
) -> Result<()> {
    use electrum_client::bitcoin::{consensus::deserialize, Address, Transaction, Txid};

    let net = net_from_str(&bundle.network);
    let spk_of = |addr: &str| -> Result<_> {
        Ok(Address::from_str(addr)
            .map_err(|_| anyhow::anyhow!("bad address {addr}"))?
            .require_network(net)
            .map_err(|_| anyhow::anyhow!("address {addr} wrong network"))?
            .script_pubkey())
    };
    let agg_spk = spk_of(&bundle.agg_address)?;
    let owner_spk = spk_of(&bundle.owner_exit_address)?;

    let tiers = bundle.exit_tiers(); // [trigger, ext0, state0, ext1, state1, ...]
    if tiers.len() < 3 || (tiers.len() - 1) % 2 != 0 {
        return Err(anyhow::anyhow!("malformed ladder: expected trigger + N*(extension,state)"));
    }
    // [CTES-R] Colour is STRUCTURAL, and it is checked on every acceptance path (claim + SSP
    // pre-pay), colour-blind and with no RGB engine. See `verify_colored_shape`.
    verify_colored_shape(bundle)?;

    let txs: Vec<Transaction> = tiers
        .iter()
        .map(|t| {
            let raw = hex::decode(&t.signed_tx).map_err(|_| anyhow::anyhow!("bad tier hex"))?;
            deserialize::<Transaction>(&raw).map_err(|_| anyhow::anyhow!("bad tier tx"))
        })
        .collect::<Result<_>>()?;

    // 1. Trigger spends F and pays A.
    let t = &txs[0];
    if t.input.len() != 1
        || t.input[0].previous_output.txid != Txid::from_str(&bundle.f_txid).map_err(|_| anyhow::anyhow!("bad F txid"))?
        || t.input[0].previous_output.vout != bundle.f_vout
    {
        return Err(anyhow::anyhow!("trigger does not spend the funding UTXO F"));
    }
    // The trigger must pay `A` on its declared PAYLOAD output (`payload_vout`, 0 today). Reading the
    // payload through the accessor rather than `output[0]` is what keeps this check meaningful once a
    // coloured tier carries an opret at index 0 — and a bogus declared index fails closed right here.
    // This is PIN 2; the trigger's value is not read from it (the value loop below uses the stronger
    // PIN 1 the structural loop produces), so the `LinkedPayload` is discharged rather than kept.
    tiers[0].link_pays(t, &agg_spk, "trigger", "trigger does not pay the aggregate key A")?;
    // [B1] The trigger's DECLARED timelock must be the one it was signed with — `None`, since
    // `TRIGGER_SEQUENCE` disables the relative lock. A trigger declaring a CSV it does not carry
    // would inflate the exit-headroom requirement rather than shrink it, but the binding is not a
    // one-directional guard: the declared field is either the signed number or the bundle is refused.
    mercurylib::transfer::receiver::bind_declared_csv(
        0,
        "trigger",
        tiers[0].csv,
        signed_relative_csv(t, "trigger")?,
    )?;

    // 2. Each later tier spends its parent's PAYLOAD output, within schedule bounds, paying A (or the
    //    owner if final).
    let p = &bundle.params;
    // The parent-side product of this loop: `parent_links[j]` is tier `j`'s payload output TOGETHER
    // WITH the proof that tier `j+1` spends exactly it. The value loop below indexes this instead of
    // re-deriving the output from `payload_vout`, which is what makes the check-ordering defect
    // unwritable rather than merely commented-against — see `mod linked`. Coverage is exact: the
    // value law needs the payload of every tier that funds another, i.e. `0..txs.len()-1`, and that
    // is precisely the set this loop links.
    let mut parent_links: Vec<LinkedPayload> = Vec::with_capacity(txs.len().saturating_sub(1));
    for i in 1..txs.len() {
        let tx = &txs[i];
        parent_links.push(tiers[i - 1].link_child(
            &txs[i - 1],
            tx,
            &format!("tier {}", i - 1),
            &format!("tier {i} does not spend its parent's payload output"),
        )?);
        let seq = tx.input[0].sequence.0;
        if seq & (1 << 31) != 0 || seq & (1 << 22) != 0 {
            return Err(anyhow::anyhow!("tier {i} is not a BIP-68 block relative-timelock"));
        }
        let csv = seq as u16;
        let is_extension = i % 2 == 1;
        let is_final = i == txs.len() - 1;
        // [CATS] THREE tier kinds, and the kind is STRUCTURAL — never sender-declared. `is_extension`
        // comes from position parity; `final_is_split` is a code-path constant chosen by the RECEIVER
        // (`false` on every whole-coin path, `true` only from `verify_child_bundle`), not a field on
        // the bundle. That matters: the spine's `[0,0]` is the loosest-looking bound in the file, and
        // it would be a real hole if a sender could elect into it. They cannot.
        //
        // It is a NEW KIND rather than a widened state range on purpose. Widening `[d_floor, d0]` to
        // include 0 would let any state tier be un-timelocked, which is exactly the [B1] shape;
        // pinning the spine to EXACTLY 0 means an `SP` that tried to carry a real timelock — quietly
        // making every payee's exit thousands of blocks slower with no refusal anywhere — is rejected.
        let (lo, hi) = if is_final && final_is_split {
            (SPINE_CSV, SPINE_CSV)
        } else if is_extension {
            (p.e_floor, p.e0)
        } else {
            (p.d_floor, p.d0)
        };
        if csv < lo || csv > hi {
            return Err(anyhow::anyhow!(
                "tier {i} CSV {csv} outside {} bounds [{lo},{hi}]",
                if is_final && final_is_split { "SPINE" } else if is_extension { "extension" } else { "state" }
            ));
        }
        // [B1] …and the tier's DECLARED `csv` field must be that same number. This function already
        // held the signed `nSequence` in its hand for the schedule-bounds check above and never
        // compared it to the field the bundle travels with, which is how a declared schedule could
        // contradict the signatures and still be accepted by every census check.
        mercurylib::transfer::receiver::bind_declared_csv(
            i,
            &format!("tier {i}"),
            tiers[i].csv,
            Some(csv),
        )?;
        if is_final && final_is_split {
            // SP (split state) pays the children, not the owner; its outputs are verified per-child by
            // verify_child_bundle (A_child == SP.out[j]). Skip the single-owner payee check here.
        } else {
            let want = if is_final { &owner_spk } else { &agg_spk };
            // PIN 2 again. For a non-final tier this duplicates what PIN 1 already established one
            // iteration later; for the FINAL tier it is the only pin there can be, since nothing
            // spends it. Discharged either way — the value loop reads parents, never this.
            tiers[i].link_pays(
                tx,
                want,
                &format!("tier {i}"),
                &format!("tier {i} pays the wrong output"),
            )?;
        }
    }

    // ═══ [VALUE-CONSERVATION] THE ROOT LADDER, TIER BY TIER ═══
    //
    // ORDERING IS DELIBERATE, AND IS NOW ENFORCED BY THE TYPE. This runs AFTER the structural loop
    // above, not before it. Placed first, it shadowed the specific "tier N pays the wrong output"
    // refusal with a generic arithmetic one — a wrong `payload_vout` makes this law read the P2A
    // anchor as the funding value and fail on the NEXT tier, so the message named the wrong tier and
    // the wrong cause. The structural checks are the better diagnostics; this is the check that
    // catches what they cannot see. The funding term below is `parent_links[i-1].value()`, and a
    // `LinkedPayload` can only be produced BY the structural loop — so moving this block above it no
    // longer reads the anchor, it fails to compile. See `mod linked`.
    //
    //
    // **A CORRECTION.** Commit 4e165e6 bound the split CHILD's chain and argued this lane did not
    // need it, on the grounds that "a whole coin always retains its flat backup chain … so a skim
    // here degrades the fast path and leaves the slow path whole". **That reasoning is wrong, and
    // [B1] is why.** `T` is un-timelocked and spends `F`; every prior owner retains a co-signed copy
    // of it. The moment any of them broadcasts `T`, `F` is spent and EVERY flat backup — which all
    // spend `F` — is void. The fallback is not merely slower, it is destroyed at the attacker's
    // choosing, by the same party who built the skimming ladder.
    //
    // So the skim is theft here too, and it is worse than on the child lane: the receiver books the
    // ON-CHAIN funding value (`amount: tx0_output.value`, lib/src/transfer/receiver.rs:1003, assigned
    // at transfer_receiver.rs:1486), which is the largest number in the whole structure.
    //
    // The law is the same one the builders use — each tier forwards its parent's payload output minus
    // exactly one rung — and it is checked here against values PARSED from the transactions, never
    // declared. `fee_rate` is the bundle's own field and so sender-influenced; that is acceptable in
    // this direction and only in this direction, because a *higher* declared rate makes the expected
    // forward value SMALLER, and a tier forwarding less than its own declared schedule demands is
    // exactly what the equality refuses. It is pinned properly by `bind_declared_csv`'s sibling
    // problem — see `docs/utexo/VALUE-CONSERVATION-SWEEP.md` for the remaining `fee_rate` item.
    //
    //
    // **AND THE CHAIN IS THE FIRST RUNG.** The loop below used to start at `i = 1`, which left the
    // TRIGGER bound to nothing: its payload output seeded the extension's law and was never itself
    // compared to anything. A chain of relative equalities anchored to a free value is a free chain —
    // a trigger spending a 200 000-sat `F` and paying the aggregate 1 000 makes every tier beneath it
    // conserve perfectly against a fiction, while the receiver books the on-chain 200 000. The theft
    // needs no extra co-sign, so the census balances, and it needs no counterparty: the coin's first
    // laddering owner plants it at `establish`.
    //
    // It is the WORST place in the structure to leave loose. `T` is un-timelocked, spends `F`, and
    // every prior owner keeps a co-signed copy ([B1]) — so broadcasting it both takes the money and
    // voids every flat backup, which all spend `F` too. The theft and the destruction of the slow
    // path are one transaction.
    //
    // `f_onchain` closes it, and only when the caller actually has a chain fact to close it with —
    // see the parameter's own note. This is `37d8bba`'s child-lane anchor moved down to where both
    // lanes pass through, so a caller inherits it rather than having to remember it.
    {
        let rung_forward = |prev: u64, n_payload: usize, what: &str| -> Result<u64> {
            let v = if bundle.is_colored() {
                crate::rgb::colored_tier_out_total(prev, n_payload, bundle.fee_rate)
            } else {
                mercurylib::tesr::tier_out_total(prev, n_payload, bundle.fee_rate)
            };
            v.ok_or_else(|| {
                anyhow::anyhow!(
                    "{what}: funding of {prev} sat cannot carry a {n_payload}-payload tier at {} sat/vB",
                    bundle.fee_rate
                )
            })
        };
        for i in 0..txs.len() {
            // Tier 0 is the trigger, and its funding is not another tier — it is the on-chain output
            // `F`. With no chain fact in hand there is nothing sound to compare it to, so the hop is
            // skipped rather than measured against `bundle.f_value`; the tiers below it are still
            // bound to each other.
            let prev_payload = if i == 0 {
                match f_onchain {
                    Some(v) => v,
                    None => continue,
                }
            } else {
                // The ONLY way this function can obtain a tier's payload value: a `LinkedPayload`
                // minted by the structural loop above, which proves tier `i` really spends it. There
                // is no expression here that could be written before that loop ran.
                parent_links[i - 1].value()
            };
            let is_final = i == txs.len() - 1;
            // How this hop names itself in a refusal. Tier 0's funding is the chain's, not another
            // tier's, and saying so is the difference between "some arithmetic did not add up" and
            // "the coin is not worth what it says it is".
            let what = if i == 0 { "the trigger".to_string() } else { format!("tier {i}") };
            let funded_by = if i == 0 {
                format!("{prev_payload} sat, read from the ON-CHAIN funding output F,")
            } else {
                format!("{prev_payload} sat")
            };
            // The final tier of a SPLIT parent is an `SP` with N payload outputs funding N children,
            // so its law is Σ(payloads) rather than a single forward. Every other tier has exactly one
            // payload output. `n` is derived from the transaction: total outputs less the P2A anchor,
            // less the opret when coloured — never from a declared count.
            let n_payload = if is_final && final_is_split {
                let anchors = txs[i]
                    .output
                    .iter()
                    .filter(|o| o.script_pubkey.as_bytes() == mercurylib::tesr::P2A_SCRIPT_BYTES)
                    .count();
                let oprets = txs[i].output.iter().filter(|o| o.script_pubkey.is_op_return()).count();
                txs[i].output.len().saturating_sub(anchors + oprets)
            } else {
                1
            };
            if n_payload == 0 {
                return Err(anyhow::anyhow!("{what} has no payload output at all"));
            }
            let expect = rung_forward(prev_payload, n_payload, &what)?;
            // Σ over the payload outputs — which for the single-payload case is just `out[payload_vout]`,
            // and for `SP` is every child's slot. Summing rather than checking one output is what makes
            // an EXTRA output impossible to hide: any sats routed elsewhere make the sum come up short.
            let got: u64 = txs[i]
                .output
                .iter()
                .filter(|o| {
                    o.script_pubkey.as_bytes() != mercurylib::tesr::P2A_SCRIPT_BYTES
                        && !o.script_pubkey.is_op_return()
                })
                .map(|o| o.value)
                .sum();
            if got != expect {
                return Err(anyhow::anyhow!(
                    "{what} is funded with {funded_by} but its payload outputs carry {got} \
                     (expected exactly {expect} = funding − one rung at {} sat/vB across {n_payload} \
                     payload output(s)) — the difference would leave the owner's exit chain, while the \
                     receiver is credited the on-chain funding value{}",
                    bundle.fee_rate,
                    if i == 0 {
                        " — and every tier below it conserves against a number the sender chose \
                         rather than one the chain agreed to"
                    } else {
                        ""
                    }
                ));
            }
            // [GAP 1] A non-split tier must have EXACTLY ONE payload output, and the Σ check alone
            // does not say so. `n_payload` is hard-coded to 1 above for these tiers while `got` sums
            // over every payload output, so an attacker who splits the forward value across TWO
            // outputs — 1 000 onward and the rest to themselves — keeps Σ exactly on the expected
            // total, commits the honest fee, and sails through. The leaf lane refuses this only
            // because it ALSO checks the single `out[payload_vout]`, a line that reads as redundant
            // beside its Σ check and is not; the root lane had no such second line.
            //
            // Found by `skim_root_attack_tests`, by running it. The sweep's §1 argues for the Σ form
            // because pinning one output leaves a fee-wide window; the converse — that Σ alone leaves
            // a redistribution window — was true the whole time and written down nowhere.
            let actual_payloads = txs[i]
                .output
                .iter()
                .filter(|o| {
                    o.script_pubkey.as_bytes() != mercurylib::tesr::P2A_SCRIPT_BYTES
                        && !o.script_pubkey.is_op_return()
                })
                .count();
            if actual_payloads != n_payload {
                return Err(anyhow::anyhow!(
                    "{what} carries {actual_payloads} payload output(s) but this tier kind has \
                     exactly {n_payload} — the surplus could hold value that conserves in the SUM \
                     while never reaching the owner's exit chain"
                ));
            }
        }
    }

    // ---- Superseded tiers: PARSE + LADDER-LINK + SIGNATURE-VERIFY before they may be counted [S1].
    //
    // The count in check 3 is the anti-theft linchpin, so EVERY term of it must correspond to a real,
    // co-signed tier OF THIS LADDER. Previously `superseded_*` were only `.len()`-counted — never
    // parsed, linked or signature-checked — and the CSV race-check skipped `csv: None`. A sender
    // holding a hidden low-CSV state could pad one junk entry (`signed_tx: ""`, `csv: None`) to make
    // `expected` match the inflated `num_sigs`, get ACCEPTED, then broadcast the hidden state and take
    // the coin back. Parsing alone is NOT sufficient either: a structurally valid but never-co-signed
    // tx would still pad the count. Only a valid signature by `A` proves a tier consumed a co-sign.
    //
    // `prevout_value_of` maps (parent txid, vout) → the value that output carries — exactly the prevout
    // value the child's taproot key-spend sighash commits to (mirrors `cosign_tier_request`). Two
    // properties matter:
    //   * keyed PER-OUTPUT, not per-txid. A tier may legitimately hang off any `out[j]` of its parent —
    //     that is how an in-ladder split state (PROTOCOL.md §5.4) hosts N children, and the mechanism that
    //     dissolves B1 (a split that DESCENDS from the trigger instead of racing it for `F`). A
    //     txid-only map silently assumed `out[0]` and would mis-value every child but the first.
    //   * values are read from the PARSED transactions, never from the declared `out_value` field, which
    //     is attacker-supplied. The tx is the authority; there is no reason to consult the claim.
    let mut prevout_value_of: std::collections::HashMap<(Txid, u32), u64> = std::collections::HashMap::new();
    prevout_value_of.insert(
        (Txid::from_str(&bundle.f_txid).map_err(|_| anyhow::anyhow!("bad F txid"))?, bundle.f_vout),
        bundle.f_value,
    );
    for tx in txs.iter() {
        let id = tx.txid();
        for (vout, o) in tx.output.iter().enumerate() {
            prevout_value_of.insert((id, vout as u32), o.value);
        }
    }
    // Every counted tier (exit chain AND superseded) must verify as a genuine co-sign by A.
    for (i, tx) in txs.iter().enumerate() {
        let op = tx.input[0].previous_output;
        let value = *prevout_value_of
            .get(&(op.txid, op.vout))
            .ok_or_else(|| anyhow::anyhow!("tier {i} spends an outpoint outside this ladder"))?;
        verify_tier_cosigned(tx, value, &agg_spk)
            .map_err(|e| anyhow::anyhow!("exit tier {i} is not co-signed by A: {e}"))?;
    }

    // PER-PREVOUT race map [S-1/S-2]: for each outpoint the exit chain spends, the CSV of the LIVE tier
    // that spends it. A superseded tier only ever contends with the exit tier consuming the SAME
    // outpoint, so that — not a global `final_csv` — is what it must be compared against.
    //   S-2: comparing every superseded entry to `txs.last()`'s CSV is meaningless across unrelated
    //        outpoints (post-rollover a level-0 superseded state was compared to the level-1 final
    //        state — they never contend), and it also bricks honest renew→transfer sequences.
    //   S-1: the old check was additionally gated on `kind == "state"`, leaving superseded EXTENSIONS
    //        race-UNCHECKED. A genuinely co-signed X_evil at e_floor + its child S_evil both verify
    //        against A and balance the count, but X_evil matures far ahead of the honest extension and
    //        its state pays the attacker — outright theft. Extensions are now checked identically.
    // Keyed per-OUTPUT for the same reason as `prevout_value_of`: a split state hosts a child on each
    // `out[j]`, so "the live tier over this outpoint" is only well-defined per (txid, vout).
    let mut live_csv_by_outpoint: std::collections::HashMap<(Txid, u32), u32> =
        std::collections::HashMap::new();
    for i in 1..txs.len() {
        let op = txs[i].input[0].previous_output;
        live_csv_by_outpoint.insert((op.txid, op.vout), txs[i].input[0].sequence.0 & 0xFFFF);
    }
    // [C-2] Every LIVE exit tier's txid, so a superseded list cannot re-declare one of them (or repeat
    // itself) to buy an extra census slot.
    let live_txids: std::collections::HashSet<Txid> = txs.iter().map(|t| t.txid()).collect();
    if live_txids.len() != txs.len() {
        return Err(anyhow::anyhow!(
            "the exit chain repeats a tier txid — one co-sign counted twice"
        ));
    }
    let superseded_ok = verify_superseded_segment(
        &bundle.superseded_states,
        &bundle.superseded_extensions,
        &agg_spk,
        &p,
        &mut prevout_value_of,
        &live_csv_by_outpoint,
        &live_txids,
    )?;

    // 3. The linchpin: the SE's finalized-signature count must EXACTLY account for EVERY co-signed
    //    tier — the exit chain PLUS the superseded states/extensions (full-disclosure counting). Every
    //    term below is now a VERIFIED co-sign of this ladder (above), so the count cannot be padded.
    //    A hidden co-signed state bumps num_sigs without appearing here ⟹ reject.
    let expected = flat_backups + tiers.len() as u32 + superseded_ok;
    if se_num_sigs != expected {
        return Err(anyhow::anyhow!(
            "num_sigs mismatch: SE issued {se_num_sigs}, disclosed tiers+backups account for {expected} — possible hidden state"
        ));
    }
    Ok(())
}

/// **[CTES-R] The structural half of colour, enforced with no RGB engine and no network.**
///
/// `verify_bundle_bound` binds a ladder's SATS to the coin. It is entirely colour-blind, and it must
/// stay that way — so this is what stops the two ways a conveyed bundle can lie about colour, both of
/// which are silent at every other check:
///
/// * **claiming colour it does not carry** — a bundle with an `rgb` half whose tiers carry no opret,
///   or whose consignment list does not line up one-for-one with `exit_tiers()`. The receiver would
///   derive seals for tiers that commit to nothing and book an allocation that no transition moves.
/// * **carrying colour it does not claim** — a bundle with `rgb: None` whose tiers DO carry oprets.
///   That is a coloured ladder conveyed as plain: the receiver binds the sats, runs no consignment
///   validation at all, and the asset half is simply unaccounted for.
///
/// It also pins the single-level invariant that [`TesrBundle::colored_tier_seals`] depends on, so a
/// bundle whose seals cannot be derived is rejected by the census rather than at accept time.
///
/// Deliberately NOT checked here: that the consignments are valid RGB, or that they assign anything
/// to anyone. That needs the engine and belongs to the SDK's token hook — this is the gate that runs
/// everywhere, including in the SSP's pre-payment census where there is no wallet at all.
fn verify_colored_shape(bundle: &TesrBundle) -> Result<()> {
    use electrum_client::bitcoin::{consensus::deserialize, Transaction};
    let tiers = bundle.exit_tiers();
    let mut opret_count = 0usize;
    for (i, tier) in tiers.iter().enumerate() {
        let raw = hex::decode(&tier.signed_tx)
            .map_err(|_| anyhow::anyhow!("tier {i}: hex does not decode"))?;
        let tx: Transaction =
            deserialize(&raw).map_err(|_| anyhow::anyhow!("tier {i}: tx does not parse"))?;
        let oprets: Vec<usize> = tx
            .output
            .iter()
            .enumerate()
            .filter(|(_, o)| o.script_pubkey.is_op_return())
            .map(|(v, _)| v)
            .collect();
        match (bundle.is_colored(), oprets.len()) {
            (true, 1) => {
                if oprets[0] as u32 == tier.payload_vout {
                    return Err(anyhow::anyhow!(
                        "coloured tier {i} declares its payload at vout {} — that output is the RGB \
                         opret commitment, which carries no value and cannot be spent",
                        tier.payload_vout
                    ));
                }
                opret_count += 1;
            }
            (true, n) => {
                return Err(anyhow::anyhow!(
                    "coloured tier {i} carries {n} OP_RETURN outputs, expected exactly 1 (the RGB \
                     opret commitment)"
                ))
            }
            (false, 0) => {}
            (false, n) => {
                return Err(anyhow::anyhow!(
                    "tier {i} carries {n} OP_RETURN output(s) but this ladder is conveyed as PLAIN \
                     — a coloured ladder passed off as plain would have its asset half validated by \
                     nobody"
                ))
            }
        }
    }
    let Some(rgb) = bundle.rgb.as_ref() else {
        return Ok(());
    };
    if opret_count != tiers.len() {
        return Err(anyhow::anyhow!("coloured ladder: not every tier carries an opret"));
    }
    if bundle.levels.len() != 1 {
        return Err(anyhow::anyhow!(
            "coloured ladder has {} levels — coloured rollover does not exist, and a multi-level \
             coloured bundle has no derivable seal schedule",
            bundle.levels.len()
        ));
    }
    if rgb.contract_id.trim().is_empty() {
        return Err(anyhow::anyhow!("coloured ladder names no contract"));
    }
    if rgb.amount == 0 {
        return Err(anyhow::anyhow!("coloured ladder carries a zero allocation"));
    }
    if rgb.consignments.len() != tiers.len() {
        return Err(anyhow::anyhow!(
            "coloured ladder carries {} consignments for {} tiers — they are indexed by exit order, \
             so a mismatch means the receiver cannot tell which proof belongs to which tier",
            rgb.consignments.len(),
            tiers.len()
        ));
    }
    if rgb.consignments.iter().any(|c| c.trim().is_empty()) {
        return Err(anyhow::anyhow!("coloured ladder carries an empty consignment"));
    }
    // The seals must be derivable, since that is what the receiver will do.
    bundle.colored_tier_seals()?;
    Ok(())
}

/// [in-ladder split] The x-only taproot key (hex) of a v1 taproot scriptPubKey, or an error if `spk`
/// is not `OP_1 <32-byte push>`.
fn taproot_key_hex(spk: &[u8]) -> Result<String> {
    if spk.len() != 34 || spk[0] != 0x51 || spk[1] != 0x20 {
        return Err(anyhow::anyhow!("not a v1 taproot scriptPubKey"));
    }
    Ok(hex::encode(&spk[2..34]).to_lowercase())
}

/// [in-ladder split] Verify a split child's exit bundle — the 8-check Stage-2 predicate (ruling
/// wqvoxvusg). Proves the child is safe from a hidden parent state rivalling `SP` over `X_m.out[0]`
/// with NO SGX: soundness rests on the on-chain root of `A_parent`, the authoritative server aggregate
/// records (Stage 1, UNIQUE), the exact-equality censuses, and coordinator-enforced terminality.
///
/// PURE + unit-testable: all authoritative values are passed in (the caller fetches them from chain +
/// `/info/statechain`). `parent_f_onchain_spk_hex` and `parent_f_onchain_value` are
/// `F.output[f_vout].{script_pubkey, value}` read from the chain (the caller having confirmed `F`
/// unspent+confirmed at `cb.parent.f_txid/f_vout`); the `*_aggregate_pubkey` are the server's
/// recorded aggregates (None ⟹ fail-closed).
///
/// `parent_f_onchain_value` is REQUIRED rather than read off `cb.parent.f_value`, and the difference
/// is the whole point. `37d8bba` anchored the parent's trigger to the chain inside
/// `verify_conveyed_child` — the async wrapper — which left every caller that reaches this verifier
/// directly measuring a value chain against a value the sender declared. This function's own contract
/// is that authoritative numbers arrive as parameters; `F`'s value is one of those numbers, so it
/// arrives as one, and the type system now refuses to let a caller forget it.
///
/// ⚠️ DORMANT + UNREVIEWED: nothing calls this for a live split yet (HF-1 still refuses to split a
/// laddered coin). It must pass the split E2E + an adversarial test suite + an independent review before
/// HF-1 is removed. Conservative for now: a child that has been renewed/transferred (non-empty child
/// superseded sets) is REJECTED rather than under-validated — that path is future work.
pub fn verify_child_bundle(
    cb: &ChildTesrBundle,
    parent_f_onchain_spk_hex: &str,
    parent_f_onchain_value: u64,
    parent_num_sigs: u32,
    parent_flat_backups: u32,
    parent_aggregate_pubkey: Option<&str>,
    parent_terminal: bool,
    child_num_sigs: u32,
    child_flat_backups: u32,
    child_aggregate_pubkey: Option<&str>,
    ancestor_facts: &[AncestorFacts],
    receiver_backup_address: &str,
) -> Result<()> {
    // [F2] The two segments are secured DIFFERENTLY, because only one of them is being handed over.
    //
    // PARENT (ancestor segment) — terminality is load-bearing and still REQUIRED. Its census is a
    // snapshot nobody in this transfer can refresh: the receiver never takes ownership of the parent,
    // so a sender who skipped `set_spend_budget` could pass the count here and then have the SE
    // co-sign a rival trigger T' over the live on-chain F afterwards. Terminal ⟹ the SE refuses
    // every further co-sign ⟹ the snapshot is durable. Fail-closed.
    //
    // CHILD (leaf segment) — terminality is NOT required, and requiring it would make the child
    // exit-only forever. Its census is made durable by a different, stronger mechanism: the receiver
    // COMPLETES the key handover during this same claim, which rotates the SE share and the auth key,
    // locking the sender out PERMANENTLY. The coordinator's pending-transfer lock (armed when
    // `convey_child_bundle` opened the transfer) covers the gap between this census and that
    // completion. See CHILDREN.md. NOTE the verifier must TOLERATE a terminal child — the
    // Lightning-latched lane deliberately keeps one — it simply no longer checks.
    if !parent_terminal {
        return Err(anyhow::anyhow!(
            "parent sid is NOT terminal — a rival state over F/X_m.out[0] could still be co-signed (fail-closed)"
        ));
    }
    use electrum_client::bitcoin::{consensus::deserialize, Address, Transaction};
    let net = net_from_str(&cb.parent.network);

    // The server records the UNTWEAKED aggregate x-only; an on-chain scriptPubKey commits to the
    // BIP-341-TWEAKED output key. So to compare a recorded aggregate to a key read from a spk, tweak the
    // recorded aggregate first (P2TR with no script tree) and take the resulting output key.
    let tweaked_key_hex = |agg_xonly_hex: &str| -> Result<String> {
        // Shared with `verify_bundle_bound` so the two acceptance paths can never drift.
        tweaked_p2tr_key_hex(agg_xonly_hex, net)
    };

    // [1] ON-CHAIN ROOT: A_parent := taproot key of the fetched on-chain F.spk. Bind the parent
    //     segment's declared aggregate to it — the co-sign key is anchored to the on-chain funding,
    //     not a sender field. (This closes the "sender picks a decoy F" path together with [2].)
    let f_spk = hex::decode(parent_f_onchain_spk_hex).map_err(|_| anyhow::anyhow!("bad F spk hex"))?;
    let a_parent = taproot_key_hex(&f_spk)?;
    let parent_agg_spk = Address::from_str(&cb.parent.agg_address)
        .map_err(|_| anyhow::anyhow!("bad parent agg_address"))?
        .require_network(net)
        .map_err(|_| anyhow::anyhow!("parent agg_address wrong network"))?
        .script_pubkey();
    if taproot_key_hex(parent_agg_spk.as_bytes())? != a_parent {
        return Err(anyhow::anyhow!("parent agg_address does not match the on-chain F aggregate"));
    }

    // [1b] ON-CHAIN VALUE: the parent segment's declared `f_value` must be the value the funding
    //      output actually holds. Builders hand that number to `cosign_tier` as the trigger's prevout
    //      amount, and `verify_bundle_ex` seeds its sighash map with it, so a restated `f_value` and a
    //      trigger co-signed against the restatement agree with each other perfectly. Only the chain
    //      breaks the tie. (This was `verify_conveyed_child`'s, and is now here, so that a caller of
    //      the synchronous verifier inherits it rather than having to remember it.)
    if cb.parent.f_value != parent_f_onchain_value {
        return Err(anyhow::anyhow!(
            "the bundle declares f_value {} but the on-chain funding output holds {}",
            cb.parent.f_value,
            parent_f_onchain_value
        ));
    }

    // [2] PARENT AGGREGATE AUTHORITY: the server's recorded aggregate for parent_sid must be non-NULL
    //     (fail-closed) and == A_parent. UNIQUE(aggregate_xonly) ⟹ only the REAL parent sid can hold
    //     A_parent, so a decoy parent_sid (whose counter a sender could pump) can never match here.
    let p_agg = parent_aggregate_pubkey
        .ok_or_else(|| anyhow::anyhow!("server recorded no aggregate for parent sid (fail-closed)"))?;
    if tweaked_key_hex(p_agg)? != a_parent {
        return Err(anyhow::anyhow!("parent sid's server aggregate != A_parent (decoy parent)"));
    }

    // [3]+[4] PARENT SEGMENT + CENSUS: verify_bundle over the parent proves every parent tier is
    //     co-signed under A_parent(=agg_address, now bound to on-chain F), SP is the current state,
    //     S_0 is disclosed as superseded and OUT-RACED by SP over X_m.out[0], and the exact-equality
    //     census holds (num_sigs(parent_sid) accounts for exactly the disclosed tiers).
    //     …and the parent's TRIGGER is bound to the chain's `F` value, so the whole tier chain below
    //     it — parent, ancestors, leaf — conserves against a number Bitcoin agreed to rather than one
    //     the sender picked.
    verify_bundle_ex(&cb.parent, parent_num_sigs, parent_flat_backups, true, Some(parent_f_onchain_value))
        .map_err(|e| anyhow::anyhow!("parent segment/census invalid: {e}"))?;

    // Parse SP (the parent's current, terminal state) — it hosts the children on its outputs.
    let sp_tx: Transaction = deserialize(
        &hex::decode(&cb.parent.current().state.signed_tx).map_err(|_| anyhow::anyhow!("bad SP hex"))?,
    )
    .map_err(|_| anyhow::anyhow!("SP is not a transaction"))?;

    // [4b] INTERMEDIATE CHILD SEGMENTS (a child that descends from another child). Walk them root→leaf,
    //      advancing the funding pointer; the leaf checks below then run unchanged at any depth.
    //
    //      An ancestor segment is NOT being handed over in this transfer, so — exactly like the parent —
    //      its census is a snapshot the receiver cannot refresh, and terminality is what makes it
    //      durable. Fail closed on any mismatch of supplied facts.
    if ancestor_facts.len() != cb.ancestors.len() {
        return Err(anyhow::anyhow!(
            "ancestor facts ({}) do not match disclosed ancestor segments ({}) — fail-closed",
            ancestor_facts.len(),
            cb.ancestors.len()
        ));
    }
    let mut cur_tx = sp_tx.clone();
    for (i, seg) in cb.ancestors.iter().enumerate() {
        let facts = &ancestor_facts[i];
        let fund_out = cur_tx
            .output
            .get(seg.funding_vout as usize)
            .ok_or_else(|| anyhow::anyhow!("ancestor {i}: funding tx has no output {}", seg.funding_vout))?
            .clone();
        let fund_txid = cur_tx.txid();
        let seg_spk = fund_out.script_pubkey.clone();
        // The segment's aggregate is KEY-DERIVED from the output it spends, then bound to the server's
        // recorded aggregate for its statechain id.
        let a_seg = taproot_key_hex(seg_spk.as_bytes())?;
        let seg_agg = facts
            .aggregate_pubkey
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("ancestor {i}: server recorded no aggregate (fail-closed)"))?;
        if tweaked_key_hex(seg_agg)? != a_seg {
            return Err(anyhow::anyhow!("ancestor {i}: server aggregate != its funding output key (decoy)"));
        }
        if !facts.terminal {
            return Err(anyhow::anyhow!(
                "ancestor {i} is NOT terminal — a rival state over its funding outpoint could still be co-signed (fail-closed)"
            ));
        }
        // ═══ [CATS/V1] SEGMENT SHAPE IS **DERIVED**, NOT DECLARED ═══════════════════════════════
        //
        // `seg.extension` becoming an `Option` is the first time a segment's SHAPE is expressible by
        // the sender. The obvious defence — "the exact-equality census closes it, because a dropped
        // tier leaves `expected` one short of `num_sigs`" — is FALSE, and the reason is worth stating
        // where the code is rather than in a design doc:
        //
        //   A dropped tier is not lost. The sender re-declares it as superseded, where
        //   `verify_superseded_segment` counts it (it returns `sups.len()`), and `expected` moves by
        //   exactly the same 1 in the opposite direction:
        //       CHILD_V2_BASELINE + 1 (one tier) + 1 (one superseded)
        //    == CHILD_V2_BASELINE + 2 (two tiers) + 0
        //   Every co-sign is real, every signature verifies, the census balances EXACTLY.
        //
        // Until now that attack was blocked by something this change removes: `live_ids` held BOTH
        // tier txids, so the [C-2] dedup refused any attempt to ALSO disclose the extension as
        // superseded. The `None` branch takes the extension out of `live_ids` and un-blocks it.
        //
        // What closes it instead, deliberately rather than by inheritance:
        //
        //  (1) THE PREVOUT RE-ANCHOR (below, on the state's input). A segment declared as a spine must
        //      have its lone tier spend the segment's OWN FUNDING OUTPOINT. A genuine two-tier
        //      segment's state spends `ext.out[payload_vout]`, so it cannot be re-labelled. The
        //      outpoint is committed by the taproot SIGHASH_ALL sighash, so it is derived from the
        //      SE's signature and cannot be repointed without invalidating it — the `Option` is a
        //      cross-checked declaration that must AGREE, never the source of truth
        //      (`docs/utexo/ADMISSION-INPUTS.md`: a serde field is not provenance).
        //  (2) THE `[0,0]` CSV PIN stays exactly as it is. Note that `[e_floor,e0] = [144,720]` is a
        //      strict SUBSET of `[d_floor,d0] = [144,1440]`, so extension-vs-state was NEVER
        //      CSV-separable; only the spine's `[0,0]` is disjoint from both. Widening the lone
        //      tier's bound "because it might be either kind" would destroy the last structural
        //      layer and let a real extension be passed off as the lone tier.
        //  (3) THE DEAD KNOB, immediately below.
        //
        // Without (1) the concrete consequence is [P0-1] re-opened through a new door: a real
        // `[ext 720, state 1440]` segment declared as a spine loses 721 blocks from the exit chain
        // `check_exit_headroom` reads, and a child near the epoch boundary is admitted whose real
        // exit cannot finish.
        //
        // (3) THE DEAD KNOB — free, independent of (1), and it closes the re-declaration route
        // head-on. A spine segment has no extension rung, so `superseded_extensions` has no honest
        // writer on one. Structural, so it runs before anything reads a value.
        if seg.extension.is_none() && !seg.superseded_extensions.is_empty() {
            return Err(anyhow::anyhow!(
                "ancestor {i}: extension is absent but {} superseded extension(s) are disclosed — a \
                 spine segment has no extension rung to supersede",
                seg.superseded_extensions.len()
            ));
        }
        // ext (when present) spends the funding outpoint; state spends ext.out[0], or — on a spine
        // segment — the funding outpoint itself. Every tier co-signed by A_seg.
        let mut ext_parsed: Option<(Transaction, u32, u64)> = None; // (tx, payload_vout, payload value)
        if let Some(ext) = &seg.extension {
            let ext_tx: Transaction = deserialize(
                &hex::decode(&ext.signed_tx).map_err(|_| anyhow::anyhow!("ancestor {i}: bad ext hex"))?,
            )
            .map_err(|_| anyhow::anyhow!("ancestor {i}: extension is not a transaction"))?;
            let ein = ext_tx.input.first().ok_or_else(|| anyhow::anyhow!("ancestor {i}: ext has no input"))?;
            if ext_tx.input.len() != 1
                || ein.previous_output.txid != fund_txid
                || ein.previous_output.vout != seg.funding_vout
            {
                return Err(anyhow::anyhow!("ancestor {i}: extension does not spend its funding outpoint"));
            }
            verify_tier_cosigned(&ext_tx, fund_out.value, &seg_spk)
                .map_err(|e| anyhow::anyhow!("ancestor {i}: extension not co-signed by its aggregate: {e}"))?;
            // WHERE IT PAYS. A co-sign proves what a tier SPENDS, never what it PAYS — and this
            // segment's state is about to have its own co-sign verified against a prevout SYNTHESISED
            // as `TxOut { value: ext0.value, script_pubkey: seg_spk }`. If the real `ext0` pays
            // someone else's key, that signature is valid for a prevout that does not exist: the
            // state is unbroadcastable forever, and whoever owns the real key sweeps the segment the
            // moment the extension confirms. The receiver, meanwhile, has been credited the full
            // funding value and — this ancestor being terminal — can never obtain a replacement.
            // `seg_spk` is a `ScriptBuf`, so compare it as one. An earlier version of this line wrote
            // `hex::decode(&seg_spk).unwrap_or_default()` — which compiles, because `hex::decode`
            // takes `AsRef<[u8]>` and a script derefs to its bytes, but those bytes are not ASCII
            // hex, so the decode always failed and `unwrap_or_default()` turned the failure into an
            // EMPTY vector that matches nothing. The check then refused every honest ancestor bundle
            // (sdk11 and sdk17 caught it). Precisely the swallow shape the repo's own guard exists
            // for, written into a security check by the person who keeps citing that guard.
            //
            // This is PIN 2, and it is what licenses reading `ext0.value()` below — the ancestor
            // extension's payload output is not linked to its state until further down, so the payee
            // is the structural fact this segment's value chain rests on. Expressed as a `link_pays`
            // so the value cannot be reached without it.
            let ext0_value = {
                let ext0 = ext.link_pays(
                    &ext_tx,
                    &seg_spk,
                    &format!("ancestor {i} extension"),
                    &format!(
                        "ancestor {i}: the extension's payload output does not pay the segment's \
                         aggregate key — every tier below it would be signed against a prevout that \
                         does not exist"
                    ),
                )?;
                ext0.value()
            };
            // HOW MUCH IT FORWARDS, and THAT NOTHING ELSE LEAVES. Summed over every non-anchor,
            // non-opret output rather than read from `payload_vout`, for the reason spelled out on the
            // root ladder's law: checking one output leaves a window exactly one committed fee wide.
            {
                let expect = mercurylib::tesr::tier_out_value(fund_out.value, cb.parent.fee_rate)
                    .ok_or_else(|| anyhow::anyhow!(
                        "ancestor {i} extension: funding of {} sat cannot carry a tier at {} sat/vB",
                        fund_out.value, cb.parent.fee_rate
                    ))?;
                let got: u64 = ext_tx
                    .output
                    .iter()
                    .filter(|o| {
                        o.script_pubkey.as_bytes() != mercurylib::tesr::P2A_SCRIPT_BYTES
                            && !o.script_pubkey.is_op_return()
                    })
                    .map(|o| o.value)
                    .sum();
                if got != expect {
                    return Err(anyhow::anyhow!(
                        "ancestor {i} extension is funded with {} sat but its payload outputs carry {got} \
                         (expected exactly {expect}) — the difference would leave the exit chain",
                        fund_out.value
                    ));
                }
            }
            ext_parsed = Some((ext_tx, ext.payload_vout, ext0_value));
        }
        // THE OUTPOINT THE LONE/FINAL TIER MUST RE-ANCHOR ON, and the prevout amount its co-sign is
        // checked against. Two-tier: the extension's payload output, whose payee and value were just
        // pinned. Spine: the segment's own funding outpoint, read off the PARSED funding transaction —
        // chain-structural in both cases, never a declared number.
        let (st_prev, st_prev_value) = match &ext_parsed {
            Some((ext_tx, payload_vout, payload_value)) => {
                ((ext_tx.txid(), *payload_vout), *payload_value)
            }
            None => ((fund_txid, seg.funding_vout), fund_out.value),
        };
        let st_tx: Transaction = deserialize(
            &hex::decode(&seg.state.signed_tx).map_err(|_| anyhow::anyhow!("ancestor {i}: bad state hex"))?,
        )
        .map_err(|_| anyhow::anyhow!("ancestor {i}: state is not a transaction"))?;
        let sin = st_tx.input.first().ok_or_else(|| anyhow::anyhow!("ancestor {i}: state has no input"))?;
        if st_tx.input.len() != 1
            || sin.previous_output.txid != st_prev.0
            || sin.previous_output.vout != st_prev.1
        {
            // (1) THE LOAD-BEARING CHECK. In the `None` arm this IS the shape derivation: a two-tier
            // segment re-labelled as a spine fails here, because its state spends its extension's
            // payload output and not the funding outpoint — and that input is committed by the
            // taproot SIGHASH_ALL sighash `verify_tier_cosigned` verifies, so it cannot be repointed
            // without invalidating the SE's own signature.
            return Err(anyhow::anyhow!(
                "ancestor {i}: {}",
                if seg.extension.is_some() {
                    "state does not spend its extension's payload output"
                } else {
                    "the lone tier does not spend the segment's funding outpoint — a segment declared \
                     as a spine must re-anchor on its own funding outpoint"
                }
            ));
        }
        verify_tier_cosigned(&st_tx, st_prev_value, &seg_spk)
            .map_err(|e| anyhow::anyhow!("ancestor {i}: state not co-signed by its aggregate: {e}"))?;
        // CSV bounds for every tier this segment actually has — and [B1] the declared field bound to
        // the signed one, so an intermediate segment cannot understate the depth cost of the chain it
        // sits in.
        let mut csv_checks: Vec<(&str, &Transaction, Option<u16>)> = Vec::with_capacity(2);
        if let (Some(ext), Some((ext_tx, _, _))) = (&seg.extension, &ext_parsed) {
            csv_checks.push(("extension", ext_tx, ext.csv));
        }
        csv_checks.push(("state", &st_tx, seg.state.csv));
        for (kind, tx, declared) in csv_checks {
            let seq = tx.input[0].sequence.0;
            if seq & (1 << 31) != 0 || seq & (1 << 22) != 0 {
                return Err(anyhow::anyhow!("ancestor {i} {kind}: not a BIP-68 block relative-timelock"));
            }
            let csv = seq as u16;
            let p = cb.parent.params;
            // [CATS] An intermediate segment's `state` IS that level's split state — the tier whose
            // outputs fund the level below — so it is a SPINE tier and carries `SPINE_CSV`, not a
            // schedule state. That holds for a spine segment's LONE tier too: an ancestor spine
            // segment is by definition one that has already been split, so its live tier is the next
            // batch's `SP_{i+1}`, not the resting cap `C_i` (which is disclosed as superseded and
            // bounded `[d_floor, d0]` by the superseded battery).
            //
            // `kind` is a literal pushed by this function four lines up — structural, and not
            // something the bundle can influence. And the bound must NOT be widened to
            // `[d_floor, d0]` "because the lone tier could be either kind": `[e_floor,e0]` is a
            // strict SUBSET of `[d_floor,d0]`, so `[0,0]` is the ONLY interval that separates a spine
            // tier from an extension, and it is the last structural layer left once shape becomes
            // declarable.
            let (lo, hi) = if kind == "extension" {
                (p.e_floor, p.e0)
            } else {
                (SPINE_CSV, SPINE_CSV)
            };
            if csv < lo || csv > hi {
                let what = if kind == "extension" { "extension" } else { "SPINE state" };
                return Err(anyhow::anyhow!(
                    "ancestor {i} {what}: CSV {csv} outside [{lo},{hi}]"
                ));
            }
            mercurylib::transfer::receiver::bind_declared_csv(
                i,
                &format!("ancestor {i} {kind}"),
                declared,
                Some(csv),
            )?;
        }
        // Superseded battery + exact-equality census for this segment (same shared logic as everywhere).
        let seg_superseded_ok = {
            let mut prevouts: std::collections::HashMap<(electrum_client::bitcoin::Txid, u32), u64> = std::collections::HashMap::new();
            prevouts.insert((fund_txid, seg.funding_vout), fund_out.value);
            for tx in ext_parsed.iter().map(|(t, _, _)| t).chain(std::iter::once(&st_tx)) {
                let id = tx.txid();
                for (v, o) in tx.output.iter().enumerate() {
                    prevouts.insert((id, v as u32), o.value);
                }
            }
            let mut live: std::collections::HashMap<(electrum_client::bitcoin::Txid, u32), u32> = std::collections::HashMap::new();
            // The census key MUST move with the payload vout it describes (CTESR-GATE §3.2): this map
            // is KEYED rather than content-checked, so a key that disagreed with its tier would make
            // the superseded battery compare a rival against the WRONG live CSV — the one migration
            // site that warrants explicit care. `st_tx` is asserted above to spend exactly `st_prev`,
            // so key and tier can never drift.
            //
            // ⚠️ [CATS] THE FIRST INSERT IS UNCONDITIONAL, and the two inserts are NOT symmetric.
            // This one is the FUNDING-outpoint race key — the outpoint over which the sender's
            // retained cap `C_i` is out-raced by the live tier — and it is what makes every honest
            // segment's superseded disclosure resolve by DIRECT CONTENTION. Wrapping BOTH inserts in
            // the `if let Some(ext)` (the obvious symmetry) deletes that key on a spine segment, so
            // `C_i` roots in no live contention, is classified an orphan/threat branch, and EVERY
            // honest CATS bundle is refused. Only the SECOND — the extension→state hop, which a spine
            // segment does not have — may become conditional.
            //
            // The nSequence it carries is the LONE tier's on a spine segment and the extension's on a
            // two-tier one: in both cases, the tier that actually spends the funding outpoint.
            let first_over_funding = ext_parsed.as_ref().map(|(t, _, _)| t).unwrap_or(&st_tx);
            live.insert(
                (fund_txid, seg.funding_vout),
                first_over_funding.input[0].sequence.0 & 0xFFFF,
            );
            let mut live_ids: std::collections::HashSet<electrum_client::bitcoin::Txid> =
                std::collections::HashSet::from([st_tx.txid()]);
            if let Some((ext_tx, payload_vout, _)) = &ext_parsed {
                live.insert(
                    (ext_tx.txid(), *payload_vout),
                    st_tx.input[0].sequence.0 & 0xFFFF,
                );
                live_ids.insert(ext_tx.txid());
            }
            verify_superseded_segment(
                &seg.superseded_states,
                &seg.superseded_extensions,
                &seg_spk,
                &cb.parent.params,
                &mut prevouts,
                &live,
                &live_ids,
            )
            .map_err(|e| anyhow::anyhow!("ancestor {i}: {e}"))?
        };
        // [V2] The tier term is DERIVED from the tiers actually disclosed, never the literal `2`.
        // V1 and V2 are one commit on purpose: a one-tier bundle measured against a hard-coded `2`
        // leaves a FREE CENSUS SLOT — one co-sign the SE issued that nothing in the bundle accounts
        // for, i.e. a hidden rival state — and that mismatch fails OPEN.
        //
        // The four shapes this must cover, all exact:
        //   spine at rest (never reaches here — it is the tip's own record, not an ancestor)  0+1+0=1
        //   spine after the next batch: live `SP_{i+1}`, superseded `C_i`                     0+1+1=2
        //   two-tier legacy segment (`child_in_ladder_split`): ext+CSP live, old state sup     0+2+1=3
        //   a tip handed over once, then split: live `SP`, superseded `C`,`C'`                 0+1+2=3
        let seg_tiers = 1 + u32::from(seg.extension.is_some());
        let expected = CHILD_V2_BASELINE + seg_tiers + seg_superseded_ok;
        if facts.num_sigs != expected {
            return Err(anyhow::anyhow!(
                "ancestor {i} num_sigs mismatch: SE issued {}, disclosed accounts for {expected} — possible hidden state",
                facts.num_sigs
            ));
        }
        cur_tx = st_tx;
    }

    let sp_txid = cur_tx.txid();
    // The funding tier's own payload vout — `SP.out[j]` means the j-th PAYLOAD output, so a child slot
    // can never sit before it (index 0 becomes the opret once a tier is coloured).
    let funding_payload_vout = cb
        .ancestors
        .last()
        .map(|a| a.state.payload_vout)
        .unwrap_or(cb.parent.current().state.payload_vout);
    if cb.sp_vout < funding_payload_vout {
        return Err(anyhow::anyhow!(
            "child funding vout {} precedes the funding tier's payload vout {funding_payload_vout}",
            cb.sp_vout
        ));
    }
    let sp_out = cur_tx
        .output
        .get(cb.sp_vout as usize)
        .ok_or_else(|| anyhow::anyhow!("funding tx has no output {}", cb.sp_vout))?;

    // [5] CHILD AGGREGATE AUTHORITY: A_child := SP.out[j].spk (parsed from SP, not declared). The
    //     server's recorded aggregate for child_sid must be non-NULL and == A_child. SP is un-broadcast
    //     so A_child has no on-chain root — its authority is the UNIQUE server registration (child_sid
    //     must be server-created at split time, not sender-chosen).
    let a_child = taproot_key_hex(sp_out.script_pubkey.as_bytes())?;
    let c_agg = child_aggregate_pubkey
        .ok_or_else(|| anyhow::anyhow!("server recorded no aggregate for child sid (fail-closed)"))?;
    if tweaked_key_hex(c_agg)? != a_child {
        return Err(anyhow::anyhow!("child sid's server aggregate != SP.out[j] key (decoy child)"));
    }

    // [6] CHILD SEGMENT + CENSUS, verified under A_child (attribution is KEY-DERIVED — check [7] — since
    //     each tier is verified against SP.out[j]'s key, not a sender-filled segment field).
    let child_agg_spk = sp_out.script_pubkey.clone();

    // ext_child spends exactly SP.out[j], co-signed by A_child.
    let ext_tx: Transaction = deserialize(
        &hex::decode(&cb.child_extension.signed_tx).map_err(|_| anyhow::anyhow!("bad child ext hex"))?,
    )
    .map_err(|_| anyhow::anyhow!("child extension is not a transaction"))?;
    let ext_in = ext_tx.input.first().ok_or_else(|| anyhow::anyhow!("child ext has no input"))?;
    if ext_tx.input.len() != 1 || ext_in.previous_output.txid != sp_txid || ext_in.previous_output.vout != cb.sp_vout {
        return Err(anyhow::anyhow!("child extension does not spend SP.out[j]"));
    }
    verify_tier_cosigned(&ext_tx, sp_out.value, &child_agg_spk)
        .map_err(|e| anyhow::anyhow!("child extension not co-signed by A_child: {e}"))?;
    // [F4] child extension CSV: a valid BIP-68 block relative-timelock within the extension schedule.
    {
        let seq = ext_tx.input[0].sequence.0;
        if seq & (1 << 31) != 0 || seq & (1 << 22) != 0 {
            return Err(anyhow::anyhow!("child extension: not a BIP-68 block relative-timelock"));
        }
        let (csv, p) = (seq as u16, cb.parent.params);
        if csv < p.e_floor || csv > p.e0 {
            return Err(anyhow::anyhow!("child extension CSV {csv} outside [{},{}]", p.e_floor, p.e0));
        }
        // [B1] the declared field is the signed one, or the bundle is refused.
        mercurylib::transfer::receiver::bind_declared_csv(
            0,
            "child extension",
            cb.child_extension.csv,
            Some(csv),
        )?;
    }

    // ═══ [VALUE-CONSERVATION] EVERY TIER MUST FORWARD ITS FUNDING MINUS EXACTLY ONE RUNG ═══
    //
    // THE DEFECT this closes, demonstrated against this very function rather than argued: a sender
    // built an honest parent, an honest `SP` paying the payee's slot 198 530 sat, and then a
    // `child_extension` that spends that output but pays only 1 000 sat forward — the remaining
    // 196 690 going to a SECOND output back to the sender. `child_state` then paid the payee 510 sat
    // and declared `out_value: 510` truthfully. `verify_child_bundle` returned `Ok(())`.
    //
    // Every existing check passed, and each for a good reason:
    //   * `verify_tier_cosigned` binds a co-sign to the tier's INPUT amount, never to how the tier
    //     splits it across outputs — and the SE is BLIND, so it co-signs any distribution by design.
    //   * The `[value-gate spoof]` check below binds `child_state.out_value` to the signed tx, but
    //     the attacker declares it HONESTLY; 510 really is all the payee can reach.
    //   * A tier legitimately has more than one output (payload + the P2A anchor), so "extra output"
    //     is not itself suspicious.
    //
    // What made it theft is that the receiver books the FUNDING value: `coin.amount =
    // sp_out.value` (`transfer_receiver.rs:1321`), and `verify_conveyed_child`'s returned exit value
    // is discarded at the claim site (`:979`). So the payee is credited 198 530 for a coin worth 510.
    //
    // ⚠️ THIS CHECK WAS ORIGINALLY SCOPED TO THE CHILD LANE ON REASONING THAT WAS WRONG. The
    // argument was: a whole coin retains its flat backup chain, which
    // `verify_if_locktime_is_reasonable_tx_version_and_output_size` constrains to exactly one
    // spendable output paying the owner, so a skimming ROOT ladder only degrades the fast path.
    //
    // **[B1] destroys that fallback.** `T` is un-timelocked and spends `F`, and every prior owner
    // retains a co-signed copy. The moment one of them broadcasts `T`, `F` is spent and every flat
    // backup — all of which spend `F` — is void. The slow path is not slower, it is gone, at the
    // choosing of the same party who built the skimming ladder. The root ladder therefore carries
    // the identical theft, against a LARGER number (the receiver books the on-chain funding value),
    // and it is now bound by the matching law in `verify_bundle_ex`.
    //
    // What remains true, and is the reason the two laws are written separately rather than shared:
    // the child's chain funds from an UN-BROADCAST output, so its yardstick is a value parsed from
    // another tier; the root's funds from `F`, an on-chain output. Different provenance, same law.
    // A split child additionally has no backup at all — an SE-registered key never funded on-chain,
    // so `check_deposit`/`create_tx1` never runs, which is why `CHILD_V2_BASELINE = 0`.
    //
    // The law is not a new invariant — it is what the builders have always done
    // (`build_extension_from` -> `tier_out_value`, lib/src/tesr.rs:376), so an honest bundle passes
    // with equality. It was simply never checked on the receive side. Note the direction: an
    // UNDER-paying tier is the attack, but this is an exact-equality check rather than a `>=`,
    // because an OVER-paying tier does not conserve either and would mean the fee is not committed —
    // which is the property the whole un-broadcast design rests on.
    let rung_forward = |prev: u64, what: &str| -> Result<u64> {
        let v = if cb.is_colored() {
            crate::rgb::colored_tier_out_total(prev, 1, cb.parent.fee_rate)
        } else {
            mercurylib::tesr::tier_out_value(prev, cb.parent.fee_rate)
        };
        v.ok_or_else(|| {
            anyhow::anyhow!(
                "{what}: funding of {prev} sat cannot carry a tier at {} sat/vB",
                cb.parent.fee_rate
            )
        })
    };
    let st_tx: Transaction = deserialize(
        &hex::decode(&cb.child_state.signed_tx).map_err(|_| anyhow::anyhow!("bad child state hex"))?,
    )
    .map_err(|_| anyhow::anyhow!("child state is not a transaction"))?;
    if st_tx.input.is_empty() {
        return Err(anyhow::anyhow!("child state has no input"));
    }
    // state_child spends ext_child's PAYLOAD output — PIN 1, and the ONLY door to `ext_out0`.
    //
    // This binding used to sit ~70 lines above, before `st_tx` was even parsed, and every value check
    // below it read the payload at the DECLARED `payload_vout`. On a bundle whose vout is tampered
    // they computed on the P2A anchor and refused on VALUE, shadowing the accurate structural cause
    // that this very check states (sdk70 D1 pins that message). The fix was a comment saying "all of
    // them must sit BELOW the structural check" — and then one check was moved and its two
    // neighbours were left behind. Now the structural check MAKES the value: there is no `ext_out0`
    // until `link_child` has returned one, so the ordering cannot be got wrong again.
    let ext_out0 = cb.child_extension.link_child(
        &ext_tx,
        &st_tx,
        "child extension",
        "child state does not spend ext_child's payload output",
    )?;

    let expect_ext = rung_forward(sp_out.value, "child extension")?;
    // THAT NOTHING ELSE LEAVES. Summed over every non-anchor, non-opret output, not just
    // `out[payload_vout]`: pinning one output leaves a window exactly one committed fee wide for a
    // second output to carry value out of the chain.
    let ext_payload_total: u64 = ext_tx
        .output
        .iter()
        .filter(|o| {
            o.script_pubkey.as_bytes() != mercurylib::tesr::P2A_SCRIPT_BYTES
                && !o.script_pubkey.is_op_return()
        })
        .map(|o| o.value)
        .sum();
    if ext_payload_total != expect_ext {
        return Err(anyhow::anyhow!(
            "child extension is funded with {} sat but its payload outputs carry {ext_payload_total} \
             (expected exactly {expect_ext}) — the difference would leave the exit chain",
            sp_out.value
        ));
    }
    if ext_out0.value() != expect_ext {
        return Err(anyhow::anyhow!(
            "child extension is funded with {} sat but forwards only {} to its payload output \
             (expected exactly {expect_ext} = funding − one rung at {} sat/vB) — {} sat would be \
             skimmed to another output while the receiver is credited the funding value",
            sp_out.value,
            ext_out0.value(),
            cb.parent.fee_rate,
            sp_out.value.saturating_sub(ext_out0.value() + (sp_out.value - expect_ext)),
        ));
    }

    // The DECLARED field too, symmetrically with the state's `[value-gate spoof]` check below. The
    // conservation law above pins the SIGNED value; this pins the field that travels beside it, and
    // they are different properties. `child_in_ladder_split` later feeds `cb.child_extension
    // .out_value` to `tier_out_total` and to `cosign_tier` as a prevout amount, so a field that
    // disagrees with its own transaction makes the receiver's OWN next split sign against a sighash
    // committing to an amount the transaction does not carry — a signature that verifies against
    // nothing, discovered only after `set_spend_budget` has terminalized the coin.
    if ext_out0.value() != cb.child_extension.out_value {
        return Err(anyhow::anyhow!(
            "child extension out[{}] carries {} sat but the bundle declares out_value {} — the \
             declared value is what later splits of this child would compute and sign against",
            cb.child_extension.payload_vout,
            ext_out0.value(),
            cb.child_extension.out_value
        ));
    }
    // WHERE IT PAYS — the leaf's copy of the ancestor check above, and load-bearing for the same
    // reason: `child_state`'s co-sign is verified against a prevout SYNTHESISED as
    // `TxOut { value: ext_out0.value, script_pubkey: child_agg_spk }`. If the real payload output
    // pays another key, the state is signed against a prevout that does not exist — unbroadcastable
    // forever, while whoever holds the real key sweeps the child once the extension confirms.
    if ext_out0.script_pubkey() != &child_agg_spk {
        return Err(anyhow::anyhow!(
            "child extension's payload output does not pay A_child — the child state below it would \
             be signed against a prevout that does not exist"
        ));
    }
    verify_tier_cosigned(&st_tx, ext_out0.value(), &child_agg_spk)
        .map_err(|e| anyhow::anyhow!("child state not co-signed by A_child: {e}"))?;
    // [F4] child state CSV: a valid BIP-68 block relative-timelock within the state schedule.
    {
        let seq = st_tx.input[0].sequence.0;
        if seq & (1 << 31) != 0 || seq & (1 << 22) != 0 {
            return Err(anyhow::anyhow!("child state: not a BIP-68 block relative-timelock"));
        }
        let (csv, p) = (seq as u16, cb.parent.params);
        if csv < p.d_floor || csv > p.d0 {
            return Err(anyhow::anyhow!("child state CSV {csv} outside [{},{}]", p.d_floor, p.d0));
        }
        // [B1] the declared field is the signed one, or the bundle is refused.
        mercurylib::transfer::receiver::bind_declared_csv(
            1,
            "child state",
            cb.child_state.csv,
            Some(csv),
        )?;
    }

    // MODEL A: the final child state must pay the RECEIVER's own key.
    let recv_spk = Address::from_str(receiver_backup_address)
        .map_err(|_| anyhow::anyhow!("bad receiver backup address"))?
        .require_network(net)
        .map_err(|_| anyhow::anyhow!("receiver backup address wrong network"))?
        .script_pubkey();
    let recv_key = taproot_key_hex(recv_spk.as_bytes())?;
    // PIN 2′, and the only pin available on this tier: the child state is TERMINAL, so nothing spends
    // its payload output and there is no `link_child` to be had. Model A — "the last state pays the
    // receiver's own key" — is therefore what licenses reading its value below, and the two checks
    // that do (`out_value` agreement and the second conservation hop) can no longer be written above
    // it, because `st_out0` does not exist until it has passed.
    let st_out0 = cb.child_state.link_pays_taproot_key(
        &st_tx,
        &recv_key,
        "child state",
        "child state does not pay the receiver's key (Model A violated)",
    )?;
    // [value-binding — child value-gate spoof] Bind the receiver-paying output's VALUE to the bundle's
    // declared `out_value`. `verify_tier_cosigned` binds the co-sign to the INPUT amount, not the
    // output split, and the blind SE co-signs ANY output distribution — so without this a payer crafts
    // `state_child.out[0]` paying the receiver a few sats while declaring a large `out_value` (remainder
    // to a second output back to itself), and any value gate that trusts the declared field (the SSP's
    // pre-pay value gate — audit) pays the full invoice for a near-worthless piece. out[0] is forced to
    // be the receiver payment by the key check above (a P2A anchor or change can only be a LATER
    // output), so binding `out[0].value == out_value` makes `verify_conveyed_child`'s returned value
    // trustworthy. Live on the shipped child census (sdk59), not just non-exact LN.
    if st_out0.value() != cb.child_state.out_value {
        return Err(anyhow::anyhow!(
            "child state out[0] pays {} sat but the bundle declares out_value {} — value-gate spoof",
            st_out0.value(), cb.child_state.out_value
        ));
    }
    // [VALUE-CONSERVATION, second hop] The declared/signed agreement above is NOT the same property:
    // it makes the number honest, not the number CORRECT. The skim needs both hops bound, because
    // moving it one tier down works identically — an extension that forwards the full amount and a
    // state that pays the payee 510 while sending the rest to a second output is the same theft with
    // the same receiver-side booking.
    let expect_state = rung_forward(ext_out0.value(), "child state")?;
    if st_out0.value() != expect_state {
        return Err(anyhow::anyhow!(
            "child state is funded with {} sat but pays the receiver only {} \
             (expected exactly {expect_state} = funding − one rung at {} sat/vB) — the remainder \
             would leave the receiver's exit chain entirely",
            ext_out0.value(),
            st_out0.value(),
            cb.parent.fee_rate,
        ));
    }

    // THAT NOTHING ELSE LEAVES, on the state hop too — the mirror of the extension's Σ check above,
    // and of GAP 1 on the root lane in the opposite direction. The per-output check just above pins
    // what the RECEIVER is paid; this pins that nothing ELSE is paid. `skim_root_attack_tests` proved
    // the two are independent: Σ alone permits a sum-preserving redistribution, and a single-output
    // check alone permits an extra output. The state hop had only the second, so a state tier
    // carrying a surplus output while paying the receiver exactly right passed — consensus-invalid
    // rather than a skim, i.e. it strands the child, which is the shape sweep finding V4 describes
    // and §8 does NOT claim closed for this hop.
    let st_payload_total: u64 = st_tx
        .output
        .iter()
        .filter(|o| {
            o.script_pubkey.as_bytes() != mercurylib::tesr::P2A_SCRIPT_BYTES
                && !o.script_pubkey.is_op_return()
        })
        .map(|o| o.value)
        .sum();
    if st_payload_total != expect_state {
        return Err(anyhow::anyhow!(
            "child state is funded with {} sat but its payload outputs carry {st_payload_total} \
             (expected exactly {expect_state}) — a surplus output makes the tier unbroadcastable and \
             strands the child, while the receiver is credited the funding value",
            ext_out0.value()
        ));
    }

    // [6 cont.] CHILD SUPERSEDED SEGMENT. A child that has been RE-TRANSFERRED discloses the states it
    // replaced (one per hop), each of which consumed a real co-sign and so must be counted — but only
    // after being proven non-confirmable, exactly like the root ladder's. Same shared battery, so the
    // two can never drift.
    //
    // Seeding differs from `verify_bundle_ex` in one way worth stating: that function seeds `live` from
    // `txs[1..]` because a root ladder's first tier is the TRIGGER, which carries no CSV. A child
    // segment is HEADLESS — it starts at its extension, which does have a CSV — so BOTH child tiers
    // seed the live map.
    let child_superseded_ok = {
        use electrum_client::bitcoin::Txid as _Txid;
        let mut child_prevouts: std::collections::HashMap<(_Txid, u32), u64> =
            std::collections::HashMap::new();
        child_prevouts.insert((sp_txid, cb.sp_vout), sp_out.value);
        for tx in [&ext_tx, &st_tx] {
            let id = tx.txid();
            for (vout, o) in tx.output.iter().enumerate() {
                child_prevouts.insert((id, vout as u32), o.value);
            }
        }
        let mut child_live: std::collections::HashMap<(_Txid, u32), u32> =
            std::collections::HashMap::new();
        // Same rule as the ancestor segment: the KEYED census map must track the payload vout of the
        // tier it describes, or a rival would be raced against the wrong live CSV (CTESR-GATE §3.2).
        child_live.insert((sp_txid, cb.sp_vout), ext_tx.input[0].sequence.0 & 0xFFFF);
        child_live.insert(
            (ext_tx.txid(), cb.child_extension.payload_vout),
            st_tx.input[0].sequence.0 & 0xFFFF,
        );
        let child_live_ids: std::collections::HashSet<_Txid> =
            [ext_tx.txid(), st_tx.txid()].into_iter().collect();
        verify_superseded_segment(
            &cb.child_superseded_states,
            &cb.child_superseded_extensions,
            &child_agg_spk,
            &cb.parent.params,
            &mut child_prevouts,
            &child_live,
            &child_live_ids,
        )?
    };

    // [6 cont.] CHILD CENSUS exact-equality: the child discloses exactly ext_child + state_child (2
    //     co-signs) plus one superseded state per onward hop, on top of any flat backups (a derived slot
    //     has none — CHILD_V2_BASELINE = 0). A hidden child co-sign would push child_num_sigs above
    //     this ⟹ reject. Key handovers are census-NEUTRAL (the enclave bumps sig_count only when it
    //     signs), so an adopted child counts the same as a conveyed one.
    let child_expected = child_flat_backups + 2 + child_superseded_ok;
    if child_num_sigs != child_expected {
        return Err(anyhow::anyhow!(
            "child num_sigs mismatch: SE issued {child_num_sigs}, disclosed accounts for {child_expected} — possible hidden child state"
        ));
    }

    // [7] COLOUR, structurally. Colour-blind, no RGB engine — the child-side sibling of
    //     `verify_colored_shape`, which `verify_bundle_ex` already ran over the parent segment.
    verify_colored_child_shape(cb)?;

    Ok(())
}

/// **[CTES-R] The structural half of colour for a split CHILD.** No RGB engine, no network.
///
/// The load-bearing check is the BICONDITIONAL: a coloured parent segment and a plain child is the
/// allocation-destroying shape. `SP.out[j]` is a SEALED output; a child whose two tiers are
/// uncoloured spends it with an RGB-unaware transaction, and every other check in
/// `verify_child_bundle` passes — the transactions are perfectly valid Bitcoin, the census balances,
/// the aggregates are right. Only this rejects it.
///
/// The converse (plain parent, coloured child) is equally refused: the child would claim an
/// allocation its ancestor segment carries no transition for, so nothing could ever validate it.
fn verify_colored_child_shape(cb: &ChildTesrBundle) -> Result<()> {
    use electrum_client::bitcoin::{consensus::deserialize, Transaction};

    let parent_colored = cb.parent.is_colored();
    if parent_colored != cb.is_colored() {
        return Err(anyhow::anyhow!(
            "colour mismatch: the parent segment is {} but the child is {} — a plain child over a \
             COLOURED SP output spends a sealed UTXO with no transition and destroys the \
             allocation; a coloured child under a plain parent has no transition chain to validate \
             against",
            if parent_colored { "COLOURED" } else { "PLAIN" },
            if cb.is_colored() { "COLOURED" } else { "PLAIN" }
        ));
    }

    // Both child tiers, checked for the opret shape either way round.
    let tiers = [("child_extension", &cb.child_extension), ("child_state", &cb.child_state)];
    for (name, tier) in tiers {
        let raw = hex::decode(&tier.signed_tx)
            .map_err(|_| anyhow::anyhow!("{name}: hex does not decode"))?;
        let tx: Transaction =
            deserialize(&raw).map_err(|_| anyhow::anyhow!("{name}: tx does not parse"))?;
        let oprets: Vec<usize> = tx
            .output
            .iter()
            .enumerate()
            .filter(|(_, o)| o.script_pubkey.is_op_return())
            .map(|(v, _)| v)
            .collect();
        match (cb.is_colored(), oprets.len()) {
            (true, 1) => {
                if oprets[0] as u32 == tier.payload_vout {
                    return Err(anyhow::anyhow!(
                        "coloured {name} declares its payload at vout {} — that output is the RGB \
                         opret commitment, which carries no value and cannot be spent",
                        tier.payload_vout
                    ));
                }
            }
            (true, n) => {
                return Err(anyhow::anyhow!(
                    "coloured {name} carries {n} OP_RETURN outputs, expected exactly 1"
                ))
            }
            (false, 0) => {}
            (false, n) => {
                return Err(anyhow::anyhow!(
                    "{name} carries {n} OP_RETURN output(s) but this child is conveyed as PLAIN — \
                     a coloured child passed off as plain would have its asset half validated by \
                     nobody"
                ))
            }
        }
    }

    let Some(rgb) = cb.rgb.as_ref() else {
        return Ok(());
    };
    // A coloured child must be depth-1: coloured child-level split does not exist, so a coloured
    // bundle with intermediate segments did not come from this code and has no seal schedule.
    let _ = cb.colored_child_seals()?;
    let parent_rgb = cb
        .parent
        .rgb
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("coloured child with no parent allocation"))?;
    if rgb.contract_id != parent_rgb.contract_id {
        return Err(anyhow::anyhow!(
            "coloured child claims contract {} but its ancestor segment carries {}",
            rgb.contract_id,
            parent_rgb.contract_id
        ));
    }
    if rgb.amount == 0 {
        return Err(anyhow::anyhow!("coloured child carries a zero allocation"));
    }
    // A child is one share of the parent's whole, so it can never exceed it. (The exact split is
    // proved by the CONSIGNMENT at accept time — this is the cheap structural bound.)
    if rgb.amount > parent_rgb.amount {
        return Err(anyhow::anyhow!(
            "coloured child claims {} but its ancestor segment only carries {}",
            rgb.amount,
            parent_rgb.amount
        ));
    }
    if rgb.consignments.len() != 2 {
        return Err(anyhow::anyhow!(
            "coloured child carries {} consignments for its 2 own tiers — they are indexed by exit \
             order, so a mismatch means the receiver cannot tell which proof belongs to which tier",
            rgb.consignments.len()
        ));
    }
    if rgb.consignments.iter().any(|c| c.trim().is_empty()) {
        return Err(anyhow::anyhow!("coloured child carries an empty consignment"));
    }
    // The child's `sp_vout` must be a real, spendable, NON-opret output of SP.
    let sp_raw = hex::decode(&cb.parent.current().state.signed_tx)
        .map_err(|_| anyhow::anyhow!("bad SP hex"))?;
    let sp: Transaction =
        deserialize(&sp_raw).map_err(|_| anyhow::anyhow!("SP is not a transaction"))?;
    let out = sp
        .output
        .get(cb.sp_vout as usize)
        .ok_or_else(|| anyhow::anyhow!("child's sp_vout {} is past SP's outputs", cb.sp_vout))?;
    if out.script_pubkey.is_op_return() {
        return Err(anyhow::anyhow!(
            "child's sp_vout {} is SP's RGB opret commitment, which carries no value and cannot be \
             spent",
            cb.sp_vout
        ));
    }
    Ok(())
}

/// Verify a tier tx carries a genuine SE/owner co-signature by the aggregate key `A` — i.e. that it
/// actually consumed one of the SE's finalized signatures. Consensus-style: the taproot key-spend
/// signature must verify against the OUTPUT key in `agg_spk`'s witness program over the sighash that
/// `cosign_tier_request` commits to (`TxOut { value: prevout_value, script_pubkey: agg_spk }`,
/// `TapSighashType::All`). Without this, a structurally valid but never-co-signed tx could pad
/// `verify_bundle`'s count and mask a hidden state [S1].
fn verify_tier_cosigned(
    tx: &electrum_client::bitcoin::Transaction,
    prevout_value: u64,
    agg_spk: &electrum_client::bitcoin::ScriptBuf,
) -> Result<()> {
    use electrum_client::bitcoin::{
        secp256k1::{schnorr, Message, Secp256k1, XOnlyPublicKey},
        sighash::{Prevouts, SighashCache, TapSighashType},
        TxOut,
    };

    let prog = agg_spk.as_bytes();
    if prog.len() != 34 || prog[0] != 0x51 || prog[1] != 0x20 {
        return Err(anyhow::anyhow!("aggregate address is not a v1 taproot output"));
    }
    let output_key = XOnlyPublicKey::from_slice(&prog[2..34])
        .map_err(|_| anyhow::anyhow!("bad taproot output key"))?;

    let prevout = TxOut { value: prevout_value, script_pubkey: agg_spk.clone() };
    let sighash = SighashCache::new(tx)
        .taproot_key_spend_signature_hash(0, &Prevouts::All(&[prevout]), TapSighashType::All)
        .map_err(|e| anyhow::anyhow!("sighash: {e}"))?;

    let wit = tx.input[0].witness.to_vec();
    if wit.len() != 1 {
        return Err(anyhow::anyhow!("witness is not a single key-spend signature"));
    }
    let sig = match wit[0].len() {
        64 => schnorr::Signature::from_slice(&wit[0]),
        65 => {
            if wit[0][64] != TapSighashType::All as u8 {
                return Err(anyhow::anyhow!("tier is not SIGHASH_ALL"));
            }
            schnorr::Signature::from_slice(&wit[0][..64])
        }
        n => return Err(anyhow::anyhow!("bad signature length {n}")),
    }
    .map_err(|_| anyhow::anyhow!("malformed schnorr signature"))?;

    let msg = Message::from_slice(sighash.as_ref()).map_err(|_| anyhow::anyhow!("bad sighash"))?;
    Secp256k1::verification_only()
        .verify_schnorr(&sig, &msg, &output_key)
        .map_err(|_| anyhow::anyhow!("signature does not verify against the aggregate key A"))
}

/// **THE RETIREMENT ASSERTION, source-level half: no UNCOLOURED tier builder is reachable with a
/// coloured coin.**
///
/// Every function in this module that constructs a PLAIN tier —
/// `mercurylib::tesr::build_{trigger,extension,state,split_state,detrigger,extension_from,state_from}`
/// — spends an output that, on a coloured coin, carries an RGB seal. Doing so with an RGB-unaware
/// transaction destroys the allocation, which is the one thing this protocol may never do. Each such
/// function must therefore either call [`refuse_uncolored_over_colored`] /
/// [`refuse_uncolored_over_colored_child`], or appear in [`GUARD_EXEMPT`] with the reason it does not
/// need to.
///
/// This is a GREP over this module's own source, and it is deliberately that rather than a behavioural
/// test: the hazard is a builder somebody adds LATER without a guard, which no behavioural test can
/// anticipate. A per-builder behavioural test proves the guards that exist work (see
/// `colored_interlock_tests` below); this proves the set of guarded builders is COMPLETE.
#[cfg(test)]
mod uncoloured_builder_census {
    /// Builders that construct a PLAIN (RGB-unaware) tier. Any call to one of these puts the
    /// enclosing function on the hook for a carrier guard.
    const PLAIN_TIER_BUILDERS: &[&str] = &[
        "mercurylib::tesr::build_trigger(",
        "mercurylib::tesr::build_extension(",
        "mercurylib::tesr::build_extension_from(",
        "mercurylib::tesr::build_state(",
        "mercurylib::tesr::build_state_from(",
        "mercurylib::tesr::build_split_state(",
        "mercurylib::tesr::build_detrigger(",
    ];

    /// The guard tokens that discharge the obligation.
    const GUARDS: &[&str] =
        &["refuse_uncolored_over_colored(", "refuse_uncolored_over_colored_child("];

    /// `(function, why it needs no guard)`. Every entry is a claim that a COLOURED coin cannot
    /// reach that function, and each one is checked below against a real property, not just listed.
    const GUARD_EXEMPT: &[(&str, &str)] = &[
        (
            "establish",
            "Builds a coin's FIRST ladder, so there is no bundle to inspect and no seal to break \
             yet — the coin has no coloured ladder by definition. A carrier is kept out at the \
             single decision site in the SDK's claim pass, which routes a carrier with exactly one \
             booked allocation to `build_colored_ladder_auto` and leaves every other carrier on the \
             flat lane (`LadderSkipReason::RgbCarrier`). Guarding here is impossible, not merely \
             omitted.",
        ),
        (
            "establish_child",
            "Only reachable from `in_ladder_split`, which IS guarded — a coloured parent is refused \
             before any child is established. It takes no bundle of its own to test.",
        ),
        (
            "establish_child_journalled",
            "The journalling twin of `establish_child`, and exempt for the same reason plus a \
             stronger one. Its two live callers (`in_ladder_split`, `child_in_ladder_split`) refuse \
             a coloured parent in their FIRST statement, before the journal record is written; so a \
             `splitjrnl-` record only ever describes a PLAIN split, and the third caller — \
             `resume_in_ladder_split` — can therefore only ever replay one. It takes no bundle of \
             its own to test: it reads its tier parameters from that record.",
        ),
    ];

    /// Top-level `fn` / `pub fn` / `pub async fn` boundaries of the NON-test source.
    fn production_functions() -> Vec<(String, String)> {
        let src = include_str!("tesr.rs");
        let mut out: Vec<(String, String)> = Vec::new();
        let mut name = String::from("<file scope>");
        let mut body = String::new();
        for line in src.lines() {
            // Everything from the first top-level `#[cfg(test)]` on is test code, including this
            // module. Stop there so the census never grades its own fixtures.
            if line.starts_with("#[cfg(test)]") {
                break;
            }
            let decl = line
                .strip_prefix("pub async fn ")
                .or_else(|| line.strip_prefix("pub fn "))
                .or_else(|| line.strip_prefix("async fn "))
                .or_else(|| line.strip_prefix("fn "));
            if let Some(rest) = decl {
                out.push((std::mem::take(&mut name), std::mem::take(&mut body)));
                name = rest
                    .split(|c: char| c == '(' || c == '<' || c == ' ')
                    .next()
                    .unwrap_or("?")
                    .to_string();
            }
            body.push_str(line);
            body.push('\n');
        }
        out.push((name, body));
        out
    }

    #[test]
    fn every_plain_tier_builder_call_is_behind_a_carrier_guard() {
        let mut unguarded: Vec<String> = Vec::new();
        let mut exempt_used: Vec<String> = Vec::new();
        let mut seen_any = false;
        for (name, body) in production_functions() {
            let builders: Vec<&str> = PLAIN_TIER_BUILDERS
                .iter()
                .copied()
                .filter(|b| body.contains(b))
                .collect();
            if builders.is_empty() {
                continue;
            }
            seen_any = true;
            if GUARDS.iter().any(|g| body.contains(g)) {
                continue;
            }
            match GUARD_EXEMPT.iter().find(|(f, _)| *f == name) {
                Some((f, _)) => exempt_used.push((*f).to_string()),
                None => unguarded.push(format!("{name} (calls {builders:?})")),
            }
        }
        assert!(
            seen_any,
            "the census found NO plain tier builder call at all — the source parser has drifted \
             from the code and this test is now vacuous, which is worse than failing"
        );
        assert!(
            unguarded.is_empty(),
            "these functions build an UNCOLOURED tier with no carrier guard and no stated \
             exemption — on a coloured coin each one spends a sealed output with an RGB-unaware \
             transaction and DESTROYS the allocation: {unguarded:?}"
        );
        // A stale exemption is a hole that looks like a decision. Every entry must still be earning
        // its place.
        let stale: Vec<&str> = GUARD_EXEMPT
            .iter()
            .map(|(f, _)| *f)
            .filter(|f| !exempt_used.iter().any(|u| u == f))
            .collect();
        assert!(
            stale.is_empty(),
            "these guard exemptions no longer correspond to an unguarded builder — delete them \
             rather than leaving a standing licence: {stale:?}"
        );
    }

    /// The exemption for `establish` claims the SDK's claim pass is the decision site. That claim is
    /// about a different crate, so it is asserted where it can be: `establish` must not itself be
    /// able to produce a coloured ladder (it has no RGB engine argument at all), which is what makes
    /// "a carrier must not reach it" the SDK's job rather than a missing branch here.
    #[test]
    fn establish_cannot_colour_anything_so_the_decision_must_be_upstream() {
        let src = include_str!("tesr.rs");
        let start = src.find("\npub async fn establish(").expect("establish exists");
        let body = &src[start..start + 2_000];
        assert!(
            !body.contains("RgbWallet") && !body.contains("contract_id"),
            "`establish` has gained RGB awareness — the exemption in GUARD_EXEMPT is now wrong and \
             the carrier decision may no longer be entirely upstream"
        );
    }
}

/// **THE ORDERING INVARIANT, source-level half: a declared `payload_vout` becomes an output in
/// exactly one module.**
///
/// [`LinkedPayload`] makes "value law before linkage check" a compile error for anything that goes
/// through `TesrTier`. It cannot make `tx.output[tier.payload_vout as usize]` a compile error —
/// `payload_vout` is a `pub` field, that expression is ordinary Rust, and it is how every test in
/// this file reads a payload. So the type closes the door and this closes the window: without it the
/// class simply reappears in a shape the type cannot see.
///
/// A grep over this module's own source, deliberately, and for the same reason as
/// [`uncoloured_builder_census`]: the hazard is a value check somebody adds LATER, which no
/// behavioural test can anticipate. `mod linked`'s own unit behaviour is exercised by the adversarial
/// suites; this proves the set of places that may take the step is still exactly one.
///
/// **What it does NOT catch, stated so nobody mistakes its reach:** an alias (`let outs =
/// &tx.output;` then `outs[…]`), a helper in another file, or a value laundered through a `u64`
/// before the law reads it. It catches the direct shape — which is the one all four incidents took,
/// and the one a hurried author writes.
#[cfg(test)]
mod payload_vout_access_census {
    /// Everything before the first top-level `#[cfg(test)]` — the production half of this file. The
    /// non-vacuity assertions below must run against this and not the whole file, or the string
    /// literals in this very module would satisfy them.
    fn production_half(src: &str) -> &str {
        match src.find("\n#[cfg(test)]") {
            Some(at) => &src[..at + 1],
            None => src,
        }
    }

    /// `(line number, line)` for every production line that turns a declared `payload_vout` into an
    /// output OUTSIDE `mod linked`. Pure and total over an arbitrary source string, so the planted
    /// cases below can drive the detector itself rather than trusting it.
    fn unlinked_payload_reads(src: &str) -> Vec<(usize, String)> {
        let mut hits: Vec<(usize, String)> = Vec::new();
        let mut in_linked = false;
        for (n, line) in src.lines().enumerate() {
            // Test code is exempt: a test builds the bundle it is attacking and holds both key
            // halves, so there is no attacker-supplied index to defend against.
            if line.starts_with("#[cfg(test)]") {
                break;
            }
            if line.starts_with("mod linked {") {
                in_linked = true;
                continue;
            }
            if in_linked {
                // The module is top-level, so its closing brace is the next column-0 `}`.
                if line == "}" {
                    in_linked = false;
                }
                continue;
            }
            if line.trim_start().starts_with("//") {
                continue;
            }
            let hand_rolled = line.contains("payload_vout")
                && (line.contains(".output")
                    || line.contains("output[")
                    || line.contains("output.get("));
            if line.contains("payload_out(") || hand_rolled {
                hits.push((n + 1, line.to_string()));
            }
        }
        hits
    }

    #[test]
    fn a_declared_payload_vout_becomes_an_output_only_inside_mod_linked() {
        let src = include_str!("tesr.rs");
        let prod = production_half(src);

        // Non-vacuity, part 1: the module, the private accessor and the builders' door are all still
        // where this census thinks they are. If any of these drifts, the scan below would pass by
        // scanning nothing meaningful — which is worse than failing.
        assert!(prod.contains("\nmod linked {"), "`mod linked` is gone — this census is now vacuous");
        assert!(
            prod.contains("fn payload_out<'a>(&self, tx: &'a Transaction, what: &str)"),
            "the private accessor has been renamed or moved out of `mod linked`"
        );
        assert!(
            prod.contains("pub(super) fn tier_payload_prevout("),
            "the builders' trusted door has left `mod linked` — the raw accessor now has a second \
             user and the encapsulation argument no longer holds"
        );

        // Non-vacuity, part 2: each constructor is DEFINED and actually CALLED. A guarded abstraction
        // nobody uses would let every verifier quietly go back to hand-rolling the index. The call
        // pattern carries a leading `.`, which the definition does not, so one occurrence of it is
        // one real call site.
        for ctor in ["link_child", "link_pays", "link_pays_taproot_key"] {
            assert!(
                prod.contains(&format!("pub(super) fn {ctor}<'a>(")),
                "`{ctor}` is no longer defined in `mod linked`"
            );
            assert!(
                prod.matches(&format!(".{ctor}(")).count() >= 1,
                "`{ctor}` is defined but has no production call site — a verifier has stopped \
                 pinning its payload, which is exactly the regression this census exists to see"
            );
        }

        let hits = unlinked_payload_reads(src);
        assert!(
            hits.is_empty(),
            "these production lines turn a declared `payload_vout` into an output outside \
             `mod linked`, which is how the four check-ordering defects were written: {hits:#?}"
        );
    }

    /// A guard that cannot fail is decoration. These are the shapes the four incidents took —
    /// replanted — plus the hand-rolled index that is the type's blind spot and this census's whole
    /// reason to exist.
    #[test]
    fn the_census_catches_each_shape_it_was_written_for() {
        let cases = [
            (
                "root value law re-derives the parent's payload",
                r#"
fn verify_bundle_ex() {
    let prev_payload = tiers[i - 1].payload_out(&txs[i - 1], "tier")?.value;
}
"#,
            ),
            (
                "leaf declared-out_value check reads the declared vout",
                r#"
fn verify_child_bundle() {
    let ext_out0 = cb.child_extension.payload_out(&ext_tx, "child extension")?;
}
"#,
            ),
            (
                "hand-rolled index — the escape the type cannot see",
                r#"
fn verify_child_bundle() {
    let out = ext_tx.output[cb.child_extension.payload_vout as usize].clone();
}
"#,
            ),
            (
                "...and its `get` spelling",
                r#"
fn verify_child_bundle() {
    let out = st_tx.output.get(cb.child_state.payload_vout as usize).unwrap();
}
"#,
            ),
        ];
        for (tag, body) in cases {
            assert!(
                !unlinked_payload_reads(body).is_empty(),
                "planted shape `{tag}` was NOT caught — the census would not have caught the \
                 defects it was written for"
            );
        }
    }

    /// ...and it must not cry wolf, or it just pushes people back to the hand-rolled index.
    #[test]
    fn the_census_accepts_the_corrected_shapes() {
        let body = r#"
mod linked {
    impl TesrTier {
        fn payload_out<'a>(&self, tx: &'a Transaction, what: &str) -> Result<&'a TxOut> {
            tx.output.get(self.payload_vout as usize).ok_or_else(|| anyhow::anyhow!("{what}"))
        }
    }
}

fn verify_bundle_ex() {
    // A hand-rolled tx.output[tier.payload_vout as usize] named in a COMMENT is documentation.
    let link = tiers[i - 1].link_child(&txs[i - 1], tx, "tier 0", "tier 1 does not spend it")?;
    let prev_payload = link.value();
    // Comparing the declared vout to a prevout, or keying a census map by it, reads no output.
    if tx.input[0].previous_output.vout != tiers[i - 1].payload_vout {
        return Err(anyhow::anyhow!("no"));
    }
    live.insert((ext_tx.txid(), cb.child_extension.payload_vout), seq);
}
"#;
        let hits = unlinked_payload_reads(body);
        assert!(hits.is_empty(), "the corrected shapes were flagged: {hits:#?}");
    }
}

#[cfg(test)]
mod verify_tests {
    use super::*;

    const AGG: &str = "bcrt1p83afnxgnczlsqvd20swjlnr3kcm7hvz9p338dgueetjz2tx6vvjs05rsfy";
    pub(super) const OWNER: &str = "bcrt1qv23qwf82jw5k68juxnlxx06yz8plu0mrfrqvws";
    const F_TXID: &str = "1111111111111111111111111111111111111111111111111111111111111111";

    // A schedule-conformant single-level bundle (unsigned tiers — verify_bundle checks structure).
    pub(super) fn sample_bundle() -> TesrBundle {
        let p = mercurylib::tesr::TesrParams::regtest();
        let f_value = 100_000u64;
        let t = mercurylib::tesr::build_trigger(F_TXID, 0, f_value, AGG, "regtest", p.committed_fee_rate).unwrap();
        let x = mercurylib::tesr::build_extension(&t.txid, t.out_value, AGG, "regtest", p.ext_csv(0), p.committed_fee_rate).unwrap();
        let s = mercurylib::tesr::build_state(&x.txid, x.out_value, OWNER, "regtest", p.state_csv(0), p.committed_fee_rate).unwrap();
        TesrBundle {
            version: 1, statechain_id: "sid".into(), network: "regtest".into(),
            fee_rate: p.committed_fee_rate, agg_address: AGG.into(), owner_exit_address: OWNER.into(),
            f_txid: F_TXID.into(), f_vout: 0, f_value,
            trigger: TesrTier { txid: t.txid, signed_tx: t.tx_hex, out_value: t.out_value, csv: None, payload_vout: t.payload_vout },
            levels: vec![TesrLevel {
                extension: TesrTier { txid: x.txid, signed_tx: x.tx_hex, out_value: x.out_value, csv: Some(p.ext_csv(0)), payload_vout: x.payload_vout },
                state: TesrTier { txid: s.txid, signed_tx: s.tx_hex, out_value: s.out_value, csv: Some(p.state_csv(0)), payload_vout: s.payload_vout },
            }],
            m: 0, superseded_states: vec![], superseded_extensions: vec![], params: p, rgb: None,
        }
    }

    /// A minimal split child over `sample_bundle`, with colour dialled independently on each half so
    /// the BICONDITIONAL can be exercised in both broken directions. The tiers are the sample
    /// bundle's plain ones — that is deliberate: the colour-mismatch check must fire on the
    /// declaration alone, before anything is parsed.
    fn sample_child(colored_parent: bool, colored_child: bool) -> ChildTesrBundle {
        let mut parent = sample_bundle();
        if colored_parent {
            parent.rgb = Some(ColoredLadder {
                contract_id: "rgb:contract".into(),
                amount: 1_000,
                consignments: vec!["a".into(), "b".into(), "c".into()],
            });
        }
        let lvl = parent.levels[0].clone();
        ChildTesrBundle {
            parent,
            parent_statechain_id: "sid".into(),
            sp_vout: 0,
            child_statechain_id: "child-sid".into(),
            child_owner_exit_address: OWNER.into(),
            child_extension: lvl.extension,
            child_state: lvl.state,
            child_superseded_states: vec![],
            child_superseded_extensions: vec![],
            ancestors: vec![],
            rgb: colored_child.then(|| ColoredChild {
                contract_id: "rgb:contract".into(),
                amount: 400,
                consignments: vec!["x".into(), "y".into()],
            }),
            // These shape tests fire before any chain access, so the conveyed chain is not read.
            parent_flat_backups: vec![],
        }
    }

    /// A plain child bundle for the [CATS/V4] payload-interchangeability test in `spine_tip_tests`.
    pub(super) fn sample_child_for_tip_test() -> ChildTesrBundle {
        sample_child(false, false)
    }

    /// **The allocation-destroying shape, refused.** A COLOURED parent segment whose child is plain
    /// means the child's two tiers spend `SP.out[j]` — a SEALED output — with RGB-unaware
    /// transactions. Every other check in `verify_child_bundle` passes on that bundle: the
    /// transactions are valid Bitcoin, the census balances, the aggregates are right. This is the
    /// only thing standing between it and a burnt allocation.
    #[test]
    fn a_plain_child_over_a_coloured_parent_is_refused() {
        let e = verify_colored_child_shape(&sample_child(true, false))
            .expect_err("a plain child over a COLOURED SP output destroys the allocation");
        assert!(
            e.to_string().contains("colour mismatch"),
            "must reject on the colour biconditional, got: {e}"
        );
        // And the converse: a coloured child under a plain ancestor segment has no transition chain
        // anything could validate against.
        let e = verify_colored_child_shape(&sample_child(false, true))
            .expect_err("a coloured child under a PLAIN parent has no chain to validate");
        assert!(e.to_string().contains("colour mismatch"), "got: {e}");
        // Both halves agreeing is accepted (plain/plain is the pre-CTES-R world, unchanged).
        verify_colored_child_shape(&sample_child(false, false))
            .expect("a plain child under a plain parent must keep verifying");
    }

    /// The child-side interlock: once a child can carry an allocation, the child-level split and the
    /// onward re-transfer would each build an UNCOLOURED tier over a sealed output. They were
    /// vacuously safe only while no child was ever coloured.
    #[test]
    fn colored_children_refuse_every_uncolored_child_replacement() {
        let plain = sample_child(false, false);
        for what in ["child_in_ladder_split", "child_retransfer"] {
            assert!(
                refuse_uncolored_over_colored_child(&plain, what).is_ok(),
                "a plain child must keep taking {what}"
            );
        }
        let colored = sample_child(true, true);
        for what in ["child_in_ladder_split", "child_retransfer"] {
            let e = refuse_uncolored_over_colored_child(&colored, what)
                .expect_err("{what} over a coloured child destroys the allocation");
            assert!(e.to_string().contains("destroying it"), "got: {e}");
        }
    }

    /// **`SP` must not inherit `S_0`'s blinding.** They are RIVAL transitions over the same `X_m`
    /// payload output. Sharing a blinding collapses them to one `OpId`, after which rgb-lib keeps
    /// whichever witness has the smaller INTERNAL txid — an arbitrary hash lottery, and the loser's
    /// consignment validates for nobody (CTESR-GATE §2.2).
    ///
    /// This also pins why `TesrBundle::colored_tier_seals` cannot be reused for a split parent: it
    /// hard-codes `TierRole::State` for the current state, which for a split parent IS `SP`.
    #[test]
    fn the_split_state_seal_is_distinct_from_every_rival_over_x_m() {
        use crate::rgb::TierRole;
        let p = mercurylib::tesr::TesrParams::regtest();
        let s0_csv = p.state_csv(0);
        let sp_csv = s0_csv - p.delta;

        let s0 = colored_tier_seal("sid", TierRole::State, 0, 0, Some(s0_csv)).blinding();
        let sp = colored_tier_seal("sid", TierRole::SplitState, 0, 0, Some(sp_csv)).blinding();
        // A renewal-era state is a THIRD rival over the same outpoint — >=3, per the rival rule.
        let s_renew = colored_tier_seal("sid", TierRole::State, 0, 1, Some(s0_csv)).blinding();
        let all = [s0, sp, s_renew];
        for (i, a) in all.iter().enumerate() {
            for (j, b) in all.iter().enumerate() {
                if i != j {
                    assert_ne!(a, b, "rivals {i} and {j} over X_m share a blinding — they collapse");
                }
            }
        }
        // The role alone separates them even at an IDENTICAL rung, which is the case a
        // CSV-derivation bug would produce.
        assert_ne!(
            colored_tier_seal("sid", TierRole::State, 0, 0, Some(sp_csv)).blinding(),
            colored_tier_seal("sid", TierRole::SplitState, 0, 0, Some(sp_csv)).blinding(),
            "role must separate SP from S_0 independently of the rung"
        );
    }

    /// A child's own tiers must not collide with its PARENT's, nor with a sibling's. The child rungs
    /// are derived under the CHILD's statechain id and under roles no parent tier uses, so the two
    /// separators are independent.
    #[test]
    fn child_tier_seals_collide_with_nothing() {
        use crate::rgb::TierRole;
        let p = mercurylib::tesr::TesrParams::regtest();
        let (ce, cd) = (p.ext_csv(0), p.state_csv(0));
        let mut seen = std::collections::HashSet::new();
        for sid in ["sid", "child-a", "child-b"] {
            for (role, csv) in [
                (TierRole::Trigger, None),
                (TierRole::Extension, Some(ce)),
                (TierRole::State, Some(cd)),
                (TierRole::SplitState, Some(cd - p.delta)),
                (TierRole::ChildExtension, Some(ce)),
                (TierRole::ChildState, Some(cd)),
            ] {
                let b = colored_tier_seal(sid, role, 0, 0, csv).blinding();
                assert!(seen.insert(b), "blinding collision at {sid}/{role:?}");
            }
        }
        // The stable wire tags of the two new roles, pinned. A renumber silently re-blinds every
        // coloured child in flight.
        assert_eq!(TierRole::ChildExtension.tag(), 0x0A);
        assert_eq!(TierRole::ChildState.tag(), 0x0B);
    }

    /// The coloured CHILD floor, as arithmetic. A child ladder is headless — two rungs, not three —
    /// so it is cheaper than a root coloured ladder but dearer than the plain child floor by exactly
    /// the two oprets it must pay for.
    #[test]
    fn colored_child_floor_price() {
        let rate = 2.0;
        // RE-DERIVED, not weakened ([D4]): a coloured rung is 576 sat, not 574. The tier's signed
        // vsize is 168 vB — `crate::rgb::COLORED_TIER_VBYTES`, measured on a production-finalised
        // transaction — because a TES-R taproot signature carries an explicit SIGHASH_ALL byte and
        // so is 65 witness bytes, not 64. 168 * 2 + 240 = 576.
        assert_eq!(
            colored_child_floor(rate, COLORED_LADDER_DUST),
            2 * 576 + 330,
            "two coloured rungs plus a spendable final output"
        );
        assert!(
            colored_child_floor(rate, COLORED_LADDER_DUST)
                < colored_ladder_floor(rate, COLORED_LADDER_DUST),
            "a headless child must be cheaper than a full root ladder"
        );
        let plain = mercurylib::tesr::min_child_value(rate, COLORED_LADDER_DUST);
        // RE-DERIVED ([D4], BOTH halves): two rungs x 43 vB of opret x 2 sat/vB.
        //
        // This read `2 * 88` while [D4] was fixed on the coloured half only: the extra vB per rung
        // was the explicit SIGHASH_ALL byte, which is on EVERY tier, and it showed up as a coloured
        // "surcharge" purely because `mercurylib::tesr::TIER_VBYTES` still modelled a 64-byte
        // SIGHASH_DEFAULT witness (124 vB for a transaction that measures 125). With that constant
        // corrected the surcharge is what it always physically was: the opret and nothing else.
        assert_eq!(
            colored_child_floor(rate, COLORED_LADDER_DUST) - plain,
            2 * (mercurylib::tesr::P2TR_OUT_VBYTES * 2),
            "the coloured child surcharge is exactly two oprets"
        );
        assert_eq!(colored_child_floor(rate, COLORED_LADDER_DUST) - plain, 2 * 86);
        // THE MIXED-WALLET TRAP, pinned as arithmetic and now ESCAPED. The legacy 1,500-sat token
        // piece cleared the coloured CHILD floor, so a coloured split would carve one — but not the
        // coloured ROOT floor, so the moment its receiver claimed it as a root coin the colouring
        // was refused. Retiring the flat lane at that value strands every received piece.
        //
        // The interval `(child_floor, root_floor)` is the trap, and it is a property of THESE two
        // functions, so it is pinned here; `TOKEN_PIECE_SATS` was moved OUT of it (1_500 → 3_066 =
        // the root floor at twice the committed rate, [D4]-corrected from 3_054) and that move is
        // pinned, with the constant itself in scope, by
        // `mercury_utexo_sdk::tokens::token_piece_sats_is_the_coloured_root_floor`.
        // This crate cannot see the SDK's constant, so the two halves of the statement live in the
        // two crates that own them.
        assert!(1_500 > colored_child_floor(rate, COLORED_LADDER_DUST));
        assert!(1_500 < colored_ladder_floor(rate, COLORED_LADDER_DUST));
        assert!(
            3_066 >= colored_ladder_floor(rate, COLORED_LADDER_DUST),
            "the replacement piece size must clear the coloured ROOT floor"
        );
    }

    /// **[CATS/V5] The coloured SPINE-TIP floor — 906 sat, one coloured rung plus dust.**
    ///
    /// Pinned as arithmetic and as an ORDERING. The ordering is the load-bearing half: the tip floor
    /// is 576 sat below the coloured child floor, so anything that lets the two swap places (or lets
    /// the tip floor be applied to a piece) admits a coloured piece that cannot fund its second
    /// coloured rung — and it dies inside the coloured child builder, after the carrier has been
    /// terminalized, destroying the allocation's only exit.
    #[test]
    fn colored_spine_tip_floor_price() {
        let rate = 2.0;
        assert_eq!(
            colored_spine_tip_floor(rate, COLORED_LADDER_DUST),
            576 + 330,
            "ONE coloured rung plus a spendable final output"
        );
        assert_eq!(colored_spine_tip_floor(rate, COLORED_LADDER_DUST), 906);
        // The coloured surcharge over the plain tip is exactly ONE opret — half the child's, because
        // the tip builds half the rungs.
        let plain_tip = mercurylib::tesr::min_spine_tip_value(rate, COLORED_LADDER_DUST);
        assert_eq!(plain_tip, 820);
        assert_eq!(
            colored_spine_tip_floor(rate, COLORED_LADDER_DUST) - plain_tip,
            mercurylib::tesr::P2TR_OUT_VBYTES * 2,
            "the coloured tip surcharge is exactly ONE opret"
        );
        for r in [1.0f64, 2.0, 5.0, 10.0] {
            assert!(
                colored_spine_tip_floor(r, COLORED_LADDER_DUST)
                    < colored_child_floor(r, COLORED_LADDER_DUST),
                "rate {r}: one coloured rung must be cheaper than two"
            );
        }
    }

    /// PRE-EXISTING, corrected (not weakened): this used to assert `verify_bundle(&b, 3, 0).is_ok()`
    /// and had been RED since the `[S1]` fix, because `sample_bundle` builds UNSIGNED tiers and every
    /// counted tier must now verify as a genuine co-sign by `A`. A structurally perfect but
    /// never-co-signed ladder must be REFUSED — that is the property, and it is what is asserted here.
    /// The positive control (an honest, live-SE-co-signed ladder is ACCEPTED) cannot be built without
    /// the SE and lives in the E2Es: sdk46, sdk48, sdk54 and sdk70.
    #[test]
    fn rejects_a_structurally_perfect_but_uncosigned_ladder() {
        let b = sample_bundle();
        // trigger + extension + state = 3 tiers, 0 flat backups — the COUNT balances exactly, so the
        // only thing left to refuse on is the absent signature.
        let e = verify_bundle(&b, 3, 0).expect_err("an un-co-signed ladder must never be accepted");
        assert!(
            e.to_string().contains("not co-signed by A"),
            "must reject on the missing co-sign, got: {e}"
        );
    }

    /// The scriptPubKey of the sample bundle's aggregate address, hex — what a coin's on-chain
    /// funding output would carry.
    fn agg_spk_hex() -> String {
        use electrum_client::bitcoin::{Address, Network};
        let spk = Address::from_str(AGG)
            .unwrap()
            .require_network(Network::Regtest)
            .unwrap()
            .script_pubkey();
        hex::encode(spk.as_bytes())
    }

    /// The authority a receiver derives from the coin the sample bundle claims to describe.
    fn sample_authority() -> CoinAuthority {
        CoinAuthority {
            statechain_id: "sid".into(),
            f_txid: F_TXID.into(),
            f_vout: 0,
            f_value: 100_000,
            f_spk_hex: agg_spk_hex(),
            // A real one is the coordinator's untweaked aggregate; the tests below all fire on checks
            // that precede it, or on its absence.
            se_aggregate_pubkey: None,
        }
    }

    fn reject_msg(r: Result<()>) -> String {
        r.expect_err("must be rejected").to_string()
    }

    // ---- CTES-R: colour the ladder (this commit). ----------------------------------------------

    /// **[CATS] The spine CSV must be OUTSIDE every schedule's state range, on every profile.**
    ///
    /// §4.5 of `PARTIAL-PAYMENT-ECONOMICS.md` insists the spine be a NEW tier kind rather than a
    /// widened state range, and this is the property that makes that distinction real rather than
    /// nominal. If `SPINE_CSV` were ever a legal `state` CSV, then "spine" and "state" would be two
    /// names for one admissible interval: a state tier could be un-timelocked (the [B1] shape) and a
    /// spine tier could quietly carry a real timelock, making every payee's exit thousands of blocks
    /// slower with nothing to refuse it. `d_floor > SPINE_CSV` is what keeps the two disjoint, and it
    /// is a property of the SCHEDULES, so it is asserted against each shipped one.
    ///
    /// It also underwrites the builders' `s0_csv <= SPINE_CSV` guards: replace-by-lower-timelock
    /// needs the spine strictly below whatever it supersedes, and every state a spine can supersede
    /// is ≥ `d_floor`.
    #[test]
    fn the_spine_csv_is_not_a_legal_state_csv_on_any_profile() {
        for (name, p) in [
            ("mainnet", mercurylib::tesr::TesrParams::mainnet()),
            ("regtest", mercurylib::tesr::TesrParams::regtest()),
        ] {
            assert!(
                p.d_floor > SPINE_CSV,
                "{name}: d_floor {} must exceed the spine CSV {SPINE_CSV}, or the spine is just a \
                 widened state range",
                p.d_floor
            );
            assert!(
                p.e_floor > SPINE_CSV,
                "{name}: e_floor {} must exceed the spine CSV {SPINE_CSV} too — an extension is \
                 never un-timelocked",
                p.e_floor
            );
            // …and the deepest a state can ever walk is still above it, so no amount of rung
            // consumption lands a state ON the spine value.
            let deepest = p.state_csv(u16::MAX);
            assert!(
                deepest > SPINE_CSV,
                "{name}: a state walked to its floor is {deepest}, which must still exceed \
                 {SPINE_CSV}"
            );
        }
    }

    /// The coloured-rung price and the three-rung floor, pinned as arithmetic rather than prose.
    /// `docs/utexo/CTESR-GATE.md` §3.4 as corrected by [D4]: a coloured tier is one whole extra
    /// P2TR output wide (the opret serialises to exactly `P2TR_OUT_VBYTES`) and nothing else, so a
    /// coloured rung is `ceil(crate::rgb::colored_tier_vbytes(1) * rate) + P2A_VALUE` and an
    /// uncoloured one is `ceil(mercurylib::tesr::TIER_VBYTES * rate) + P2A_VALUE`, with
    /// `TIER_VBYTES + P2TR_OUT_VBYTES == COLORED_TIER_VBYTES`. The explicit `SIGHASH_ALL` byte
    /// [D4] found is a cost of EVERY tier, and is now carried by both constants rather than by the
    /// coloured one alone.
    #[test]
    fn colored_rung_price_and_floor() {
        let rate = 2.0;
        let rung = crate::rgb::colored_committed_fee(1, rate) + mercurylib::tesr::P2A_VALUE;
        // RE-DERIVED, not weakened: 576, not 574. `crate::rgb::COLORED_TIER_VBYTES` = 168 vB is
        // MEASURED on a production-finalised tier, and 168 * 2 + 240 = 576. The old 574 came from
        // (124 + 43) * 2 + 240, i.e. a 167-vB model of a 168-vB transaction.
        assert_eq!(crate::rgb::colored_tier_vbytes(1), 168);
        assert_eq!(rung, 576, "a coloured rung at 2 sat/vB is 168 * 2 + 240");
        // Strictly dearer than an uncoloured rung, by exactly 43 vB of opret — and by NOTHING else.
        // [D4] is now fixed on both halves: `mercurylib::tesr::TIER_VBYTES` was 124 (a 64-byte
        // SIGHASH_DEFAULT witness) for a transaction that measures 125, so the sighash byte — a cost
        // every TES-R tier pays — masqueraded as a coloured surcharge and this assertion read 88.
        // With TIER_VBYTES = 125 the identity 125 + 43 == 168 holds and the surcharge is 43 * rate.
        let plain = mercurylib::tesr::committed_fee(rate) + mercurylib::tesr::P2A_VALUE;
        assert_eq!(plain, 490, "an uncoloured rung at 2 sat/vB is 125 * 2 + 240");
        assert_eq!(rung - plain, mercurylib::tesr::P2TR_OUT_VBYTES * 2);
        assert_eq!(rung - plain, 86, "the coloured surcharge is exactly the opret's 43 vB * 2 sat/vB");
        assert_eq!(
            colored_ladder_floor(rate, COLORED_LADDER_DUST),
            3 * 576 + 330,
            "three coloured rungs plus a spendable final output"
        );
        // The LEGACY 1,500-sat token piece cannot afford a coloured ladder — the case that must
        // be caught BEFORE the first co-sign, not at rung 3. (`TOKEN_PIECE_SATS` has since been
        // re-derived to 3_054 so that new pieces clear this floor; old 1,500-sat pieces still exist
        // in wallets, so the pre-flight gate this pins is still load-bearing.)
        assert!(1_500 < colored_ladder_floor(rate, COLORED_LADDER_DUST));
    }

    /// A plain bundle is not coloured and every tier-replacing path stays open on it; a coloured one
    /// refuses them all. This is the interlock that keeps an UNCOLOURED tier from ever being built
    /// over a sealed output.
    #[test]
    fn colored_ladders_refuse_every_uncolored_replacement() {
        let plain = sample_bundle();
        assert!(!plain.is_colored());
        for what in ["renew", "rollover", "presign_receiver_state", "in_ladder_split"] {
            assert!(
                refuse_uncolored_over_colored(&plain, what).is_ok(),
                "a plain ladder must keep taking {what}"
            );
        }
        let mut colored = sample_bundle();
        colored.rgb = Some(ColoredLadder {
            contract_id: "rgb:contract".into(),
            amount: 1000,
            consignments: vec!["a".into(), "b".into(), "c".into()],
        });
        assert!(colored.is_colored());
        for what in ["renew", "rollover", "presign_receiver_state", "in_ladder_split"] {
            let e = refuse_uncolored_over_colored(&colored, what)
                .expect_err("a coloured ladder must refuse an uncoloured replacement")
                .to_string();
            assert!(e.contains("COLOURED") && e.contains(what), "unexpected refusal: {e}");
        }
    }

    /// **RIVAL TIERS OVER ONE PARENT OUTPUT NEVER SHARE A BLINDING** — the whole point of
    /// `colored_tier_seal`, pinned over the full renewal-and-transfer schedule rather than a pair.
    ///
    /// Over the trigger's payload output the rivals are the extensions of every renewal epoch; over
    /// each extension's payload output they are the renewal's own state plus every transfer's `S'`.
    /// The derivation must separate all of them, and it must also keep the two ROLES apart, since
    /// `role` is the only thing distinguishing an extension from a state at the same `(m, csv)`.
    #[test]
    fn rival_tiers_over_one_parent_never_share_a_blinding() {
        use crate::rgb::TierRole;
        let p = mercurylib::tesr::TesrParams::regtest();
        let mut all: std::collections::HashMap<u64, String> = std::collections::HashMap::new();
        let mut note = |b: u64, what: String| {
            if let Some(prev) = all.insert(b, what.clone()) {
                panic!("seal blinding collision: {what} and {prev} both derive {b}");
            }
        };
        note(colored_tier_seal("sid", TierRole::Trigger, 0, 0, None).blinding(), "T".into());
        // 10 renewal epochs, stepping the extension CSV by 1 (the shape sdk74 drives).
        for m in 0..10u32 {
            let csv_e = p.e0 - m as u16;
            note(
                colored_tier_seal("sid", TierRole::Extension, 0, m, Some(csv_e)).blinding(),
                format!("X(m={m},csv={csv_e})"),
            );
            // At each epoch: the renewal's own state, then one `S'` per onward hop, each a delta lower.
            for k in 0..3u16 {
                let csv_d = p.state_csv(0) - k * p.delta;
                note(
                    colored_tier_seal("sid", TierRole::State, 0, m, Some(csv_d)).blinding(),
                    format!("S(m={m},csv={csv_d})"),
                );
            }
        }
        // A DIFFERENT coin at the identical schedule must not collide with any of the above.
        for m in 0..10u32 {
            let csv_e = p.e0 - m as u16;
            note(
                colored_tier_seal("other", TierRole::Extension, 0, m, Some(csv_e)).blinding(),
                format!("other X(m={m})"),
            );
        }
    }

    /// The seal schedule a RECEIVER derives is exactly the one the sender used, and it is refused
    /// outright for a shape whose renewal counters cannot be reconstructed.
    #[test]
    fn a_receiver_derives_the_same_seals_and_refuses_what_it_cannot() {
        use crate::rgb::TierRole;
        let mut b = sample_bundle();
        b.m = 4;
        b.rgb = Some(ColoredLadder {
            contract_id: "rgb:c".into(),
            amount: 7,
            consignments: vec!["a".into(), "b".into(), "c".into()],
        });
        let seals = b.colored_tier_seals().expect("single-level coloured ladder");
        assert_eq!(seals.len(), 3);
        assert_eq!(
            seals[1].2,
            colored_tier_seal("sid", TierRole::Extension, 0, 4, b.current().extension.csv)
                .blinding(),
            "the receiver's extension blinding must be the sender's"
        );
        assert_eq!(
            seals[2].2,
            colored_tier_seal("sid", TierRole::State, 0, 4, b.current().state.csv).blinding()
        );
        assert_ne!(seals[1].2, seals[2].2, "role must separate an extension from a state");
        // A PLAIN ladder has no seals at all, and a multi-level coloured one is refused rather than
        // guessed at (its per-level counters are unreconstructable — coloured rollover does not exist).
        assert!(sample_bundle().colored_tier_seals().is_err());
        let extra = b.levels[0].clone();
        b.levels.push(extra);
        assert!(b.colored_tier_seals().unwrap_err().to_string().contains("exactly one level"));
    }

    /// Colour is STRUCTURAL and is checked on every acceptance path, with no RGB engine: a bundle
    /// cannot claim colour it does not carry, nor carry colour it does not claim.
    #[test]
    fn the_census_refuses_a_bundle_that_lies_about_colour() {
        // A plain ladder passes unchanged — the gate costs the plain path nothing.
        assert!(verify_colored_shape(&sample_bundle()).is_ok());

        // CLAIMS colour, carries none: the sample tiers are `[payload, P2A]`, no opret. A receiver
        // would derive seals for tiers that commit to nothing.
        let mut lying = sample_bundle();
        lying.rgb = Some(ColoredLadder {
            contract_id: "rgb:c".into(),
            amount: 1,
            consignments: vec!["a".into(), "b".into(), "c".into()],
        });
        assert!(
            reject_msg(verify_bundle(&lying, 3, 0)).contains("OP_RETURN"),
            "a bundle claiming colour whose tiers carry no opret must be refused"
        );

        // CARRIES colour, claims none: `rgb: None` with opret-bearing tiers. That is a coloured
        // ladder conveyed as plain — the receiver binds the sats and validates no asset at all.
        let mut smuggled = sample_bundle();
        {
            use electrum_client::bitcoin::{consensus::deserialize, ScriptBuf, Transaction, TxOut};
            let raw = hex::decode(&smuggled.trigger.signed_tx).unwrap();
            let mut tx: Transaction = deserialize(&raw).unwrap();
            tx.output.insert(
                0,
                TxOut { value: 0, script_pubkey: ScriptBuf::new_op_return(&[0u8; 32]) },
            );
            smuggled.trigger.payload_vout = 1;
            smuggled.trigger.txid = tx.txid().to_string();
            smuggled.trigger.signed_tx =
                hex::encode(electrum_client::bitcoin::consensus::serialize(&tx));
        }
        assert!(
            reject_msg(verify_bundle(&smuggled, 3, 0)).contains("conveyed as PLAIN"),
            "a coloured tier smuggled into a bundle declared PLAIN must be refused"
        );
    }

    /// The colour field is additive on the wire: a `tesr-` row persisted BEFORE CTES-R must keep
    /// deserializing byte-identically, as a PLAIN ladder. `#[serde(default)]` is what guarantees it,
    /// and this pins it so a future edit cannot quietly drop the attribute.
    #[test]
    fn pre_ctesr_bundles_still_deserialize_as_plain() {
        let plain = sample_bundle();
        let mut v: serde_json::Value = serde_json::to_value(&plain).unwrap();
        // Exactly the shape a pre-CTES-R client wrote: no `rgb` key at all.
        v.as_object_mut().unwrap().remove("rgb");
        let back: TesrBundle = serde_json::from_value(v).unwrap();
        assert!(!back.is_colored(), "an absent `rgb` field must mean PLAIN, never a parse failure");
        assert_eq!(back.trigger.txid, plain.trigger.txid);
        // And a plain bundle round-trips unchanged.
        let round: TesrBundle = serde_json::from_str(&serde_json::to_string(&plain).unwrap()).unwrap();
        assert_eq!(round.rgb, None);
    }

    // ---- payload_vout: every migrated site fails CLOSED with a named error (CTES-R commit 1). ----
    //
    // These run BEFORE the signature battery, so they are exercisable without the SE — which is the
    // point: a wrong payload vout is caught structurally, early, and loudly.

    #[test]
    fn wrong_trigger_payload_vout_is_rejected() {
        let mut b = sample_bundle();
        b.trigger.payload_vout = 1; // the P2A anchor, not the payload
        assert!(reject_msg(verify_bundle(&b, 3, 0)).contains("does not pay the aggregate key A"));
    }

    #[test]
    fn out_of_range_payload_vout_is_rejected_not_defaulted() {
        let mut b = sample_bundle();
        b.trigger.payload_vout = 9;
        let e = reject_msg(verify_bundle(&b, 3, 0));
        assert!(e.contains("out of range"), "must fail closed on the accessor, got: {e}");
    }

    #[test]
    fn wrong_extension_payload_vout_is_rejected() {
        let mut b = sample_bundle();
        b.levels[0].extension.payload_vout = 1;
        assert!(reject_msg(verify_bundle(&b, 3, 0)).contains("tier 1 pays the wrong output"));
    }

    #[test]
    fn wrong_final_state_payload_vout_is_rejected() {
        let mut b = sample_bundle();
        b.levels[0].state.payload_vout = 1;
        assert!(reject_msg(verify_bundle(&b, 3, 0)).contains("tier 2 pays the wrong output"));
    }

    #[test]
    fn child_spending_a_non_payload_output_of_its_parent_is_rejected() {
        // Isolate the LINKAGE check: give the trigger a payload that really does sit at index 1 (swap
        // its two outputs) so the payee check passes, then hang the extension off `out[0]`. The chain
        // is broken and the linkage check — not the payee check — must say so.
        use electrum_client::bitcoin::{
            consensus::{deserialize, serialize},
            Transaction,
        };
        let mut b = sample_bundle();
        let mut t: Transaction = deserialize(&hex::decode(&b.trigger.signed_tx).unwrap()).unwrap();
        t.output.swap(0, 1);
        let t_txid = t.txid().to_string();
        let payload_value = t.output[1].value;
        let p = b.params;
        let x = mercurylib::tesr::build_extension(
            &t_txid, payload_value, AGG, "regtest", p.ext_csv(0), p.committed_fee_rate,
        )
        .unwrap();
        b.trigger = TesrTier {
            txid: t_txid,
            signed_tx: hex::encode(serialize(&t)),
            out_value: payload_value,
            csv: None,
            payload_vout: 1,
        };
        b.levels[0].extension = TesrTier {
            txid: x.txid,
            signed_tx: x.tx_hex,
            out_value: x.out_value,
            csv: Some(p.ext_csv(0)),
            payload_vout: x.payload_vout,
        };
        let e = reject_msg(verify_bundle(&b, 3, 0));
        assert!(e.contains("does not spend its parent's payload output"), "got: {e}");
    }

    // ---- [C-1] the acceptance-path verifier is bound to the COIN, not to the bundle. ----

    #[test]
    fn bound_verifier_rejects_a_ladder_for_another_statechain_id() {
        let b = sample_bundle();
        let mut coin = sample_authority();
        coin.statechain_id = "a-different-sid".into();
        assert!(reject_msg(verify_bundle_bound(&b, 3, 0, &coin)).contains("statechain id"));
    }

    #[test]
    fn bound_verifier_rejects_a_ladder_over_another_outpoint() {
        let b = sample_bundle();
        let mut coin = sample_authority();
        coin.f_txid = "2".repeat(64);
        assert!(reject_msg(verify_bundle_bound(&b, 3, 0, &coin)).contains("decoy ladder"));
        let mut coin = sample_authority();
        coin.f_vout = 7;
        assert!(reject_msg(verify_bundle_bound(&b, 3, 0, &coin)).contains("decoy ladder"));
    }

    #[test]
    fn bound_verifier_rejects_a_restated_funding_value() {
        let b = sample_bundle();
        let mut coin = sample_authority();
        coin.f_value += 1;
        assert!(reject_msg(verify_bundle_bound(&b, 3, 0, &coin)).contains("F value"));
    }

    #[test]
    fn bound_verifier_rejects_an_aggregate_that_is_not_the_coins() {
        use electrum_client::bitcoin::{
            secp256k1::{Secp256k1, XOnlyPublicKey},
            Address, Network,
        };
        let b = sample_bundle();
        let mut coin = sample_authority();
        // A different, perfectly valid P2TR funding output — the ladder's tiers are co-signed under a
        // key that does not control it.
        let x = XOnlyPublicKey::from_slice(
            &hex::decode("79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798").unwrap(),
        )
        .unwrap();
        coin.f_spk_hex = hex::encode(
            Address::p2tr(&Secp256k1::verification_only(), x, None, Network::Regtest)
                .script_pubkey()
                .as_bytes(),
        );
        let e = reject_msg(verify_bundle_bound(&b, 3, 0, &coin));
        assert!(e.contains("on-chain funding key"), "got: {e}");
    }

    #[test]
    fn bound_verifier_rejects_a_non_taproot_funding_output() {
        // `taproot_key_hex` on a non-P2TR spk — here the P2A anchor script, a witness program that is
        // NOT P2TR (the exact shape that broke `create_colored_split_tx`'s filter, CTESR-GATE §2.1d).
        let b = sample_bundle();
        let mut coin = sample_authority();
        coin.f_spk_hex = "51024e73".into();
        let e = reject_msg(verify_bundle_bound(&b, 3, 0, &coin));
        assert!(e.contains("not a v1 taproot output"), "got: {e}");
    }

    #[test]
    fn bound_verifier_fails_closed_without_a_coordinator_aggregate() {
        // Everything else matches; the coordinator simply has no aggregate on record for the sid.
        // That is a REJECTION, never a fallback to the sender-supplied key.
        let b = sample_bundle();
        let coin = sample_authority();
        assert!(coin.se_aggregate_pubkey.is_none());
        let e = reject_msg(verify_bundle_bound(&b, 3, 0, &coin));
        assert!(e.contains("recorded no aggregate"), "got: {e}");
    }

    // ---- [R5] The establish-time counterpart: a legacy coin is left UN-LADDERED, not bricked. ----
    //
    // `verify_bundle_bound`'s fail-closed-on-missing-aggregate is correct and stays exactly as it is.
    // Its cost is that a pre-0009 coin (`aggregate_xonly IS NULL`) which gets laddered in place carries
    // a bundle no receiver can bind. `ladder_binding_precheck` is the same authority test hoisted ahead
    // of establishment, so such a coin never acquires that ladder — and never burns the three
    // irreversible SE co-signs it would cost. It does NOT restore off-chain transferability (the [R4]
    // version floor refuses the un-laddered shape too); only a coordinator-side backfill of
    // `aggregate_xonly` does that.

    /// The generator's x-coordinate — a valid x-only key standing in for "some other aggregate".
    const OTHER_XONLY: &str =
        "79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";

    /// P2TR spk (hex) of an UNTWEAKED x-only key — what the coordinator's recorded aggregate looks
    /// like once it is on chain.
    fn p2tr_spk_hex_of(untweaked_xonly: &str) -> String {
        use electrum_client::bitcoin::{
            secp256k1::{Secp256k1, XOnlyPublicKey},
            Address, Network,
        };
        let x = XOnlyPublicKey::from_str(untweaked_xonly).unwrap();
        hex::encode(
            Address::p2tr(&Secp256k1::verification_only(), x, None, Network::Regtest)
                .script_pubkey()
                .as_bytes(),
        )
    }

    #[test]
    fn precheck_refuses_to_ladder_a_legacy_coin_with_no_coordinator_aggregate() {
        // A perfectly legitimate, live, on-chain root coin — the coordinator simply predates the
        // aggregate column. Laddering it would spend three SE co-signs to produce an unclaimable
        // bundle, so the pass must leave it alone.
        let e = ladder_binding_precheck("sid-legacy", &agg_spk_hex(), None, "regtest")
            .expect_err("a coin with no coordinator aggregate must never be laddered");
        assert!(e.to_string().contains("recorded no aggregate"), "got: {e}");

        // Why it must never be laddered: this is precisely the coin whose conveyed ladder the
        // acceptance path refuses. The refusal is NOT relaxed — the fix is upstream of it.
        let b = sample_bundle();
        let legacy = sample_authority(); // se_aggregate_pubkey: None
        assert!(legacy.se_aggregate_pubkey.is_none());
        assert!(
            reject_msg(verify_bundle_bound(&b, 3, 0, &legacy)).contains("recorded no aggregate"),
            "the acceptance-path fail-closed must stand unchanged"
        );
    }

    #[test]
    fn precheck_refuses_an_aggregate_that_does_not_control_the_funding_output() {
        // Not the legacy case: the coordinator HAS a record, it just is not the key on chain. A ladder
        // here would be refused as a decoy at acceptance, so it must not be built either.
        let e = ladder_binding_precheck("sid", &agg_spk_hex(), Some(OTHER_XONLY), "regtest")
            .expect_err("a mismatched coordinator aggregate must not be laddered");
        assert!(e.to_string().contains("does not match the funding output key"), "got: {e}");
    }

    #[test]
    fn precheck_fails_closed_on_a_non_taproot_funding_output() {
        let e = ladder_binding_precheck("sid", "51024e73", Some(OTHER_XONLY), "regtest")
            .expect_err("a non-P2TR funding output has no aggregate key to bind to");
        assert!(e.to_string().contains("not a v1 taproot output"), "got: {e}");
        assert!(
            ladder_binding_precheck("sid", "zz", Some(OTHER_XONLY), "regtest").is_err(),
            "unparseable spk must fail closed"
        );
    }

    /// The four refusal causes are DISTINGUISHABLE. A caller that collapses them into `is_err()`
    /// licenses "decoy-shaped coin" and "unreadable spk" as if they were the harmless legacy case —
    /// which is exactly the fail-open the flat-conveyance classifier used to have.
    #[test]
    fn precheck_reports_a_distinguishable_cause_for_every_refusal() {
        use BindingRefusal::*;
        let cases: &[(BindingRefusal, &str, Option<&str>)] = &[
            (NoCoordinatorAggregate, &"", None),
            (AggregateMismatch, &"", Some(OTHER_XONLY)),
            (FundingNotTaproot, "51024e73", Some(OTHER_XONLY)),
            (FundingSpkUnparseable, "zz", Some(OTHER_XONLY)),
        ];
        let agg = agg_spk_hex();
        for (expected, spk, se_agg) in cases {
            let spk = if spk.is_empty() { agg.as_str() } else { *spk };
            let e = ladder_binding_precheck_cause("sid", spk, *se_agg, "regtest")
                .expect_err("this shape must not be ladderable");
            assert_eq!(e.cause, *expected, "wrong cause for spk={spk} agg={se_agg:?}: {e}");
            // The untyped wrapper keeps producing the exact same prose.
            assert_eq!(
                ladder_binding_precheck("sid", spk, *se_agg, "regtest").unwrap_err().to_string(),
                e.message
            );
        }
        // ...and ONLY the legacy case is the permanent one a caller may license.
        assert_ne!(AggregateMismatch, NoCoordinatorAggregate);
    }

    #[test]
    fn precheck_passes_exactly_when_the_receiver_could_bind_the_ladder() {
        // The coordinator's untweaked aggregate, and the funding output that key actually controls.
        let f_spk_hex = p2tr_spk_hex_of(OTHER_XONLY);
        ladder_binding_precheck("sid", &f_spk_hex, Some(OTHER_XONLY), "regtest")
            .expect("a coin whose coordinator aggregate controls F is ladderable");

        // And the acceptance-path verifier agrees on the same coin: a ladder built over that aggregate
        // clears EVERY binding check and stops only at the SE co-signature, which no unit test can
        // forge. So `precheck ⟹ bindable`: the establish gate never creates an unclaimable ladder.
        use electrum_client::bitcoin::{
            secp256k1::{Secp256k1, XOnlyPublicKey},
            Address, Network,
        };
        let agg_addr = Address::p2tr(
            &Secp256k1::verification_only(),
            XOnlyPublicKey::from_str(OTHER_XONLY).unwrap(),
            None,
            Network::Regtest,
        )
        .to_string();
        let p = mercurylib::tesr::TesrParams::regtest();
        let f_value = 100_000u64;
        let t = mercurylib::tesr::build_trigger(F_TXID, 0, f_value, &agg_addr, "regtest", p.committed_fee_rate).unwrap();
        let x = mercurylib::tesr::build_extension(&t.txid, t.out_value, &agg_addr, "regtest", p.ext_csv(0), p.committed_fee_rate).unwrap();
        let s = mercurylib::tesr::build_state(&x.txid, x.out_value, OWNER, "regtest", p.state_csv(0), p.committed_fee_rate).unwrap();
        let mut b = sample_bundle();
        b.agg_address = agg_addr;
        b.trigger = TesrTier { txid: t.txid, signed_tx: t.tx_hex, out_value: t.out_value, csv: None, payload_vout: t.payload_vout };
        b.levels[0].extension = TesrTier { txid: x.txid, signed_tx: x.tx_hex, out_value: x.out_value, csv: Some(p.ext_csv(0)), payload_vout: x.payload_vout };
        b.levels[0].state = TesrTier { txid: s.txid, signed_tx: s.tx_hex, out_value: s.out_value, csv: Some(p.state_csv(0)), payload_vout: s.payload_vout };

        let coin = CoinAuthority {
            statechain_id: "sid".into(),
            f_txid: F_TXID.into(),
            f_vout: 0,
            f_value,
            f_spk_hex,
            se_aggregate_pubkey: Some(OTHER_XONLY.into()),
        };
        let e = reject_msg(verify_bundle_bound(&b, 3, 0, &coin));
        assert!(
            e.contains("not co-signed by A"),
            "binding must not be what fails for a precheck-passing coin, got: {e}"
        );
    }

    // ---- [C-2] one co-sign, one census slot. ----

    /// The dedup guard sits in `verify_superseded_segment`'s PRE-PASS, ahead of the co-sign battery,
    /// so it is exercisable without the SE — which matters, because the padding it stops is padding
    /// with GENUINE, fully co-signed tiers that every other check waves through. (The end-to-end
    /// version, against a real renewed ladder, is sdk70 PART C.)
    fn superseded_dedup(sup: &[TesrTier], live: &[TesrTier]) -> String {
        use electrum_client::bitcoin::{Address, Network, Txid};
        let b = sample_bundle();
        let agg_spk = Address::from_str(AGG)
            .unwrap()
            .require_network(Network::Regtest)
            .unwrap()
            .script_pubkey();
        let mut prevouts: std::collections::HashMap<(Txid, u32), u64> =
            std::collections::HashMap::new();
        let live_csv: std::collections::HashMap<(Txid, u32), u32> = std::collections::HashMap::new();
        let live_txids: std::collections::HashSet<Txid> = live
            .iter()
            .map(|t| Txid::from_str(&t.txid).unwrap())
            .collect();
        verify_superseded_segment(
            sup,
            &[],
            &agg_spk,
            &b.params,
            &mut prevouts,
            &live_csv,
            &live_txids,
        )
        .expect_err("a repeated tier must be rejected")
        .to_string()
    }

    #[test]
    fn a_disclosed_tier_cannot_be_counted_twice() {
        // Repeat a superseded entry: `expected` grows by one for free and absorbs a hidden co-sign.
        let s = sample_bundle().levels[0].state.clone();
        let e = superseded_dedup(&[s.clone(), s], &[]);
        assert!(e.contains("disclosed more than once"), "got: {e}");
    }

    #[test]
    fn a_live_tier_cannot_also_be_disclosed_as_superseded() {
        // Already counted once by `tiers.len()`; re-declaring it buys the same free slot.
        let b = sample_bundle();
        let x = b.levels[0].extension.clone();
        let e = superseded_dedup(&[x.clone()], &[x]);
        assert!(e.contains("disclosed more than once"), "got: {e}");
    }

    #[test]
    fn rejects_hidden_extra_sig() {
        let b = sample_bundle();
        // SE issued one MORE sig than the ladder accounts for → a hidden state → reject.
        assert!(verify_bundle(&b, 4, 0).is_err());
    }

    #[test]
    fn rejects_undercount() {
        let b = sample_bundle();
        assert!(verify_bundle(&b, 2, 0).is_err());
    }

    #[test]
    fn rejects_broken_prevout_link() {
        let mut b = sample_bundle();
        // Point the extension at the wrong parent by corrupting the trigger txid it references:
        // rebuild the extension off a bogus parent.
        let p = b.params;
        let bogus = mercurylib::tesr::build_extension(
            "2222222222222222222222222222222222222222222222222222222222222222",
            b.trigger.out_value, AGG, "regtest", p.ext_csv(0), p.committed_fee_rate,
        ).unwrap();
        b.levels[0].extension = TesrTier { txid: bogus.txid, signed_tx: bogus.tx_hex, out_value: bogus.out_value, csv: Some(p.ext_csv(0)), payload_vout: bogus.payload_vout };
        assert!(verify_bundle(&b, 3, 0).is_err(), "extension not linked to the trigger must be rejected");
    }

    #[test]
    fn rejects_final_state_not_paying_owner() {
        let p = mercurylib::tesr::TesrParams::regtest();
        let mut b = sample_bundle();
        // Rebuild the final state paying A instead of the owner.
        let x = &b.levels[0].extension;
        let s = mercurylib::tesr::build_state(&x.txid, x.out_value, AGG, "regtest", p.state_csv(0), p.committed_fee_rate).unwrap();
        b.levels[0].state = TesrTier { txid: s.txid, signed_tx: s.tx_hex, out_value: s.out_value, csv: Some(p.state_csv(0)), payload_vout: s.payload_vout };
        assert!(verify_bundle(&b, 3, 0).is_err(), "final state must pay the owner");
    }
}

/// Blind-co-sign one TES-R tier transaction end-to-end against the SE and return the signed tx hex.
///
/// * `unsigned_tx_hex` — a tier tx built by `mercurylib::tesr::build_{trigger,extension,state}`.
/// * `prevout_value`   — the value of the output this tier spends (the parent tier's `out[0]`, or the
///   funding UTXO value for the trigger). Every tier's prevout is a P2TR of the coin's aggregate key.
///
/// Fresh MuSig2 nonces are committed per call, so co-signing several tiers of one coin never reuses a
/// secnonce (the SE would refuse a reused nonce with 409 anyway).
pub async fn cosign_tier(
    client_config: &ClientConfig,
    coin: &mut Coin,
    unsigned_tx_hex: String,
    prevout_value: u64,
    network: &str,
) -> Result<String> {
    let coin_nonce = mercurylib::transaction::create_and_commit_nonces(coin)?;
    coin.secret_nonce = Some(coin_nonce.secret_nonce);
    coin.public_nonce = Some(coin_nonce.public_nonce);
    coin.blinding_factor = Some(coin_nonce.blinding_factor);

    let server_public_nonce =
        sign_first(client_config, &coin_nonce.sign_first_request_payload).await?;
    coin.server_public_nonce = Some(server_public_nonce);

    let partial = cosign_tier_request(coin, unsigned_tx_hex, prevout_value, network.to_string())?;

    // [KEYSTONE / client half] Retry the SAME sign/second — NEVER restart sign/first here.
    //
    // Restarting sign/first would mint a fresh secnonce and a fresh SE co-sign, so sig_count would run
    // ahead of the one tier we actually keep, and the receiver census (num_sigs == flat_backups + tiers +
    // superseded) could never rebalance ⟹ the coin bricks. Resending the IDENTICAL payload is idempotent
    // at the lockbox (same session ⟹ cached partial sig, no re-sign, no re-increment — see the lockbox
    // signed_session_cache), so a lost sign/second response is recovered with the exact same signature.
    //
    // Retrying the same session is ALWAYS safe: it can only return the already-produced sig, produce it
    // once (secnonce still sealed), or 400 (a different session already consumed the secnonce — which
    // cannot happen for THIS session). If every attempt fails, we surface the error; the only
    // unrecoverable case (a crash between the enclave consume and the atomic store) left NO count
    // increment, so a caller that restarts the whole cosign is also safe. Bounded so a genuinely down SE
    // still returns promptly.
    let mut server_partial_sig = None;
    let mut last_err = None;
    for attempt in 0u32..5 {
        match sign_second(client_config, &partial.partial_signature_request_payload).await {
            Ok(sig) => {
                server_partial_sig = Some(sig);
                break;
            }
            Err(e) => {
                last_err = Some(e);
                if attempt < 4 {
                    tokio::time::sleep(std::time::Duration::from_millis(300 * (attempt as u64 + 1))).await;
                }
            }
        }
    }
    let server_partial_sig = match server_partial_sig {
        Some(sig) => sig,
        None => return Err(anyhow::anyhow!(
            "sign/second failed after retries (session unchanged, no double-count): {}",
            last_err.map(|e| e.to_string()).unwrap_or_default()
        )),
    };

    let signature = create_signature(
        partial.msg,
        partial.client_partial_sig,
        hex::encode(server_partial_sig.serialize()),
        partial.encoded_session,
        partial.output_pubkey,
    )?;

    let signed_tx = new_backup_transaction(partial.encoded_unsigned_tx, signature)?;
    Ok(signed_tx)
}

/// **F4 — a blind pass must never look like an idle one.**
///
/// These are the regression tests for the external review's finding that the TES-R watch/exit
/// passes returned success-like defaults (`false` from every chain read, an empty `Vec<String>`
/// from the pass) whenever the backend failed. An empty result was the *only* signal available to a
/// caller, and a healthy quiet coin produced exactly the same one — so a tower with a dead electrum
/// connection reported "all quiet" while defending nothing, during precisely the race window in
/// which being blind loses the coin.
#[cfg(test)]
mod watch_visibility_tests {
    use super::*;
    use super::verify_tests::sample_bundle;

    /// An electrum backend that is REACHABLE but unusable: it completes the TCP handshake and then
    /// hangs up on every request. `Client::new` performs no handshake, so the client builds fine and
    /// each RPC fails at the transport — the realistic "the backend fell over" shape, and the one
    /// the old code silently read as "F is unspent / the tx is not on chain".
    fn dead_electrum() -> electrum_client::Client {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                drop(stream); // accept, then immediately hang up
            }
        });
        electrum_client::Client::new(&format!("tcp://127.0.0.1:{port}")).expect("connect")
    }

    /// THE finding, stated as an assertion: with an unreadable backend the pass reports `Blind`,
    /// and `Blind` is *not* `Idle` even though both carry no ids. Before F4 this returned `vec![]`
    /// — byte-identical to the healthy "F unspent, nothing to defend" answer.
    #[test]
    fn a_blind_backend_reports_blind_not_idle() {
        let state = watch_pass(&dead_electrum(), &sample_bundle());
        assert!(state.is_blind(), "an unreadable backend must report Blind, got {state:?}");
        assert!(!state.is_idle(), "Blind must never satisfy is_idle — that is the whole finding");
        assert!(
            state.ids().is_empty(),
            "a blind pass acted on nothing, so emptiness alone can never mean 'all quiet'"
        );
        assert!(state.blind_reason().is_some(), "the cause must be reportable to the owner");
    }

    /// The positive control the assertion above needs: `Idle` is reserved for a pass that actually
    /// READ the chain. Nothing in the code can produce it from a failure.
    #[test]
    fn idle_is_only_ever_a_positive_observation() {
        assert!(WatchState::Idle.is_idle() && !WatchState::Idle.is_blind());
        let blind = WatchState::Blind { reason: "backend down".into() };
        assert!(!blind.is_idle() && blind.is_blind());
        assert_ne!(WatchState::Idle, blind);
        // An `Acted` pass that broadcast nothing (every mature tier already out) is ALSO not idle:
        // the coin is triggered and the exit is under way.
        let under_way = WatchState::Acted { ids: vec![], failures: vec!["csv not met".into()], blind: vec![] };
        assert!(!under_way.is_idle() && !under_way.is_blind());
        assert_eq!(under_way.failures().len(), 1);
    }

    /// The owner-driven exit must fail LOUD rather than report `complete: false, wait 0` — which an
    /// app renders as a healthy "just wait for the next block".
    #[test]
    fn a_blind_backend_makes_the_exit_pass_an_error_not_progress() {
        let e = exit_pass(&dead_electrum(), &sample_bundle())
            .expect_err("an unreadable backend must not be reported as exit progress");
        assert!(
            e.to_string().contains("chain backend unreadable"),
            "the error must name the blindness, got: {e}"
        );
    }

    /// ... and the wait-time hint must never fabricate a number, nor claim completion.
    #[test]
    fn a_blind_backend_makes_the_wait_hint_an_error_not_a_number() {
        let r = next_exit_tier(&dead_electrum(), &sample_bundle());
        assert!(r.is_err(), "a blind backend must not yield a wait time, got {r:?}");
    }

    /// The classifier that draws the line. A server that ANSWERS "no such transaction" gave a real,
    /// trustworthy negative; anything else — transport failure, retry exhaustion, or a server error
    /// we do not recognise — is blindness. It fails CLOSED, so an unfamiliar message costs an alert,
    /// never a silent "absent".
    #[test]
    fn only_an_explicit_no_such_transaction_counts_as_absence() {
        use electrum_client::Error;
        // The verbatim bitcoind/electrs reply, confirmed against the live regtest backend.
        assert!(is_missing_tx_error(&Error::Protocol(serde_json::json!({
            "code": 2,
            "message": "No such mempool or blockchain transaction. Use gettransaction for wallet transactions."
        }))));
        assert!(is_missing_tx_error(&Error::Protocol(serde_json::json!("transaction not found"))));

        // A server error that is NOT a not-found: the tx may well exist and we simply cannot see it.
        assert!(!is_missing_tx_error(&Error::Protocol(serde_json::json!({
            "code": -32000, "message": "excessive resource usage"
        }))));
        assert!(!is_missing_tx_error(&Error::Protocol(serde_json::json!({
            "code": -32603, "message": "daemon error"
        }))));
        // Transport / client-side failures are never an answer at all.
        assert!(!is_missing_tx_error(&Error::IOError(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "reset"
        ))));
        assert!(!is_missing_tx_error(&Error::AllAttemptsErrored(vec![])));
        assert!(!is_missing_tx_error(&Error::Message("no idea".into())));
        assert!(!is_missing_tx_error(&Error::CouldntLockReader));
        assert!(!is_missing_tx_error(&Error::Mpsc));
    }

    /// Stored material that cannot be parsed is a failure too, not a quiet skip: the old code
    /// `break`-ed out of the tier loop on a bad txid/hex and returned the tiers it happened to have
    /// broadcast, indistinguishable from a completed pass.
    #[test]
    fn unusable_stored_material_is_surfaced_not_skipped() {
        let mut b = sample_bundle();
        b.f_txid = "not-a-txid".into();
        let state = watch_pass(&dead_electrum(), &b);
        assert!(state.is_blind(), "an unusable funding txid must be reported, got {state:?}");
        assert!(
            state.blind_reason().unwrap().contains("unusable funding txid"),
            "the reason must name the bad material, got {:?}",
            state.blind_reason()
        );
    }

    /// **[CATS/V4] The spine-tip tower pass is on the SAME contract as the other two.** Written as a
    /// separate assertion rather than trusted from a copied body: a tip is the one slot whose only
    /// defence is this pass, so "it returns an empty vector when the backend is dead" would be the
    /// silent-degradation shape landing on the sender's whole change.
    #[test]
    fn a_blind_backend_makes_the_spine_tip_pass_blind_not_idle() {
        let tip = super::spine_tip_tests::sample_tip();
        let state = watch_spine_tip_pass(&dead_electrum(), &tip);
        assert!(state.is_blind(), "a dead backend must be Blind, got {state:?}");
        // …and the exit/wait-hint halves too — `Err`, never `complete: false, wait: 0`, which reads
        // exactly like a healthy exit still waiting for its next CSV.
        assert!(exit_spine_tip_pass(&dead_electrum(), &tip).is_err());
        assert!(next_spine_tip_exit_tier(&dead_electrum(), &tip).is_err());
    }
}

/// **[CATS/V4] The SPINE TIP record — the sender's own change leg, and what makes it not a leaf.**
#[cfg(test)]
mod spine_tip_tests {
    use super::*;
    use super::verify_tests::{sample_bundle, sample_child_for_tip_test, OWNER};

    /// A depth-1 tip over `sample_bundle`: the parent's own state tier stands in for the cap, which
    /// is exactly the right shape — a cap IS a state tier, hung one level lower.
    ///
    /// ⚠️ SHAPE ONLY. Its cap spends the parent's `X_m`, not `SP.out[1]`, so it does NOT satisfy
    /// [`SpineTipBundle::validate`] and must not be reused for anything that reads the signed
    /// transactions. Use `sample_valid_tip` for those.
    pub(super) fn sample_tip() -> SpineTipBundle {
        let parent = sample_bundle();
        let cap = parent.levels[0].state.clone();
        SpineTipBundle {
            parent_statechain_id: parent.statechain_id.clone(),
            sp_out_value: parent.levels[0].extension.out_value,
            parent,
            ancestors: vec![],
            sp_vout: 1,
            statechain_id: "tip-sid".into(),
            owner_exit_address: OWNER.into(),
            cap,
            superseded_caps: vec![],
            parent_flat_backups: vec![],
            rgb: None,
        }
    }

    /// **The tip's exit chain is exactly ONE tier longer than its parent's ladder.** That is the
    /// whole of change 2 stated as arithmetic: a piece adds `[extension, state]` and pays two rungs
    /// and 720 extra blocks; the tip adds one cap.
    ///
    /// The chain and its LABELS are built by two independent loops reconciled only by a length
    /// check, exactly as on the child lane — so the two are asserted together. A silent
    /// disagreement there does not fail loudly: if the lengths happen to match, `bind_declared_csv`
    /// compares one tier's declared timelock against another tier's signed one.
    #[test]
    fn the_tip_contributes_exactly_one_tier_and_its_labels_stay_in_lock_step() {
        let tip = sample_tip();
        let chain = spine_tip_exit_chain(&tip);
        assert_eq!(
            chain.len(),
            tip.parent.exit_tiers().len() + 1,
            "T, X_m, SP, then ONE cap — no extension between SP and the cap"
        );
        assert_eq!(chain.last().unwrap().0, tip.cap.signed_tx, "the cap is the last thing broadcast");
        assert_eq!(spine_tip_exit_labels(&tip).len(), chain.len());
        assert_eq!(spine_tip_exit_labels(&tip).last().unwrap(), "spine tip cap");

        // …and with an intermediate SPINE segment spliced in, both loops grow by exactly one, not
        // two. A two-tier ancestor grows them by two. This is the pairing that must hold.
        let mut deeper = sample_tip();
        deeper.ancestors.push(ChildSegment {
            statechain_id: "seg".into(),
            funding_vout: 0,
            extension: None,
            state: deeper.cap.clone(),
            superseded_states: vec![],
            superseded_extensions: vec![],
        });
        assert_eq!(spine_tip_exit_chain(&deeper).len(), chain.len() + 1);
        assert_eq!(spine_tip_exit_labels(&deeper).len(), chain.len() + 1);
        deeper.ancestors[0].extension = Some(deeper.cap.clone());
        assert_eq!(spine_tip_exit_chain(&deeper).len(), chain.len() + 2);
        assert_eq!(spine_tip_exit_labels(&deeper).len(), chain.len() + 2);
    }

    /// **A tip must not deserialize from a child bundle, or vice versa.** The record exists to keep
    /// the two apart at every reader, and the readers dispatch on the KEY PREFIX — so the types must
    /// also refuse each other's payloads, or a mis-keyed row would be read as the wrong shape rather
    /// than refused. `serde_json::from_str` is what every one of those readers calls.
    #[test]
    fn a_tip_and_a_child_bundle_are_not_interchangeable_payloads() {
        let tip = sample_tip();
        let json = serde_json::to_string(&tip).unwrap();
        // Round-trips as itself…
        let back: SpineTipBundle = serde_json::from_str(&json).unwrap();
        assert_eq!(back.statechain_id, "tip-sid");
        assert_eq!(back.cap.txid, tip.cap.txid);
        assert!(back.superseded_caps.is_empty());
        // …and is NOT a child bundle. A `ctesr-` reader handed this row refuses it (it has no
        // `child_extension`), which is what keeps the tip out of leaf handling even if a row is
        // written under the wrong key.
        assert!(serde_json::from_str::<ChildTesrBundle>(&json).is_err());
        // The converse, which is the direction that would strand the sender's change: a child
        // bundle's JSON has no `cap`, so it cannot be read as a tip either.
        let child_json = serde_json::to_string(&sample_child_for_tip_test()).unwrap();
        assert!(serde_json::from_str::<SpineTipBundle>(&child_json).is_err());
    }

    /// **The one-cap floors, wired end to end.** `SplitLegRole` is the only thing that selects a
    /// floor, and `change_leg_role()` is the only thing that selects the change leg's role — so the
    /// floor a payment is admitted at and the ladder the builder then constructs cannot disagree.
    ///
    /// The ORDERING assertions are the load-bearing ones: they fail if a future edit ever lets the
    /// tip's cheaper floor reach a PAYEE's piece, which is the [V5] hazard and which kills the coin
    /// after the parent is already terminal.
    #[test]
    fn the_leg_role_selects_the_floor_and_the_piece_floor_never_falls() {
        let (rate, dust) = (2.0f64, 330u64);
        assert_eq!(SplitLegRole::Piece.min_value(rate, dust), 1_310);
        assert_eq!(SplitLegRole::SpineTip.min_value(rate, dust), 820);
        assert_eq!(SplitLegRole::Piece.colored_min_value(rate, COLORED_LADDER_DUST), 1_482);
        assert_eq!(SplitLegRole::SpineTip.colored_min_value(rate, COLORED_LADDER_DUST), 906);
        for r in [1.0f64, 2.0, 5.0, 25.0] {
            assert!(SplitLegRole::SpineTip.min_value(r, dust) < SplitLegRole::Piece.min_value(r, dust));
            assert!(
                SplitLegRole::SpineTip.colored_min_value(r, COLORED_LADDER_DUST)
                    < SplitLegRole::Piece.colored_min_value(r, COLORED_LADDER_DUST)
            );
        }
        // And the DECLARED state of change 2. This assertion is meant to be edited — in the same
        // commit as the builder change and no earlier. Flipped early, the wallet admits a change leg
        // it then cannot build a ladder for, AFTER `set_spend_budget` has terminalized the parent.
        assert_eq!(
            change_leg_role(),
            SplitLegRole::Piece,
            "the split builders still give the change leg two tiers; the floor must say so"
        );
    }

    /// A tip whose material is REAL: `SP` is a genuine 2-payload split state over `X_m.out[0]`, and
    /// the cap is a genuine state tier built over `SP.out[1]`. [`SpineTipBundle::validate`] reads
    /// the signed transactions, so a fixture of hand-written stubs would prove nothing about it.
    fn sample_valid_tip() -> SpineTipBundle {
        let p = mercurylib::tesr::TesrParams::regtest();
        let mut parent = sample_bundle();
        let x = parent.levels[0].extension.clone();
        // SP pays a payee's piece at out[0] and the sender's own tip at out[1].
        let avail =
            mercurylib::tesr::tier_out_total(x.out_value, 2, p.committed_fee_rate).unwrap();
        let piece = avail / 2;
        let tip_value = avail - piece;
        let sp = mercurylib::tesr::build_split_state(
            &x.txid,
            x.out_value,
            &[(OWNER.to_string(), piece), (OWNER.to_string(), tip_value)],
            "regtest",
            SPINE_CSV,
            p.committed_fee_rate,
        )
        .unwrap();
        parent.levels[0].state = TesrTier {
            txid: sp.txid.clone(),
            signed_tx: sp.tx_hex,
            out_value: sp.out_value,
            csv: Some(SPINE_CSV),
            payload_vout: sp.payload_vout,
        };
        // The cap: ONE tier, directly over `SP.out[1]`, at `D0` — no extension between them.
        let cap = mercurylib::tesr::build_state_from(
            &sp.txid,
            1,
            tip_value,
            OWNER,
            "regtest",
            p.state_csv(0),
            p.committed_fee_rate,
        )
        .unwrap();
        SpineTipBundle {
            parent_statechain_id: parent.statechain_id.clone(),
            sp_out_value: tip_value,
            parent,
            ancestors: vec![],
            sp_vout: 1,
            statechain_id: "tip-sid".into(),
            owner_exit_address: OWNER.into(),
            cap: TesrTier {
                txid: cap.txid,
                signed_tx: cap.tx_hex,
                out_value: cap.out_value,
                csv: Some(p.state_csv(0)),
                payload_vout: cap.payload_vout,
            },
            superseded_caps: vec![],
            parent_flat_backups: vec![],
            rgb: None,
        }
    }

    /// **[F2] The record is checked against ITSELF, and the arbiter is the signature.**
    ///
    /// A tip has no counterparty: nobody else ever verifies it, so every field it gets wrong is
    /// believed until it costs something. `sp_out_value` is the sharpest of them — `parent_shape`
    /// feeds it straight into the next batch's payload arithmetic, and the split builder's own
    /// conservation law is satisfied by ANY self-consistent set of amounts, so an inflated one
    /// mis-prices a whole batch of payees with every downstream check passing.
    ///
    /// Non-vacuity is built in: the honest fixture is asserted to PASS first, so each refusal below
    /// is attributable to the one field it tampers with and not to a fixture that never validated.
    #[test]
    fn a_spine_tip_record_that_lies_about_its_own_funding_is_refused() {
        let good = sample_valid_tip();
        good.validate().expect("the honest fixture must validate, or every refusal below is vacuous");

        // (1) THE RE-ANCHOR. The cap's signed prevout is the one fact the writer cannot restate:
        // re-pointing `sp_vout` at the payee's slot leaves the signature naming `out[1]`.
        let mut wrong_vout = sample_valid_tip();
        wrong_vout.sp_vout = 0;
        let e = wrong_vout.validate().expect_err("a tip funded by a different SP output is a lie");
        assert!(e.to_string().contains("but the record declares it funded by"), "got: {e}");

        // …and the same check catches a re-parented tip: a record whose ancestors say the cap hangs
        // off a segment the cap's signature has never heard of.
        let mut reparented = sample_valid_tip();
        reparented.ancestors.push(ChildSegment {
            statechain_id: "seg".into(),
            funding_vout: 1,
            extension: None,
            state: reparented.parent.trigger.clone(),
            superseded_states: vec![],
            superseded_extensions: vec![],
        });
        let e = reparented.validate().expect_err("the cap does not spend that segment's state");
        assert!(e.to_string().contains("but the record declares it funded by"), "got: {e}");

        // (2) THE SOURCE VALUE — the number that prices the next batch.
        let mut fat = sample_valid_tip();
        fat.sp_out_value += 50_000;
        let e = fat.validate().expect_err("an inflated sp_out_value mis-prices the next batch");
        assert!(
            e.to_string().contains("the next batch would carve its payees out of an amount that \
                                    does not exist"),
            "got: {e}"
        );

        // (3) THE CSV BAND — `[d_floor, d0]`, and emphatically NOT `[0,0]`. A cap pinned to the
        // spine CSV would leave the next batch's SP nothing to out-race, and the builders' own
        // `s0_csv <= SPINE_CSV` guard would then refuse to build that batch at all.
        let p = mercurylib::tesr::TesrParams::regtest();
        let mut zero_csv = sample_valid_tip();
        let sp_txid = zero_csv.funding_tier().txid.clone();
        let recap = |csv: u16| {
            let t = mercurylib::tesr::build_state_from(
                &sp_txid,
                1,
                zero_csv.sp_out_value,
                OWNER,
                "regtest",
                csv,
                p.committed_fee_rate,
            )
            .unwrap();
            TesrTier {
                txid: t.txid,
                signed_tx: t.tx_hex,
                out_value: t.out_value,
                csv: Some(csv),
                payload_vout: t.payload_vout,
            }
        };
        zero_csv.cap = recap(SPINE_CSV);
        let e = zero_csv.validate().expect_err("a cap at the spine CSV strands the tip");
        assert!(e.to_string().contains("outside the state band"), "got: {e}");
        // Below the floor is refused for the same reason; the floor itself is admitted.
        let mut under = sample_valid_tip();
        under.cap = recap(p.d_floor - 1);
        assert!(under.validate().is_err(), "d_floor - 1 is outside the band");
        let mut at_floor = sample_valid_tip();
        at_floor.cap = recap(p.d_floor);
        at_floor.validate().expect("the floor itself is inside the band");

        // (4) THE DECLARED FIELD MAY NEVER WIN. `cap.csv` is serde; the nSequence is signed. A
        // record that declares an in-band CSV over an out-of-band signature is refused on the
        // disagreement, not admitted on the declaration.
        let mut liar = sample_valid_tip();
        liar.cap = recap(SPINE_CSV);
        liar.cap.csv = Some(p.state_csv(0));
        assert!(liar.validate().is_err(), "the declared CSV must never override the signed one");
    }

    /// **[F1] A coloured tip's exit MOVES its allocation, and the move is resolved against the
    /// tip's OWN funding outpoint.**
    ///
    /// This is the sweep hole that made the producer flip unsafe. `register_colored_exit_tip`
    /// resolved two record shapes and returned `Ok(None)` for everything else — the same answer it
    /// gives a plain coin, and one its caller maps to no event, no fault and no error. A coloured
    /// tip therefore completed its walk on chain while the RGB engine went on advertising the
    /// allocation at `SP.out[K]`, an outpoint the cap had just spent.
    ///
    /// The DEPTH case is the second half and is not decoration: naming the root parent's `SP` for a
    /// tip that descends through earlier spine levels would be a real, existing outpoint belonging
    /// to the wrong transaction — the failure mode that survives adding an arm carelessly.
    #[test]
    fn a_coloured_spine_tip_moves_its_allocation_off_its_own_funding_sp() {
        let mut tip = sample_valid_tip();
        tip.rgb = Some(ColoredTip {
            contract_id: "rgb:contract".into(),
            amount: 400,
            consignment: "cap-consignment".into(),
        });
        let mv = colored_exit_move(LadderRecord::Tip(&tip))
            .expect("a COLOURED tip's exit must be bookable — this is the fallthrough F1 names");
        // Where the allocation LANDS: the cap's payload output, not the P2A anchor beside it.
        assert_eq!(mv.tip_txid, tip.cap.txid);
        assert_eq!(mv.tip_vout, tip.cap.payload_vout);
        assert_eq!(mv.tip_value, tip.cap.out_value);
        assert_eq!((mv.contract_id.as_str(), mv.amount), ("rgb:contract", 400));
        // Where it LEAVES: `SP.out[K]`, the outpoint the engine has registered and the cap spends —
        // never the parent's `F`, which was marked spent when the split was made and would leave
        // this allocation counted twice.
        assert_eq!(
            mv.spent_outpoint,
            format!("{}:{}", tip.parent.current().state.txid, tip.sp_vout)
        );
        assert_ne!(mv.spent_outpoint, format!("{}:{}", tip.parent.f_txid, tip.parent.f_vout));

        // DEPTH: `sp_vout` is relative to the LAST ancestor segment, not to the root parent.
        let mut deep = tip.clone();
        deep.ancestors.push(ChildSegment {
            statechain_id: "spine-level-2".into(),
            funding_vout: 1,
            extension: None,
            state: deep.parent.levels[0].extension.clone(),
            superseded_states: vec![],
            superseded_extensions: vec![],
        });
        let mv = colored_exit_move(LadderRecord::Tip(&deep)).expect("still coloured, still bookable");
        assert_eq!(
            mv.spent_outpoint,
            format!("{}:{}", deep.ancestors[0].state.txid, deep.sp_vout),
            "a deeper tip is funded by the LAST spine level's SP, not the root parent's"
        );

        // A PLAIN tip moves no allocation — `None` here is the one honest negative, and it must
        // stay distinguishable from the fallthrough above.
        assert!(colored_exit_move(LadderRecord::Tip(&sample_valid_tip())).is_none());
    }
}

#[cfg(test)]
mod split_journal_tests {
    //! [P0-3] The journal's PURE contract: what a record says, what it rebuilds, and what it refuses
    //! to rebuild. The live half (a real SIGABRT mid-split, then a restart that replays it) is
    //! `SDK_E2E=81`; these are the parts that must hold with no SE and no database.
    use super::*;
    use super::verify_tests::sample_bundle;

    fn tier(txid: &str, csv: u16) -> TesrTier {
        TesrTier {
            txid: txid.into(),
            signed_tx: "00".into(),
            out_value: 10_000,
            csv: Some(csv),
            payload_vout: 0,
        }
    }

    fn record(stage: SplitStage) -> SplitJournalRecord {
        let p = mercurylib::tesr::TesrParams::regtest();
        SplitJournalRecord {
            op_id: "in_ladder_split:parent:sp".into(),
            lane: "in_ladder_split".into(),
            stage,
            terminalized_statechain_id: "parent".into(),
            parent: sample_bundle(),
            parent_statechain_id: "parent".into(),
            ancestors: vec![],
            parent_flat_backups: vec![],
            children: vec![
                SplitJournalChild {
                    statechain_id: "piece".into(),
                    owner_exit_address: "bcrt1qpayee".into(),
                    value: 30_000,
                    sp_vout: 0,
                    extension: None,
                    state: None,
                    rgb: None,
                    pending_extension: None,
                    pending_state: None,
                    role: SplitLegRole::Piece,
                },
                SplitJournalChild {
                    statechain_id: "change".into(),
                    owner_exit_address: "bcrt1qchange".into(),
                    value: 60_000,
                    sp_vout: 1,
                    extension: None,
                    state: None,
                    rgb: None,
                    pending_extension: None,
                    pending_state: None,
                    role: SplitLegRole::Piece,
                },
            ],
            child_ext_csv: p.ext_csv(0),
            child_state_csv: p.state_csv(0),
            fee_rate: p.committed_fee_rate,
            network: "regtest".into(),
            sp_txid: "sp".into(),
        }
    }

    #[test]
    fn only_a_committed_or_stranded_record_leaves_the_open_set() {
        // Everything before the caller has persisted+conveyed still needs the recovery reader —
        // including `Established`, because the bundles exist only in the returning call's memory.
        assert!(SplitStage::Planned.is_open());
        assert!(SplitStage::Signed.is_open());
        assert!(SplitStage::Established.is_open());
        assert!(!SplitStage::Committed.is_open());
        assert!(!SplitStage::Stranded.is_open());
    }

    #[test]
    fn a_half_built_ladder_is_never_rebuilt_into_a_bundle() {
        // The failure this guards: returning a bundle whose child tiers are missing would convey a
        // ladder the receiver cannot exit — worse than reporting the interruption.
        let mut rec = record(SplitStage::Signed);
        assert!(rec.bundles().is_err(), "no tiers at all → refuse");

        rec.children[0].extension = Some(tier("x0", 12));
        assert!(rec.bundles().is_err(), "extension without state → still refuse");

        rec.children[0].state = Some(tier("s0", 24));
        assert!(
            rec.bundles().is_err(),
            "one COMPLETE child is not enough — the other child's value would silently vanish"
        );

        rec.children[1].extension = Some(tier("x1", 12));
        rec.children[1].state = Some(tier("s1", 24));
        let bundles = rec.bundles().expect("both ladders complete → rebuildable");
        assert_eq!(bundles.len(), 2);
        assert_eq!(bundles[0].child_statechain_id, "piece");
        assert_eq!(bundles[0].sp_vout, 0);
        assert_eq!(bundles[0].child_owner_exit_address, "bcrt1qpayee");
        assert_eq!(bundles[1].sp_vout, 1);
        // A journalled split is always a PLAIN one — the coloured lanes refuse before the record is
        // written — so a rebuilt bundle must never claim an allocation.
        assert!(bundles.iter().all(|b| b.rgb.is_none()));
    }

    #[test]
    fn the_op_id_is_recoverable_from_the_bundles_alone() {
        // The split cannot hand its op_id back without changing a signature every caller uses, so
        // the id must be derivable from what it DOES return — otherwise the caller could never close
        // the record it opened.
        let mut rec = record(SplitStage::Signed);
        for c in rec.children.iter_mut() {
            c.extension = Some(tier("x", 12));
            c.state = Some(tier("s", 24));
        }
        let bundles = rec.bundles().unwrap();
        let expected = format!(
            "in_ladder_split:parent:{}",
            rec.parent.current().state.txid
        );
        assert_eq!(split_op_id(&bundles[0]), expected);
        assert_eq!(split_op_id(&bundles[1]), expected, "one split, one record");

        // The child lane keys off the segment it terminalized, not the root parent.
        let mut deep = bundles[0].clone();
        deep.ancestors.push(ChildSegment {
            statechain_id: "childcoin".into(),
            funding_vout: 0,
            extension: Some(tier("xc", 12)),
            state: tier("csp", 18),
            superseded_states: vec![],
            superseded_extensions: vec![],
        });
        assert_eq!(split_op_id(&deep), "child_in_ladder_split:childcoin:csp");
    }

    #[test]
    fn a_journalled_record_round_trips_through_json() {
        // The record IS the recovery: if it cannot be read back byte-for-byte after a restart, the
        // journal is decoration.
        let mut rec = record(SplitStage::Signed);
        rec.children[0].extension = Some(tier("x0", 12));
        let json = serde_json::to_string(&rec).unwrap();
        let back: SplitJournalRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back.op_id, rec.op_id);
        assert_eq!(back.stage, SplitStage::Signed);
        assert_eq!(back.children.len(), 2);
        assert_eq!(back.children[0].extension.as_ref().unwrap().txid, "x0");
        assert!(back.children[1].extension.is_none());
        assert_eq!(back.sp_txid, "sp");
        assert_eq!(back.child_state_csv, mercurylib::tesr::TesrParams::regtest().state_csv(0));
    }

    /// **[CATS] The journalled leg ROLE, and why it cannot be inferred from `extension: None`.**
    ///
    /// On this record `None` already means "not co-signed YET" — it is what
    /// [`resume_in_ladder_split`] keys off. If it ALSO meant "never has one", a replay would read a
    /// spine tip as unfinished and co-sign a phantom extension over `SP.out[K]` at the piece
    /// schedule's CSV, out-racing the sender's own cap over that outpoint. So the role is journalled,
    /// and both directions are checked.
    #[test]
    fn the_leg_role_is_journalled_and_defaults_to_piece_on_pre_existing_rows() {
        // Every row written before this field existed is a two-tier piece — so a MISSING `role`
        // must read as `Piece`, and it does. (This is a LOCAL record, never conveyed, which is the
        // whole reason a default is safe here and is refused on `ChildSegment::extension`.)
        let old_row = r#"{
            "statechain_id":"legacy","owner_exit_address":"bcrt1q","value":1,"sp_vout":0,
            "extension":null,"state":null
        }"#;
        let c: SplitJournalChild = serde_json::from_str(old_row).unwrap();
        assert_eq!(c.role, SplitLegRole::Piece);
        assert_eq!(SplitLegRole::default(), SplitLegRole::Piece);

        // And a tip round-trips as a tip.
        let mut rec = record(SplitStage::Signed);
        rec.children[1].role = SplitLegRole::SpineTip;
        let back: SplitJournalRecord =
            serde_json::from_str(&serde_json::to_string(&rec).unwrap()).unwrap();
        assert_eq!(back.children[0].role, SplitLegRole::Piece);
        assert_eq!(back.children[1].role, SplitLegRole::SpineTip);

        // `bundles()` must never hand a tip back as a `ctesr-` child bundle: that key routes to leaf
        // handling, which is exactly the mis-classing the tip's own record exists to prevent.
        let mut done = rec.clone();
        for c in done.children.iter_mut() {
            c.extension = Some(tier("x", 12));
            c.state = Some(tier("s", 24));
        }
        let msg = done.bundles().expect_err("a tip is not a conveyable child").to_string();
        assert!(msg.contains("spine tip"), "{msg}");
    }

    /// **[CATS/V1] `ChildSegment::extension` carries NO `#[serde(default)]`, and here is why adding
    /// one would be worse than useless.** `Option<T>` already deserialises from a MISSING field, so a
    /// `default` adds nothing a sender could not already do — it only makes the omission look
    /// sanctioned. The shape a conveyed bundle claims is settled by the verifier's prevout
    /// derivation, never by which fields the JSON happens to contain.
    #[test]
    fn an_omitted_segment_extension_reads_as_a_spine_and_is_not_a_serde_decision() {
        let no_field = r#"{
            "statechain_id":"seg","funding_vout":3,
            "state":{"txid":"s","signed_tx":"00","out_value":1,"csv":0,"payload_vout":0}
        }"#;
        let seg: ChildSegment = serde_json::from_str(no_field).unwrap();
        assert!(seg.extension.is_none(), "a missing field IS the None case — no default required");
        assert!(seg.superseded_extensions.is_empty());

        let two_tier = ChildSegment {
            statechain_id: "seg".into(),
            funding_vout: 3,
            extension: Some(tier("x", 12)),
            state: tier("s", 0),
            superseded_states: vec![],
            superseded_extensions: vec![],
        };
        let back: ChildSegment =
            serde_json::from_str(&serde_json::to_string(&two_tier).unwrap()).unwrap();
        assert_eq!(back.extension.as_ref().unwrap().txid, "x");
    }
}

/// # THE SKIM-LEAF ATTACK, BUILT AND RUN
///
/// Every fix in `docs/utexo/VALUE-CONSERVATION-SWEEP.md` was landed against HONEST traffic: sdk 1, 2,
/// 11, 17, 58, 59, 74, 75, 76 and 77 all show that a well-formed bundle still passes. Not one of them
/// constructs the theft and asserts the refusal, and the probe that originally proved the defect
/// (`4e165e6`) was temporary and is gone. §8 of that document names this as the single most valuable
/// thing left to build. This module is it, for the LEAF hop.
///
/// **What the attacker holds, and why that is realistic.** These tests build a real, fully co-signed
/// ladder while holding BOTH halves of every aggregate key. That is not a cheat — it is precisely
/// what a blind SE hands out. `cosign_tier_request` (lib/src/tesr.rs:503) presents a sighash and the
/// enclave signs it; it never deserialises the transaction, never sees the outputs, and takes the
/// prevout amount as a CALLER parameter. So "this tier is genuinely co-signed" carries no information
/// whatsoever about how the tier splits its input across outputs. Each test below asserts that
/// directly — `verify_tier_cosigned` is checked to still ACCEPT the tampered tier — so the refusal
/// can only be coming from the value law and from nothing else.
///
/// **The attack.** `child_extension` spends `SP.out[j]` (198 530 sat here, the same number the
/// original probe used) and forwards a fraction, sending the remainder to a second output that pays
/// the sender. `child_state` then pays the receiver that fraction minus one rung and declares
/// `out_value` **honestly** — 510 really is all the payee can ever reach. Every declared-vs-signed
/// check therefore passes. What makes it theft is that the receiver books the FUNDING value
/// (`coin.amount = sp_out.value`, transfer_receiver.rs:1321), so the payee is credited 198 530 for a
/// coin worth 510.
///
/// **Non-vacuity.** `honest_child_bundle_is_accepted` runs the identical construction with no
/// tampering and asserts `Ok(())`. Every attack test additionally asserts that the refusal names the
/// right cause, so a bundle that happened to be malformed for an unrelated reason would not pass as a
/// success here.
#[cfg(test)]
mod skim_leaf_attack_tests {
    use super::*;
    use electrum_client::bitcoin::{
        consensus::{deserialize, serialize},
        key::TapTweak,
        secp256k1::{KeyPair, Message, Secp256k1, SecretKey},
        sighash::{Prevouts, SighashCache, TapSighashType},
        Address, Network, ScriptBuf, Transaction, TxOut, Witness,
    };

    const NET: &str = "regtest";
    /// The parent coin's on-chain funding outpoint. Nothing fetches it — `verify_child_bundle` is
    /// pure and takes `F`'s scriptPubKey as a parameter.
    const F_TXID: &str = "3333333333333333333333333333333333333333333333333333333333333333";
    const F_VOUT: u32 = 0;
    const F_VALUE: u64 = 200_000;

    /// A party holding BOTH halves of an aggregate key — what a blind co-sign is equivalent to.
    struct Holder {
        kp: KeyPair,
        /// The P2TR address for the UNTWEAKED internal key (BIP-341, no script tree).
        address: String,
        spk: ScriptBuf,
        /// The x-only the coordinator would have on record: UNTWEAKED, as `/info/statechain` serves it.
        recorded_xonly: String,
    }

    fn holder(seed: u8) -> Holder {
        let secp = Secp256k1::new();
        let sk = SecretKey::from_slice(&[seed; 32]).expect("valid secret key");
        let kp = KeyPair::from_secret_key(&secp, &sk);
        let (xonly, _parity) = kp.x_only_public_key();
        let address = Address::p2tr(&secp, xonly, None, Network::Regtest);
        Holder {
            kp,
            spk: address.script_pubkey(),
            address: address.to_string(),
            recorded_xonly: hex::encode(xonly.serialize()),
        }
    }

    /// Produce exactly the witness `verify_tier_cosigned` checks: a BIP-341 key-spend Schnorr
    /// signature by the aggregate over `TxOut { value: prevout_value, script_pubkey: agg_spk }` with
    /// `TapSighashType::All`. This IS the blind co-sign, performed locally.
    fn cosign(tx: &Transaction, prevout_value: u64, agg_spk: &ScriptBuf, kp: &KeyPair) -> Transaction {
        let secp = Secp256k1::new();
        let prevout = TxOut { value: prevout_value, script_pubkey: agg_spk.clone() };
        let sighash = SighashCache::new(tx)
            .taproot_key_spend_signature_hash(0, &Prevouts::All(&[prevout]), TapSighashType::All)
            .expect("taproot key-spend sighash");
        let msg = Message::from_slice(sighash.as_ref()).expect("32-byte sighash");
        let sig = secp.sign_schnorr_no_aux_rand(&msg, &kp.tap_tweak(&secp, None).to_inner());
        let mut signed = tx.clone();
        signed.input[0].witness = Witness::from_slice(&[&sig[..]]);
        signed
    }

    fn parse(tx_hex: &str) -> Transaction {
        deserialize(&hex::decode(tx_hex).expect("hex")).expect("transaction")
    }

    fn as_hex(tx: &Transaction) -> String {
        hex::encode(serialize(tx))
    }

    /// A bundle tier describing `tx`, with `out_value` read from the transaction — i.e. declared
    /// HONESTLY. Every attack below keeps it that way; the lie is in the OUTPUT VECTOR, never in the
    /// field beside it.
    fn tier(tx: &Transaction, csv: Option<u16>, payload_vout: u32) -> TesrTier {
        TesrTier {
            txid: tx.txid().to_string(),
            signed_tx: as_hex(tx),
            out_value: tx.output[payload_vout as usize].value,
            csv,
            payload_vout,
        }
    }

    /// Where the skim is planted.
    #[derive(Clone, Copy, PartialEq, Debug)]
    enum Skim {
        /// No tampering at all — the non-vacuity control.
        None,
        /// **The original probe.** The extension forwards a fraction and routes the rest to a second
        /// output paying the sender, taking a plausible fee on the way. `Σ payload outputs` comes up
        /// short of the law's expected total.
        ExtensionGreedy,
        /// The same theft, sized so that `Σ payload outputs` STILL equals the expected total — the
        /// attacker pays the committed fee and simply moves the value between two payload outputs.
        /// This is the variant a Σ-only law would wave through, and it exists to prove the
        /// per-output check beside the Σ check is load-bearing rather than redundant.
        ExtensionFeeNeutral,
        /// The skim moved one hop DOWN: an honest extension, and a state that pays the receiver a
        /// fraction while returning the rest. `child_state.out_value` is still declared honestly.
        StateHop,
    }

    struct Rig {
        parent: Holder,
        child: Holder,
        receiver: Holder,
        sender: Holder,
        params: mercurylib::tesr::TesrParams,
        rate: f64,
    }

    /// Everything `verify_child_bundle` needs alongside the bundle: the facts a receiver reads off
    /// the chain and the coordinator, none of which the attack touches.
    struct Facts {
        f_spk_hex: String,
        parent_num_sigs: u32,
        parent_flat_backups: u32,
        parent_xonly: String,
        child_num_sigs: u32,
        child_flat_backups: u32,
        child_xonly: String,
        receiver_address: String,
    }

    fn rig() -> Rig {
        let params = mercurylib::tesr::TesrParams::regtest();
        Rig {
            parent: holder(0x11),
            child: holder(0x22),
            receiver: holder(0x33),
            sender: holder(0x44),
            rate: params.committed_fee_rate,
            params,
        }
    }

    impl Rig {
        /// The PARENT segment, honest throughout and genuinely co-signed by `A_parent`:
        /// `T -> X -> SP`, with `SP` a spine split state (CSV = `SPINE_CSV`) funding ONE child slot.
        /// Returns the bundle and the value of that slot.
        fn parent_segment(&self) -> (TesrBundle, u64) {
            let p = self.params;
            let a = &self.parent;

            let t = mercurylib::tesr::build_trigger(F_TXID, F_VOUT, F_VALUE, &a.address, NET, self.rate)
                .expect("trigger");
            let t_tx = cosign(&parse(&t.tx_hex), F_VALUE, &a.spk, &a.kp);

            let x = mercurylib::tesr::build_extension(
                &t.txid, t.out_value, &a.address, NET, p.ext_csv(0), self.rate,
            )
            .expect("extension");
            let x_tx = cosign(&parse(&x.tx_hex), t.out_value, &a.spk, &a.kp);

            // The child's slot on `SP`: the whole of `X`'s payload minus exactly one rung.
            let slot = mercurylib::tesr::tier_out_total(x.out_value, 1, self.rate).expect("slot");
            let sp = mercurylib::tesr::build_split_state(
                &x.txid,
                x.out_value,
                &[(self.child.address.clone(), slot)],
                NET,
                SPINE_CSV,
                self.rate,
            )
            .expect("split state");
            let sp_tx = cosign(&parse(&sp.tx_hex), x.out_value, &a.spk, &a.kp);

            let bundle = TesrBundle {
                version: 1,
                statechain_id: "parent-sid".into(),
                network: NET.into(),
                fee_rate: self.rate,
                agg_address: a.address.clone(),
                owner_exit_address: self.sender.address.clone(),
                f_txid: F_TXID.into(),
                f_vout: F_VOUT,
                f_value: F_VALUE,
                trigger: tier(&t_tx, None, t.payload_vout),
                levels: vec![TesrLevel {
                    extension: tier(&x_tx, Some(p.ext_csv(0)), x.payload_vout),
                    state: tier(&sp_tx, Some(SPINE_CSV), sp.payload_vout),
                }],
                m: 0,
                superseded_states: vec![],
                superseded_extensions: vec![],
                params: p,
                rgb: None,
            };
            (bundle, slot)
        }

        /// The LEAF: `child_extension` over `SP.out[0]`, then `child_state` paying the receiver.
        /// Both tiers really co-signed by `A_child`, with `skim` deciding where (if anywhere) value
        /// is diverted.
        fn child_bundle(&self, skim: Skim) -> ChildTesrBundle {
            let p = self.params;
            let (parent, slot) = self.parent_segment();
            let sp_txid = parent.current().state.txid.clone();
            let c = &self.child;

            // ---- hop 1: the child extension ------------------------------------------------------
            let xc = mercurylib::tesr::build_extension(
                &sp_txid, slot, &c.address, NET, p.ext_csv(0), self.rate,
            )
            .expect("child extension");
            let honest_forward = xc.out_value; // slot − one rung, what an honest tier pays on
            let mut xc_tx = parse(&xc.tx_hex);
            let forwarded = match skim {
                Skim::ExtensionGreedy | Skim::ExtensionFeeNeutral => {
                    // Forward a token amount; the rest goes to a SECOND output paying the sender.
                    let forwarded = 1_000u64;
                    let diverted = match skim {
                        // Take everything the transaction can carry after a plausible 3-output fee:
                        // Σ(payload) then falls SHORT of the law's expected total.
                        Skim::ExtensionGreedy => {
                            slot - forwarded
                                - mercurylib::tesr::P2A_VALUE
                                - mercurylib::tesr::committed_fee_for_outputs(2, self.rate)
                        }
                        // Keep Σ(payload) EXACTLY on the law's expected total — the fee is committed
                        // honestly and only the distribution across payload outputs is a lie.
                        _ => honest_forward - forwarded,
                    };
                    xc_tx.output[xc.payload_vout as usize].value = forwarded;
                    xc_tx.output.push(TxOut {
                        value: diverted,
                        script_pubkey: self.sender.spk.clone(),
                    });
                    forwarded
                }
                Skim::None | Skim::StateHop => honest_forward,
            };
            // The blind SE co-signs the tampered distribution exactly as it co-signs an honest one.
            let xc_tx = cosign(&xc_tx, slot, &c.spk, &c.kp);

            // ---- hop 2: the child state ----------------------------------------------------------
            let sc = mercurylib::tesr::build_state(
                &xc_tx.txid().to_string(),
                forwarded,
                &self.receiver.address,
                NET,
                p.state_csv(0),
                self.rate,
            )
            .expect("child state");
            let mut sc_tx = parse(&sc.tx_hex);
            if skim == Skim::StateHop {
                // Pay the receiver a fraction and return the rest to the sender. `out_value` below is
                // still read off THIS transaction, so the declaration stays honest.
                let to_receiver = 510u64;
                let diverted = sc.out_value - to_receiver;
                sc_tx.output[sc.payload_vout as usize].value = to_receiver;
                sc_tx.output.push(TxOut { value: diverted, script_pubkey: self.sender.spk.clone() });
            }
            let sc_tx = cosign(&sc_tx, forwarded, &c.spk, &c.kp);

            ChildTesrBundle {
                parent,
                parent_statechain_id: "parent-sid".into(),
                sp_vout: 0,
                child_statechain_id: "child-sid".into(),
                child_owner_exit_address: self.receiver.address.clone(),
                child_extension: tier(&xc_tx, Some(p.ext_csv(0)), xc.payload_vout),
                child_state: tier(&sc_tx, Some(p.state_csv(0)), sc.payload_vout),
                child_superseded_states: vec![],
                child_superseded_extensions: vec![],
                ancestors: vec![],
                rgb: None,
                parent_flat_backups: vec![],
            }
        }

        fn facts(&self) -> Facts {
            Facts {
                f_spk_hex: hex::encode(self.parent.spk.as_bytes()),
                // The parent's census: one deposit backup + T + X + SP, nothing superseded.
                parent_num_sigs: 1 + 3,
                parent_flat_backups: 1,
                parent_xonly: self.parent.recorded_xonly.clone(),
                // A derived child slot has no flat backup (CHILD_V2_BASELINE = 0) — just its two tiers.
                child_num_sigs: 2,
                child_flat_backups: 0,
                child_xonly: self.child.recorded_xonly.clone(),
                receiver_address: self.receiver.address.clone(),
            }
        }
    }

    fn verify(cb: &ChildTesrBundle, f: &Facts) -> Result<()> {
        verify_child_bundle(
            cb,
            &f.f_spk_hex,
            // The chain fact: what `F` actually holds. The parent segment here is honest, so the
            // trigger anchor never fires — every refusal below still comes from the leaf laws.
            F_VALUE,
            f.parent_num_sigs,
            f.parent_flat_backups,
            Some(&f.parent_xonly),
            true, // the parent segment is terminal, as the protocol requires
            f.child_num_sigs,
            f.child_flat_backups,
            Some(&f.child_xonly),
            &[],
            &f.receiver_address,
        )
    }

    /// The co-sign that blesses the skim, isolated. `verify_tier_cosigned` must ACCEPT the tampered
    /// extension: that is what makes the value law the only thing standing between the receiver and
    /// the theft, and it is what every attack test below leans on for non-vacuity.
    fn assert_still_genuinely_cosigned(cb: &ChildTesrBundle, rig: &Rig) {
        let sp: Transaction = parse(&cb.parent.current().state.signed_tx);
        let funding = sp.output[cb.sp_vout as usize].value;
        let ext: Transaction = parse(&cb.child_extension.signed_tx);
        verify_tier_cosigned(&ext, funding, &rig.child.spk)
            .expect("the blind SE really does co-sign this tier — the skim is not a forgery");
        let st: Transaction = parse(&cb.child_state.signed_tx);
        verify_tier_cosigned(&st, ext.output[cb.child_extension.payload_vout as usize].value, &rig.child.spk)
            .expect("and so is the state below it");
    }

    /// A refusal must name the value law, not some unrelated malformation. These are the causes that
    /// would make an attack test pass for the WRONG reason.
    fn assert_not_an_unrelated_refusal(msg: &str) {
        for unrelated in [
            "not co-signed",
            "num_sigs",
            "CSV",
            "does not spend",
            "colour",
            "Model A",
            "decoy",
            "terminal",
            "out of range",
        ] {
            assert!(
                !msg.contains(unrelated),
                "the bundle was refused for an UNRELATED reason ({unrelated:?}) — this test would \
                 be worthless: {msg}"
            );
        }
    }

    // ── THE NON-VACUITY CONTROL ────────────────────────────────────────────────────────────────

    /// **The control every attack below depends on.** The identical construction, untampered, must be
    /// ACCEPTED. Without this, a refusal proves nothing: the scaffolding could be malformed in a
    /// dozen ways that have nothing to do with value conservation.
    #[test]
    fn honest_child_bundle_is_accepted() {
        let rig = rig();
        let cb = rig.child_bundle(Skim::None);
        // The arithmetic the tests below break, stated first so a schedule change shows up here and
        // not as a mystery failure three tests down. A plain rung at 2 sat/vB is 490 sat.
        let sp: Transaction = parse(&cb.parent.current().state.signed_tx);
        let slot = sp.output[0].value;
        assert_eq!(slot, 198_530, "the child's slot on SP");
        assert_eq!(cb.child_extension.out_value, slot - 490, "the extension forwards one rung less");
        assert_eq!(
            cb.child_state.out_value,
            slot - 2 * 490,
            "and the state pays the receiver one rung less again"
        );
        verify(&cb, &rig.facts()).expect("an honest, fully co-signed child bundle must be ACCEPTED");
    }

    // ── THE ATTACKS ────────────────────────────────────────────────────────────────────────────

    /// **The original probe, reconstructed.** `child_extension` spends the 198 530-sat slot and pays
    /// 1 000 sat forward, with the remainder going to a second output back to the sender.
    /// `child_state` then pays the receiver 510 sat and declares `out_value: 510` truthfully. Before
    /// `4e165e6` this returned `Ok(())` and the receiver was credited 198 530 for a coin worth 510.
    #[test]
    fn a_skimming_child_extension_is_refused() {
        let rig = rig();
        let cb = rig.child_bundle(Skim::ExtensionGreedy);

        // The theft is real and everything else about the bundle is honest.
        assert_eq!(cb.child_extension.out_value, 1_000, "only 1 000 sat is forwarded");
        assert_eq!(cb.child_state.out_value, 510, "…so 510 sat is all the payee can ever reach");
        let ext: Transaction = parse(&cb.child_extension.signed_tx);
        assert_eq!(ext.output.len(), 3, "payload + P2A anchor + the sender's second output");
        assert_eq!(
            ext.output[2].script_pubkey, rig.sender.spk,
            "the skimmed value goes back to the SENDER"
        );
        assert_eq!(
            ext.output[cb.child_extension.payload_vout as usize].value,
            cb.child_extension.out_value,
            "the declared value is HONEST — the lie is in the output vector"
        );
        assert_still_genuinely_cosigned(&cb, &rig);

        let e = verify(&cb, &rig.facts()).expect_err("a skimming child extension must be REFUSED");
        let msg = e.to_string();
        assert!(
            msg.contains("payload outputs carry") && msg.contains("would leave the exit chain"),
            "the refusal must name the payload outputs and the exit chain, got: {msg}"
        );
        assert!(msg.contains("child extension"), "…and it must name the hop, got: {msg}");
        assert_not_an_unrelated_refusal(&msg);
    }

    /// **The Σ-neutral variant, which is why the per-output check beside the Σ check is not
    /// redundant.** Same theft, but the attacker keeps `Σ payload outputs` exactly on the number the
    /// conservation law expects — the committed fee is paid honestly and only the DISTRIBUTION
    /// between the two payload outputs is a lie. A law that summed and stopped there would accept
    /// this bundle.
    #[test]
    fn a_sum_neutral_skim_across_two_payload_outputs_is_refused() {
        let rig = rig();
        let cb = rig.child_bundle(Skim::ExtensionFeeNeutral);

        let ext: Transaction = parse(&cb.child_extension.signed_tx);
        let payload_total: u64 = ext
            .output
            .iter()
            .filter(|o| {
                o.script_pubkey.as_bytes() != mercurylib::tesr::P2A_SCRIPT_BYTES
                    && !o.script_pubkey.is_op_return()
            })
            .map(|o| o.value)
            .sum();
        let sp: Transaction = parse(&cb.parent.current().state.signed_tx);
        let slot = sp.output[0].value;
        assert_eq!(
            payload_total,
            mercurylib::tesr::tier_out_value(slot, rig.rate).unwrap(),
            "Σ over the payload outputs is EXACTLY what the conservation law expects — the sum \
             check alone cannot see this attack"
        );
        assert_eq!(cb.child_extension.out_value, 1_000, "yet only 1 000 sat continues down the chain");
        assert_still_genuinely_cosigned(&cb, &rig);

        let e = verify(&cb, &rig.facts()).expect_err("a sum-neutral skim must be REFUSED");
        let msg = e.to_string();
        assert!(
            msg.contains("forwards only") && msg.contains("skimmed"),
            "the refusal must name the under-forwarding payload output, got: {msg}"
        );
        assert!(msg.contains("child extension"), "…and it must name the hop, got: {msg}");
        assert_not_an_unrelated_refusal(&msg);
    }

    /// **The same theft one hop down.** The extension is entirely honest; the STATE pays the receiver
    /// 510 sat and returns the rest to the sender, declaring `out_value: 510` truthfully. The
    /// declared-vs-signed check (`value-gate spoof`) is satisfied — it makes the number honest, not
    /// correct — so only the second conservation hop refuses it.
    #[test]
    fn a_skimming_child_state_is_refused() {
        let rig = rig();
        let cb = rig.child_bundle(Skim::StateHop);

        let sp: Transaction = parse(&cb.parent.current().state.signed_tx);
        let slot = sp.output[0].value;
        assert_eq!(
            cb.child_extension.out_value,
            mercurylib::tesr::tier_out_value(slot, rig.rate).unwrap(),
            "the extension hop is HONEST — the skim is entirely in the state"
        );
        let st: Transaction = parse(&cb.child_state.signed_tx);
        assert_eq!(st.output.len(), 3, "receiver payment + P2A anchor + the sender's second output");
        assert_eq!(st.output[2].script_pubkey, rig.sender.spk);
        assert_eq!(cb.child_state.out_value, 510, "declared honestly: 510 is what the payee reaches");
        assert_eq!(
            st.output[cb.child_state.payload_vout as usize].value,
            cb.child_state.out_value,
            "so the `value-gate spoof` check is satisfied and cannot be what refuses this"
        );
        assert_still_genuinely_cosigned(&cb, &rig);

        let e = verify(&cb, &rig.facts()).expect_err("a skimming child state must be REFUSED");
        let msg = e.to_string();
        assert!(
            msg.contains("pays the receiver only")
                && msg.contains("would leave the receiver's exit chain entirely"),
            "the refusal must name the receiver's exit chain, got: {msg}"
        );
        assert!(msg.contains("child state"), "…and it must name the hop, got: {msg}");
        assert_not_an_unrelated_refusal(&msg);
    }
}

/// # THE SKIM-ROOT ATTACK, BUILT AND RUN
///
/// The sibling of `skim_leaf_attack_tests` on the WHOLE-COIN lane. `deed25c` added a per-tier
/// conservation law to `verify_bundle_ex` — the root ladder's `T → X_m → S_k` — and, like every other
/// commit in the `docs/utexo/VALUE-CONSERVATION-SWEEP.md` series, it was validated only by showing
/// that honest traffic still passes. §8 of that document names the missing half: *"No test proves the
/// attacks are now refused."* This module builds them.
///
/// **Why the root lane is the worse one.** On the child lane the number the receiver books comes out
/// of the bundle, so a skim lowers the credited amount too. On the root lane the receiver books the
/// **on-chain funding value** (`amount: tx0_output.value`, lib/src/transfer/receiver.rs → assigned at
/// transfer_receiver.rs:1486), which is the largest number in the whole structure and which no skim
/// can touch. And the fallback is not a fallback: `T` is un-timelocked, spends `F`, and every prior
/// owner keeps a co-signed copy of it, so the moment one of them broadcasts it every flat backup —
/// all of which also spend `F` — is void ([B1]). The theft and the destruction of the slow path are
/// the same transaction.
///
/// **What the attacker holds, and why that is realistic.** These tests build a real, fully co-signed
/// ladder while holding both halves of the aggregate key `A`. That is not a cheat — it is exactly
/// what a blind SE hands out: `cosign_tier_request` (lib/src/tesr.rs:503) is handed a sighash and a
/// prevout AMOUNT, never a transaction, so it cannot see how a tier splits its input across outputs.
/// Every attack below additionally asserts that `verify_tier_cosigned` still ACCEPTS the tampered
/// tier, so the refusal can only be coming from the value law.
///
/// **Non-vacuity.** `honest_root_ladder_is_accepted` runs the identical construction untampered and
/// asserts `Ok(())` through all three entry points (`verify_bundle_ex`, `verify_bundle`,
/// `verify_bundle_bound`). Every attack test asserts the refusal names the value law, and
/// `assert_not_an_unrelated_refusal` fails the test if the bundle was thrown out for a structural
/// reason instead.
///
/// **Both holes this module opened with are now closed, and the tests that pinned them were flipped
/// rather than deleted.** GAP 1 (a sum-preserving redistribution across two payload outputs) is
/// refused by the payload-COUNT check; GAP 2 (a skimming trigger) by the trigger-to-`F` anchor,
/// `verify_bundle_ex`'s `f_onchain` parameter. One residue is deliberate and is asserted rather than
/// left implicit: `verify_bundle`, the UNBOUND entry point, has no chain access and therefore no
/// anchor — see the last assertion of
/// `a_skimming_trigger_is_refused_against_the_on_chain_funding_value` for why supplying
/// `bundle.f_value` there would be worse than supplying nothing.
#[cfg(test)]
mod skim_root_attack_tests {
    use super::*;
    use electrum_client::bitcoin::{
        consensus::{deserialize, serialize},
        key::TapTweak,
        secp256k1::{KeyPair, Message, Secp256k1, SecretKey},
        sighash::{Prevouts, SighashCache, TapSighashType},
        Address, Network, ScriptBuf, Transaction, TxOut, Witness,
    };

    const NET: &str = "regtest";
    const SID: &str = "root-sid";
    /// The coin's on-chain funding outpoint. Nothing fetches it — every entry point used here is pure
    /// and takes the chain facts as parameters.
    const F_TXID: &str = "5555555555555555555555555555555555555555555555555555555555555555";
    const F_VOUT: u32 = 0;
    const F_VALUE: u64 = 200_000;

    /// An ordinary on-chain root coin: the deposit co-signs one flat backup before the ladder is
    /// established, then `T`, `X_0` and `S_0` consume one co-sign each. The census is EXACT equality,
    /// so these two constants are not decoration — get either wrong and every test below fails on
    /// `num_sigs mismatch` instead of on the value law.
    const FLAT_BACKUPS: u32 = 1;
    const NUM_SIGS: u32 = FLAT_BACKUPS + 3;

    /// One plain rung at the committed 2 sat/vB: `committed_fee(2.0) + P2A_VALUE` = 250 + 240.
    const RUNG: u64 = 490;

    /// A party holding BOTH halves of an aggregate key — what a blind co-sign is equivalent to.
    struct Holder {
        kp: KeyPair,
        /// The P2TR address for the UNTWEAKED internal key (BIP-341, no script tree).
        address: String,
        spk: ScriptBuf,
        /// The x-only the coordinator would have on record: UNTWEAKED, as `/info/statechain` serves it.
        recorded_xonly: String,
    }

    fn holder(seed: u8) -> Holder {
        let secp = Secp256k1::new();
        let sk = SecretKey::from_slice(&[seed; 32]).expect("valid secret key");
        let kp = KeyPair::from_secret_key(&secp, &sk);
        let (xonly, _parity) = kp.x_only_public_key();
        let address = Address::p2tr(&secp, xonly, None, Network::Regtest);
        Holder {
            kp,
            spk: address.script_pubkey(),
            address: address.to_string(),
            recorded_xonly: hex::encode(xonly.serialize()),
        }
    }

    /// Produce exactly the witness `verify_tier_cosigned` checks: a BIP-341 key-spend Schnorr
    /// signature by the aggregate over `TxOut { value: prevout_value, script_pubkey: agg_spk }` with
    /// `TapSighashType::All`. This IS the blind co-sign, performed locally — note that it commits to
    /// the prevout AMOUNT and to the transaction, but the SE that produces it in production sees only
    /// the resulting 32 bytes.
    fn cosign(tx: &Transaction, prevout_value: u64, agg_spk: &ScriptBuf, kp: &KeyPair) -> Transaction {
        let secp = Secp256k1::new();
        let prevout = TxOut { value: prevout_value, script_pubkey: agg_spk.clone() };
        let sighash = SighashCache::new(tx)
            .taproot_key_spend_signature_hash(0, &Prevouts::All(&[prevout]), TapSighashType::All)
            .expect("taproot key-spend sighash");
        let msg = Message::from_slice(sighash.as_ref()).expect("32-byte sighash");
        let sig = secp.sign_schnorr_no_aux_rand(&msg, &kp.tap_tweak(&secp, None).to_inner());
        let mut signed = tx.clone();
        signed.input[0].witness = Witness::from_slice(&[&sig[..]]);
        signed
    }

    fn parse(tx_hex: &str) -> Transaction {
        deserialize(&hex::decode(tx_hex).expect("hex")).expect("transaction")
    }

    /// A bundle tier describing `tx`, with `out_value` and `txid` read FROM the transaction — i.e.
    /// declared honestly. Every attack here keeps it that way: the lie is in the output vector, never
    /// in the field beside it, so no declared-vs-signed check can be what refuses these bundles.
    fn tier(tx: &Transaction, csv: Option<u16>, payload_vout: u32) -> TesrTier {
        TesrTier {
            txid: tx.txid().to_string(),
            signed_tx: hex::encode(serialize(tx)),
            out_value: tx.output[payload_vout as usize].value,
            csv,
            payload_vout,
        }
    }

    /// What a skimming tier deigns to forward down the exit chain.
    const TOKEN_FORWARD: u64 = 1_000;
    /// What the EXTRA-OUTPUT variant takes. It comes out of the committed fee rather than out of the
    /// payload, so the tier stays consensus-valid (outputs still below the input) and would really
    /// relay — which is what makes that variant worth refusing rather than shrugging at.
    const FEE_THEFT: u64 = 100;

    /// How a single tier is tampered with. Each is applied to the BUILDER's honest output vector, so
    /// everything not named here — version, locktime, sequence, the P2A anchor, the payee's
    /// scriptPubKey — is exactly what an honest tier carries.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Tamper {
        None,
        /// **The skim.** Forward a token amount and route the rest to a second output paying the
        /// attacker, paying a plausible 3-output fee on the way. `Σ payload outputs` comes up SHORT
        /// of the law's expected total by exactly the extra output's own fee.
        Short,
        /// **The extra-output variant.** The payload output is left exactly right; a THIRD output is
        /// appended, funded out of the committed fee. Reading `out[payload_vout]` and stopping there
        /// would wave this through — summing is what sees it.
        Extra,
        /// **The sum-preserving redistribution.** Forward a token amount and put the remainder in a
        /// second output, sized so `Σ payload outputs` still equals the law's expected total exactly.
        /// See `gap_*` below: this one is NOT refused.
        SumNeutral,
    }

    fn apply(t: Tamper, tx: &mut Transaction, prev: u64, rate: f64, attacker: &ScriptBuf) {
        match t {
            Tamper::None => {}
            Tamper::Short => {
                // Everything the transaction can carry after an honest 3-output fee.
                let diverted = prev
                    - TOKEN_FORWARD
                    - mercurylib::tesr::P2A_VALUE
                    - mercurylib::tesr::committed_fee_for_outputs(2, rate);
                tx.output[0].value = TOKEN_FORWARD;
                tx.output.push(TxOut { value: diverted, script_pubkey: attacker.clone() });
            }
            Tamper::Extra => {
                tx.output.push(TxOut { value: FEE_THEFT, script_pubkey: attacker.clone() });
            }
            Tamper::SumNeutral => {
                let honest = tx.output[0].value;
                tx.output[0].value = TOKEN_FORWARD;
                tx.output
                    .push(TxOut { value: honest - TOKEN_FORWARD, script_pubkey: attacker.clone() });
            }
        }
    }

    /// Which tier of the root ladder is attacked, and how. Tier 0 is the trigger, 1 the extension,
    /// 2 the owner state.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Skim {
        /// No tampering at all — the non-vacuity control.
        None,
        TriggerShort,
        ExtensionShort,
        ExtensionExtra,
        ExtensionSumNeutral,
        StateShort,
        StateExtra,
    }

    impl Skim {
        fn at(self, i: usize) -> Tamper {
            match (self, i) {
                (Skim::TriggerShort, 0) => Tamper::Short,
                (Skim::ExtensionShort, 1) => Tamper::Short,
                (Skim::ExtensionExtra, 1) => Tamper::Extra,
                (Skim::ExtensionSumNeutral, 1) => Tamper::SumNeutral,
                (Skim::StateShort, 2) => Tamper::Short,
                (Skim::StateExtra, 2) => Tamper::Extra,
                _ => Tamper::None,
            }
        }
    }

    struct Rig {
        agg: Holder,
        owner: Holder,
        attacker: Holder,
        params: mercurylib::tesr::TesrParams,
        rate: f64,
    }

    fn rig() -> Rig {
        let params = mercurylib::tesr::TesrParams::regtest();
        Rig {
            agg: holder(0x51),
            owner: holder(0x52),
            attacker: holder(0x53),
            rate: params.committed_fee_rate,
            params,
        }
    }

    impl Rig {
        /// The whole-coin root ladder `T → X_0 → S_0`, every tier genuinely co-signed by `A`, with
        /// `skim` deciding where (if anywhere) value is diverted.
        ///
        /// Each tier is built FROM the value its parent actually forwards, so a skim high up does not
        /// leave the tiers below it inconsistent: the ladder stays internally perfect and only its
        /// relationship to the funding value is a lie. That is the whole point — an inconsistent
        /// ladder would be refused by the co-sign check and would prove nothing about the value law.
        fn ladder(&self, skim: Skim) -> TesrBundle {
            let p = self.params;
            let a = &self.agg;

            // ---- tier 0: the trigger, spending the on-chain funding output F --------------------
            let t = mercurylib::tesr::build_trigger(F_TXID, F_VOUT, F_VALUE, &a.address, NET, self.rate)
                .expect("trigger");
            let mut t_tx = parse(&t.tx_hex);
            apply(skim.at(0), &mut t_tx, F_VALUE, self.rate, &self.attacker.spk);
            let t_tx = cosign(&t_tx, F_VALUE, &a.spk, &a.kp);
            let t_forward = t_tx.output[t.payload_vout as usize].value;

            // ---- tier 1: the extension ----------------------------------------------------------
            let x = mercurylib::tesr::build_extension(
                &t_tx.txid().to_string(), t_forward, &a.address, NET, p.ext_csv(0), self.rate,
            )
            .expect("extension");
            let mut x_tx = parse(&x.tx_hex);
            apply(skim.at(1), &mut x_tx, t_forward, self.rate, &self.attacker.spk);
            let x_tx = cosign(&x_tx, t_forward, &a.spk, &a.kp);
            let x_forward = x_tx.output[x.payload_vout as usize].value;

            // ---- tier 2: the owner state ---------------------------------------------------------
            let s = mercurylib::tesr::build_state(
                &x_tx.txid().to_string(), x_forward, &self.owner.address, NET, p.state_csv(0), self.rate,
            )
            .expect("state");
            let mut s_tx = parse(&s.tx_hex);
            apply(skim.at(2), &mut s_tx, x_forward, self.rate, &self.attacker.spk);
            let s_tx = cosign(&s_tx, x_forward, &a.spk, &a.kp);

            TesrBundle {
                version: 1,
                statechain_id: SID.into(),
                network: NET.into(),
                fee_rate: self.rate,
                agg_address: a.address.clone(),
                owner_exit_address: self.owner.address.clone(),
                f_txid: F_TXID.into(),
                f_vout: F_VOUT,
                f_value: F_VALUE,
                trigger: tier(&t_tx, None, t.payload_vout),
                levels: vec![TesrLevel {
                    extension: tier(&x_tx, Some(p.ext_csv(0)), x.payload_vout),
                    state: tier(&s_tx, Some(p.state_csv(0)), s.payload_vout),
                }],
                m: 0,
                superseded_states: vec![],
                superseded_extensions: vec![],
                params: p,
                rgb: None,
            }
        }

        /// The authority a receiver derives from the CHAIN and the coordinator — none of which any
        /// attack here touches, which is exactly why `verify_bundle_bound` cannot be what refuses them.
        fn authority(&self) -> CoinAuthority {
            CoinAuthority {
                statechain_id: SID.into(),
                f_txid: F_TXID.into(),
                f_vout: F_VOUT,
                f_value: F_VALUE,
                f_spk_hex: hex::encode(self.agg.spk.as_bytes()),
                se_aggregate_pubkey: Some(self.agg.recorded_xonly.clone()),
            }
        }
    }

    /// The entry point under test: the root ladder, un-split (`final_is_split: false`, the constant
    /// every whole-coin path passes), with the funding value supplied as the CHAIN fact a receiver
    /// would have fetched — `Some(F_VALUE)`, exactly what `verify_bundle_bound` derives from `tx0`.
    fn verify_root(b: &TesrBundle) -> Result<()> {
        verify_bundle_ex(b, NUM_SIGS, FLAT_BACKUPS, false, Some(F_VALUE))
    }

    /// The co-sign that blesses the skim, isolated. `verify_tier_cosigned` must ACCEPT every tampered
    /// tier: that is what makes the value law the only thing standing between the receiver and the
    /// theft, and it is what every attack test leans on for non-vacuity.
    fn assert_still_genuinely_cosigned(b: &TesrBundle, rig: &Rig) {
        let tiers = b.exit_tiers();
        let txs: Vec<Transaction> = tiers.iter().map(|t| parse(&t.signed_tx)).collect();
        let mut prev = b.f_value;
        for (i, tx) in txs.iter().enumerate() {
            verify_tier_cosigned(tx, prev, &rig.agg.spk).unwrap_or_else(|e| {
                panic!("tier {i} is not a genuine co-sign, so this test would prove nothing: {e}")
            });
            prev = tx.output[tiers[i].payload_vout as usize].value;
        }
    }

    /// A refusal must name the value law, not some unrelated malformation. These are the causes that
    /// would make an attack test pass for the WRONG reason.
    fn assert_not_an_unrelated_refusal(msg: &str) {
        for unrelated in [
            "not co-signed",
            "num_sigs",
            "CSV",
            "does not spend",
            "pays the wrong output",
            "does not pay the aggregate",
            "colour",
            "PLAIN",
            "decoy",
            "malformed ladder",
            "repeats a tier txid",
            "outside this ladder",
        ] {
            assert!(
                !msg.contains(unrelated),
                "the bundle was refused for an UNRELATED reason ({unrelated:?}) — this test would \
                 be worthless: {msg}"
            );
        }
    }

    /// Every refusal in this module has the same shape; this asserts it names the value law, the
    /// right tier, and the two numbers that make the theft legible.
    fn assert_conservation_refusal(msg: &str, tier_index: usize, funded_with: u64, carried: u64) {
        assert!(
            msg.contains(&format!("tier {tier_index} is funded with {funded_with} sat")),
            "the refusal must name the offending tier and the value it was funded with, got: {msg}"
        );
        assert!(
            msg.contains(&format!("payload outputs carry {carried}")),
            "…and what its payload outputs actually carry, got: {msg}"
        );
        assert!(
            msg.contains("would leave the owner's exit chain"),
            "…and where the difference goes, got: {msg}"
        );
        assert_not_an_unrelated_refusal(msg);
    }

    // ── THE NON-VACUITY CONTROL ────────────────────────────────────────────────────────────────

    /// **The control every attack below depends on.** The identical construction, untampered, must be
    /// ACCEPTED — through the unbound entry point, the public wrapper, AND the bound acceptance path.
    /// Without this a refusal proves nothing: the scaffolding could be malformed in a dozen ways that
    /// have nothing to do with value conservation.
    #[test]
    fn honest_root_ladder_is_accepted() {
        let rig = rig();
        let b = rig.ladder(Skim::None);

        // The arithmetic the attacks break, stated once here so a schedule change shows up as a
        // failure in the CONTROL rather than as a mystery three tests down.
        assert_eq!(RUNG, mercurylib::tesr::committed_fee(rig.rate) + mercurylib::tesr::P2A_VALUE);
        assert_eq!(b.trigger.out_value, F_VALUE - RUNG, "the trigger forwards F minus one rung");
        assert_eq!(b.levels[0].extension.out_value, F_VALUE - 2 * RUNG);
        assert_eq!(b.levels[0].state.out_value, F_VALUE - 3 * RUNG, "198 530 reaches the owner");

        verify_root(&b).expect("an honest, fully co-signed root ladder must be ACCEPTED");
        verify_bundle(&b, NUM_SIGS, FLAT_BACKUPS).expect("…through the public wrapper too");
        verify_bundle_bound(&b, NUM_SIGS, FLAT_BACKUPS, &rig.authority())
            .expect("…and through the bound acceptance path the claim/SSP lanes actually call");
    }

    // ── THE ATTACKS ────────────────────────────────────────────────────────────────────────────

    /// **The skim, on the extension.** `X_0` spends the trigger's 199 510-sat payload output and pays
    /// 1 000 sat forward, with the remainder going to a second output that pays the attacker. `S_0`
    /// then pays the owner 510 sat, and every declared field in the bundle is honest about it. What
    /// makes it theft is that the receiver books the ON-CHAIN funding value — 200 000 — for a coin
    /// whose exit chain can deliver 510.
    #[test]
    fn a_skimming_extension_is_refused() {
        let rig = rig();
        let b = rig.ladder(Skim::ExtensionShort);

        // The theft is real, and everything else about the bundle is honest.
        let x: Transaction = parse(&b.levels[0].extension.signed_tx);
        assert_eq!(x.output.len(), 3, "payload + P2A anchor + the attacker's second output");
        assert_eq!(x.output[2].script_pubkey, rig.attacker.spk, "the skim pays the ATTACKER");
        assert_eq!(b.levels[0].extension.out_value, TOKEN_FORWARD, "only 1 000 sat is forwarded");
        assert_eq!(b.levels[0].state.out_value, TOKEN_FORWARD - RUNG, "…so the owner reaches 510");
        assert_eq!(
            x.output[b.levels[0].extension.payload_vout as usize].value,
            b.levels[0].extension.out_value,
            "the declared value is HONEST — the lie is in the output vector"
        );
        assert_still_genuinely_cosigned(&b, &rig);

        let msg = verify_root(&b).expect_err("a skimming extension must be REFUSED").to_string();
        // Σ = 1 000 forwarded + 197 934 diverted = 198 934, against an expected 199 020: short by
        // exactly the extra output's own fee.
        assert_conservation_refusal(&msg, 1, F_VALUE - RUNG, 198_934);

        // The same refusal on the path a receiver and a pre-paying SSP actually call — the bound
        // entry point binds `f_value` to the chain and then delegates to the same law.
        let bound = verify_bundle_bound(&b, NUM_SIGS, FLAT_BACKUPS, &rig.authority())
            .expect_err("the bound acceptance path must refuse it too")
            .to_string();
        assert_conservation_refusal(&bound, 1, F_VALUE - RUNG, 198_934);
    }

    /// **The same skim on the final tier.** `X_0` is entirely honest; `S_0` pays the owner 1 000 sat
    /// and returns the rest to the attacker. The declared `out_value` is again truthful, so nothing
    /// about the bundle's *fields* is wrong — only the transaction is.
    #[test]
    fn a_skimming_owner_state_is_refused() {
        let rig = rig();
        let b = rig.ladder(Skim::StateShort);

        let s: Transaction = parse(&b.levels[0].state.signed_tx);
        assert_eq!(s.output.len(), 3);
        assert_eq!(s.output[2].script_pubkey, rig.attacker.spk);
        assert_eq!(
            b.levels[0].extension.out_value,
            F_VALUE - 2 * RUNG,
            "the extension hop is HONEST — the skim is entirely in the state"
        );
        assert_eq!(
            s.output[0].script_pubkey,
            rig.owner.spk,
            "and the state still PAYS THE OWNER on its payload output, so the payee check passes"
        );
        assert_still_genuinely_cosigned(&b, &rig);

        let msg = verify_root(&b).expect_err("a skimming owner state must be REFUSED").to_string();
        // Funded with 199 020; Σ = 1 000 + 197 444 = 198 444 against an expected 198 530.
        assert_conservation_refusal(&msg, 2, F_VALUE - 2 * RUNG, 198_444);
    }

    /// **The extra-output variant, which is what summing is FOR.** The extension's payload output is
    /// left exactly right — `out[payload_vout]` is the honest 199 020 — and a THIRD output is
    /// appended, funded out of the committed fee. A law that read `out[payload_vout]` and stopped
    /// there would accept this; the tier would still relay (its outputs remain below its input) and
    /// the attacker would pocket the difference.
    #[test]
    fn an_extra_output_on_the_extension_is_refused() {
        let rig = rig();
        let b = rig.ladder(Skim::ExtensionExtra);

        let x: Transaction = parse(&b.levels[0].extension.signed_tx);
        assert_eq!(x.output.len(), 3, "payload + P2A anchor + one appended output");
        assert_eq!(x.output[2].value, FEE_THEFT);
        assert_eq!(x.output[2].script_pubkey, rig.attacker.spk);
        assert_eq!(
            b.levels[0].extension.out_value,
            F_VALUE - 2 * RUNG,
            "the PAYLOAD output is exactly what the conservation law expects — reading that one \
             output cannot see this attack"
        );
        let outs: u64 = x.output.iter().map(|o| o.value).sum();
        assert!(
            outs < b.trigger.out_value,
            "and the tier is still consensus-valid ({outs} out of {}), so it really would relay",
            b.trigger.out_value
        );
        assert_still_genuinely_cosigned(&b, &rig);

        let msg = verify_root(&b)
            .expect_err("a tier carrying a third output must be REFUSED")
            .to_string();
        assert_conservation_refusal(&msg, 1, F_VALUE - RUNG, F_VALUE - 2 * RUNG + FEE_THEFT);
    }

    /// The same extra output on the FINAL tier, where the payee check is against the owner rather than
    /// the aggregate — a different branch of the structural loop, and the last hop before the money is
    /// the owner's.
    #[test]
    fn an_extra_output_on_the_owner_state_is_refused() {
        let rig = rig();
        let b = rig.ladder(Skim::StateExtra);

        let s: Transaction = parse(&b.levels[0].state.signed_tx);
        assert_eq!(s.output.len(), 3);
        assert_eq!(s.output[0].script_pubkey, rig.owner.spk, "the owner is still paid in full…");
        assert_eq!(b.levels[0].state.out_value, F_VALUE - 3 * RUNG);
        assert_eq!(s.output[2].script_pubkey, rig.attacker.spk, "…and the fee is stolen anyway");
        assert_still_genuinely_cosigned(&b, &rig);

        let msg = verify_root(&b).expect_err("a third output on S_0 must be REFUSED").to_string();
        assert_conservation_refusal(&msg, 2, F_VALUE - 2 * RUNG, F_VALUE - 3 * RUNG + FEE_THEFT);
    }

    // ── TWO HOLES THIS LANE STILL HAS ──────────────────────────────────────────────────────────
    //
    // The two tests below assert what `verify_bundle_ex` does TODAY, and what it does today is accept
    // a theft. They are tripwires, not endorsements: each one fails the moment the hole is closed, and
    // says so. Do not "fix" them by deleting them — fix the verifier and flip the assertion.

    /// **GAP 1 — a sum-preserving redistribution is ACCEPTED.**
    ///
    /// `deed25c`'s law is `Σ(payload outputs) == tier_out_total(prev, n_payload, rate)`, and for every
    /// tier that is not the split state of a split parent it hard-codes `n_payload = 1` while summing
    /// over ALL non-anchor, non-opret outputs. So a tier with TWO payload outputs whose values add up
    /// to the expected total satisfies it exactly. The attacker forwards 1 000 sat down the exit chain
    /// and takes 198 020 in a second output; the ladder below re-bases on the 1 000 and stays
    /// internally perfect; the receiver books the on-chain 200 000.
    ///
    /// The child lane does NOT have this hole: `verify_child_bundle` checks the payload output
    /// itself, not just the sum (`skim_leaf_attack_tests::a_sum_neutral_skim_across_two_payload_outputs_is_refused`).
    /// The root lane needs the same — for a non-split tier, `n_payload` must be REQUIRED to be 1, or
    /// `out[payload_vout]` must be checked alongside the sum.
    #[test]
    fn a_sum_preserving_redistribution_on_the_root_lane_is_refused() {
        let rig = rig();
        let b = rig.ladder(Skim::ExtensionSumNeutral);

        // The theft, stated as arithmetic.
        let x: Transaction = parse(&b.levels[0].extension.signed_tx);
        let payload_sum: u64 = x
            .output
            .iter()
            .filter(|o| {
                o.script_pubkey.as_bytes() != mercurylib::tesr::P2A_SCRIPT_BYTES
                    && !o.script_pubkey.is_op_return()
            })
            .map(|o| o.value)
            .sum();
        assert_eq!(
            payload_sum,
            mercurylib::tesr::tier_out_value(b.trigger.out_value, rig.rate).unwrap(),
            "Σ over the payload outputs is EXACTLY what the law expects"
        );
        assert_eq!(x.output[2].script_pubkey, rig.attacker.spk);
        assert_eq!(x.output[2].value, F_VALUE - 2 * RUNG - TOKEN_FORWARD, "198 020 to the attacker");
        assert_eq!(b.levels[0].state.out_value, TOKEN_FORWARD - RUNG, "510 reaches the owner");
        assert_still_genuinely_cosigned(&b, &rig);

        let r = verify_bundle_bound(&b, NUM_SIGS, FLAT_BACKUPS, &rig.authority());
        // GAP 1 IS NOW CLOSED. This test was written as a tripwire asserting the hole was still
        // open; the root lane refuses a sum-preserving redistribution as of the payload-count check,
        // so it is flipped to assert the refusal, exactly as its author instructed.
        let msg = format!("{:?}", r.unwrap_err());
        assert!(
            msg.contains("payload output"),
            "the refusal must name the payload outputs, got: {msg}"
        );
    }

    /// **THE SKIMMING TRIGGER — was GAP 2, now the lane's anchor. (Formerly a tripwire.)**
    ///
    /// `deed25c`'s loop ran `for i in 1..txs.len()`, so tier 0 — the trigger — was bound to nothing.
    /// Its payload output seeded the extension's law and was never itself compared to `F`. So `T`
    /// could spend a 200 000-sat funding output, pay the aggregate 1 000, and route 198 424 to the
    /// attacker, while every tier below it conserved perfectly from the lie and the receiver booked
    /// the on-chain 200 000 (`amount: tx0_output.value`, transfer_receiver.rs:1486).
    ///
    /// It was the most severe of that pair: the trigger is un-timelocked and spends `F`, so
    /// broadcasting it kills every flat backup at the same instant ([B1]) — the theft and the
    /// destruction of the fallback are one transaction.
    ///
    /// **This test was written as a tripwire asserting the hole was open, and is flipped here exactly
    /// as its author instructed.** The loop now starts at `i = 0` and the trigger's funding is
    /// `f_onchain`, an explicit parameter carrying the value the CALLER read off the chain —
    /// `Some(coin.f_value)` from `verify_bundle_bound`, `Some(parent_f_onchain_value)` from
    /// `verify_child_bundle`.
    ///
    /// **And the last assertion is the honest residue, not an oversight.** `verify_bundle` has no
    /// chain access, passes `None`, and still accepts this bundle. That is deliberate: the alternative
    /// is to measure the trigger against `bundle.f_value`, which the sender chose and co-signed
    /// against, producing a check that never fails and reads like an anchor. The unbound entry point
    /// is documented as unbound; this pins that it really is.
    #[test]
    fn a_skimming_trigger_is_refused_against_the_on_chain_funding_value() {
        let rig = rig();
        let b = rig.ladder(Skim::TriggerShort);

        // The theft, stated as arithmetic.
        let t: Transaction = parse(&b.trigger.signed_tx);
        assert_eq!(t.output.len(), 3);
        assert_eq!(t.output[2].script_pubkey, rig.attacker.spk);
        assert_eq!(b.trigger.out_value, TOKEN_FORWARD, "the trigger forwards 1 000 of a 200 000 coin");
        assert_eq!(t.output[2].value, 198_424, "…and 198 424 goes to the attacker");
        assert_eq!(
            b.levels[0].state.out_value,
            TOKEN_FORWARD - 2 * RUNG,
            "the owner's exit chain can deliver 20 sat"
        );
        // Everything BELOW the trigger conserves exactly, which is why nothing else can be what
        // refuses this bundle: each tier was built from the value its parent really forwards.
        assert_eq!(
            b.levels[0].extension.out_value,
            TOKEN_FORWARD - RUNG,
            "the extension conserves perfectly — against the trigger's lie"
        );
        assert_still_genuinely_cosigned(&b, &rig);

        // The bound path — the one `transfer_receiver.rs` calls on every laddered claim and every SSP
        // pre-pay — knows the real funding value, and now uses it.
        let msg = verify_bundle_bound(&b, NUM_SIGS, FLAT_BACKUPS, &rig.authority())
            .expect_err("a trigger that skims the funding output must be REFUSED")
            .to_string();
        assert!(
            msg.contains("the trigger is funded with 200000 sat, read from the ON-CHAIN funding output F"),
            "the refusal must name the CHAIN's funding value, got: {msg}"
        );
        assert!(
            msg.contains("payload outputs carry 199424"),
            "…and what the trigger's outputs actually carry, got: {msg}"
        );
        assert!(
            msg.contains("expected exactly 199510"),
            "…and what one rung off the real F would have been, got: {msg}"
        );
        assert!(
            msg.contains("a number the sender chose rather than one the chain agreed to"),
            "…and why every law below it was worthless without this one, got: {msg}"
        );
        assert_not_an_unrelated_refusal(&msg);

        // Same refusal through the private entry point when the chain fact is supplied.
        let direct = verify_root(&b).expect_err("…and directly").to_string();
        assert!(direct.contains("payload outputs carry 199424"));

        // THE RESIDUE, pinned deliberately: with NO chain fact there is no anchor, and this bundle is
        // internally perfect. `verify_bundle` is for re-checking a ladder you built yourself; it is
        // not an acceptance path, and its doc comment says so.
        verify_bundle(&b, NUM_SIGS, FLAT_BACKUPS).expect(
            "the UNBOUND entry point has no F to measure against and must stay honest about that — \
             if this starts failing, someone has passed `bundle.f_value` as the anchor, which proves \
             only that the sender was self-consistent",
        );
    }
}

/// # THE FORGED-YARDSTICK ATTACK, BUILT AND RUN
///
/// The third sibling of `skim_leaf_attack_tests` and `skim_root_attack_tests`, and the one that
/// bypasses both. Those two attack the OUTPUTS of a tier and are refused by the conservation laws
/// landed in `4e165e6`, `deed25c` and `d692c07`. This one leaves every output exactly where the law
/// says it should be and moves the LAW instead.
///
/// **The lever.** Every conservation check in this file computes
/// `expect = prev − committed_fee(rate) − P2A_VALUE`, and `rate` is `bundle.fee_rate` /
/// `cb.parent.fee_rate` — a plain serde `f64` carried on the conveyed bundle
/// (`TesrBundle::fee_rate`, `tesr.rs:84`). `expect` DECREASES as `rate` rises, without bound. So an
/// attacker does not have to break the equality: they solve it. Pick the value a tier should
/// forward, and there is a rate that makes that value the equality's exact right-hand side.
/// `the_forged_yardstick_solves_the_conservation_equality_for_any_forward` does that solve
/// constructively — 1 000 000 sat in, 1 010 sat out, `tier_out_value` agreeing to the satoshi.
///
/// **Where it is refused, and where it is not.** `2ad2b2d` pinned the rate to
/// `TesrParams::for_network(&cc.network).committed_fee_rate` — but it pinned it in
/// `verify_conveyed_child`, which is `async`, fetches `F` over Electrum and `/info/config` over HTTP
/// before it ever reaches the comparison, and therefore cannot be reached from a unit test. The two
/// SYNCHRONOUS verifiers underneath it — `verify_bundle_ex`/`verify_bundle`/`verify_bundle_bound` on
/// the root lane and `verify_child_bundle` on the child lane — never mention `fee_rate` except to
/// measure against it. This module runs the attack through all four of them and records what they do
/// today. Two of them accept a 99.9 % theft.
///
/// **This module is a mix of proofs and tripwires; read the names.**
///   * `honest_*` — the non-vacuity controls. Same rig, honest rate, must be ACCEPTED.
///   * `a_forged_yardstick_ladder_is_refused_when_the_rate_is_declared_honestly` and its child-lane
///     twin — the same signed transactions, byte for byte, with the single `fee_rate` field put back
///     to 2.0, must be REFUSED by the value law. This is what proves the yardstick is load-bearing
///     rather than decorative: one `f64` flips a refusal into an acceptance.
///   * `gap_*` — what the verifier does TODAY, and what it does today is accept a theft. Each fails
///     the moment the hole is closed and says so. Do not delete them; fix the verifier and flip them.
///
/// **What the attacker holds.** As in the two sibling modules, these tests build fully co-signed
/// ladders while holding both halves of the aggregate key. That is not a cheat: `cosign_tier_request`
/// (lib/src/tesr.rs:503) is handed a sighash and a prevout AMOUNT, never a transaction and never a
/// fee rate, so the SE cannot see — let alone object to — the schedule a tier was sized at. Every
/// `gap_` test additionally asserts `verify_tier_cosigned` still accepts every tier, so nothing here
/// turns on a forged signature.
#[cfg(test)]
mod forged_yardstick_attack_tests {
    use super::*;
    use electrum_client::bitcoin::{
        consensus::{deserialize, serialize},
        key::TapTweak,
        secp256k1::{KeyPair, Message, Secp256k1, SecretKey},
        sighash::{Prevouts, SighashCache, TapSighashType},
        Address, Network, ScriptBuf, Transaction, TxOut, Witness,
    };

    const NET: &str = "regtest";
    const SID: &str = "yardstick-sid";
    const CHILD_SID: &str = "yardstick-child-sid";
    /// The coin's on-chain funding outpoint. Nothing fetches it — every entry point exercised here is
    /// pure and takes the chain facts as parameters.
    const F_TXID: &str = "7777777777777777777777777777777777777777777777777777777777777777";
    const F_VOUT: u32 = 0;
    /// A whole bitcoin, so a forged schedule has room to consume nearly all of it and still leave
    /// three (root) or five (child) tiers that the builders will actually construct.
    const F_VALUE: u64 = 1_000_000;

    /// The census terms. Exact equality, so getting either wrong fails every test on `num_sigs`
    /// instead of on the property under test.
    const FLAT_BACKUPS: u32 = 1;
    const NUM_SIGS: u32 = FLAT_BACKUPS + 3;

    /// The rate every shipped preset builds at, on every network — `TesrParams::mainnet()` and
    /// `::regtest()` both carry `committed_fee_rate: 2.0`. This is the yardstick the attack replaces.
    const HONEST_RATE: f64 = 2.0;
    /// One plain rung at the honest rate: `committed_fee(2.0) + P2A_VALUE` = 250 + 240.
    const HONEST_RUNG: u64 = 490;

    /// **The forged yardstick, root lane.** Chosen so three rungs consume 99.897 % of the coin and
    /// the builders still succeed: `committed_fee(2662) = 332 750`, rung = `332 990`, and
    /// `3 × 332 990 = 998 970` of a 1 000 000-sat coin. An integer rate keeps `125.0 * rate` exactly
    /// representable, so the `ceil` in `committed_fee` is not doing anything subtle.
    const FORGED_RATE: f64 = 2662.0;
    const FORGED_RUNG: u64 = 332_990;

    /// **The forged yardstick, child lane.** The sweep document's own number
    /// (`VALUE-CONSERVATION-SWEEP.md` V5: *"Declare `fee_rate: 700.0` … each rung consumes
    /// `committed_fee(700) + 240` = 87 740"*). The child chain is five tiers deep — `T`, `X_0`, `SP`,
    /// then the child's own extension and state — so the rung has to be small enough that five of
    /// them fit.
    const FORGED_CHILD_RATE: f64 = 700.0;
    const FORGED_CHILD_RUNG: u64 = 87_740;

    // ── SCAFFOLDING (the `skim_*_attack_tests` pattern, unchanged) ──────────────────────────────

    /// A party holding BOTH halves of an aggregate key — what a blind co-sign is equivalent to.
    struct Holder {
        kp: KeyPair,
        /// The P2TR address for the UNTWEAKED internal key (BIP-341, no script tree).
        address: String,
        spk: ScriptBuf,
        /// The x-only the coordinator would have on record: UNTWEAKED, as `/info/statechain` serves it.
        recorded_xonly: String,
    }

    fn holder(seed: u8) -> Holder {
        let secp = Secp256k1::new();
        let sk = SecretKey::from_slice(&[seed; 32]).expect("valid secret key");
        let kp = KeyPair::from_secret_key(&secp, &sk);
        let (xonly, _parity) = kp.x_only_public_key();
        let address = Address::p2tr(&secp, xonly, None, Network::Regtest);
        Holder {
            kp,
            spk: address.script_pubkey(),
            address: address.to_string(),
            recorded_xonly: hex::encode(xonly.serialize()),
        }
    }

    /// Produce exactly the witness `verify_tier_cosigned` checks: a BIP-341 key-spend Schnorr
    /// signature by the aggregate over `TxOut { value: prevout_value, script_pubkey: agg_spk }` with
    /// `TapSighashType::All`. Note what it commits to — the transaction and the prevout AMOUNT. The
    /// fee RATE is not an input to a signature anywhere in this protocol, which is precisely why it
    /// has to be bound by the receiver instead.
    fn cosign(tx: &Transaction, prevout_value: u64, agg_spk: &ScriptBuf, kp: &KeyPair) -> Transaction {
        let secp = Secp256k1::new();
        let prevout = TxOut { value: prevout_value, script_pubkey: agg_spk.clone() };
        let sighash = SighashCache::new(tx)
            .taproot_key_spend_signature_hash(0, &Prevouts::All(&[prevout]), TapSighashType::All)
            .expect("taproot key-spend sighash");
        let msg = Message::from_slice(sighash.as_ref()).expect("32-byte sighash");
        let sig = secp.sign_schnorr_no_aux_rand(&msg, &kp.tap_tweak(&secp, None).to_inner());
        let mut signed = tx.clone();
        signed.input[0].witness = Witness::from_slice(&[&sig[..]]);
        signed
    }

    fn parse(tx_hex: &str) -> Transaction {
        deserialize(&hex::decode(tx_hex).expect("hex")).expect("transaction")
    }

    /// A bundle tier describing `tx`, with `txid` and `out_value` read FROM the transaction. Every
    /// declared field in this module is honest — the whole point of the attack is that it needs no
    /// lie about any transaction. The only false statement anywhere is the fee rate.
    fn tier(tx: &Transaction, csv: Option<u16>, payload_vout: u32) -> TesrTier {
        TesrTier {
            txid: tx.txid().to_string(),
            signed_tx: hex::encode(serialize(tx)),
            out_value: tx.output[payload_vout as usize].value,
            csv,
            payload_vout,
        }
    }

    struct Rig {
        agg: Holder,
        child: Holder,
        owner: Holder,
        receiver: Holder,
        params: mercurylib::tesr::TesrParams,
    }

    fn rig() -> Rig {
        Rig {
            agg: holder(0x61),
            child: holder(0x62),
            owner: holder(0x63),
            receiver: holder(0x64),
            // The SCHEDULE is honest — the same regtest preset a receiver derives for itself. Only
            // `fee_rate` is forged, so no CSV bound, no `bind_declared_csv`, and no census term can
            // be what refuses (or accepts) anything below.
            params: mercurylib::tesr::TesrParams::regtest(),
        }
    }

    impl Rig {
        /// The whole-coin root ladder `T → X_0 → S_0`, built entirely at `build_rate` and genuinely
        /// co-signed by `A`, then DECLARED at `declared_rate`.
        ///
        /// Passing the same value twice is the honest case (and the attack: the attacker's ladder is
        /// perfectly self-consistent, which is the whole trick). Passing different values is the
        /// control — identical bytes, one field changed — and it is what proves the declared rate is
        /// the thing the laws measure against.
        ///
        /// No output is ever touched. Each tier is `[payload, P2A]`, exactly as `build_tier_tx`
        /// emits it.
        fn ladder(&self, build_rate: f64, declared_rate: f64) -> TesrBundle {
            let p = self.params;
            let a = &self.agg;

            let t = mercurylib::tesr::build_trigger(F_TXID, F_VOUT, F_VALUE, &a.address, NET, build_rate)
                .expect("trigger");
            let t_tx = cosign(&parse(&t.tx_hex), F_VALUE, &a.spk, &a.kp);

            let x = mercurylib::tesr::build_extension(
                &t.txid, t.out_value, &a.address, NET, p.ext_csv(0), build_rate,
            )
            .expect("extension");
            let x_tx = cosign(&parse(&x.tx_hex), t.out_value, &a.spk, &a.kp);

            let s = mercurylib::tesr::build_state(
                &x.txid, x.out_value, &self.owner.address, NET, p.state_csv(0), build_rate,
            )
            .expect("state");
            let s_tx = cosign(&parse(&s.tx_hex), x.out_value, &a.spk, &a.kp);

            TesrBundle {
                version: 1,
                statechain_id: SID.into(),
                network: NET.into(),
                fee_rate: declared_rate,
                agg_address: a.address.clone(),
                owner_exit_address: self.owner.address.clone(),
                f_txid: F_TXID.into(),
                f_vout: F_VOUT,
                f_value: F_VALUE,
                trigger: tier(&t_tx, None, t.payload_vout),
                levels: vec![TesrLevel {
                    extension: tier(&x_tx, Some(p.ext_csv(0)), x.payload_vout),
                    state: tier(&s_tx, Some(p.state_csv(0)), s.payload_vout),
                }],
                m: 0,
                superseded_states: vec![],
                superseded_extensions: vec![],
                // HONEST — the receiver's own preset. A forged schedule is a different attack
                // (VALUE-CONSERVATION-SWEEP.md §4, "the `params` schedule as an attack surface");
                // keeping it honest here is what makes the fee rate the only variable.
                params: p,
                rgb: None,
            }
        }

        /// The parent segment of a split: `T → X_0 → SP`, with `SP` a spine split state funding one
        /// child slot. Returns the bundle and the slot's value.
        fn parent_segment(&self, build_rate: f64, declared_rate: f64) -> (TesrBundle, u64) {
            let p = self.params;
            let a = &self.agg;

            let t = mercurylib::tesr::build_trigger(F_TXID, F_VOUT, F_VALUE, &a.address, NET, build_rate)
                .expect("trigger");
            let t_tx = cosign(&parse(&t.tx_hex), F_VALUE, &a.spk, &a.kp);

            let x = mercurylib::tesr::build_extension(
                &t.txid, t.out_value, &a.address, NET, p.ext_csv(0), build_rate,
            )
            .expect("extension");
            let x_tx = cosign(&parse(&x.tx_hex), t.out_value, &a.spk, &a.kp);

            let slot = mercurylib::tesr::tier_out_total(x.out_value, 1, build_rate).expect("slot");
            let sp = mercurylib::tesr::build_split_state(
                &x.txid,
                x.out_value,
                &[(self.child.address.clone(), slot)],
                NET,
                SPINE_CSV,
                build_rate,
            )
            .expect("split state");
            let sp_tx = cosign(&parse(&sp.tx_hex), x.out_value, &a.spk, &a.kp);

            let bundle = TesrBundle {
                version: 1,
                statechain_id: SID.into(),
                network: NET.into(),
                fee_rate: declared_rate,
                agg_address: a.address.clone(),
                owner_exit_address: self.owner.address.clone(),
                f_txid: F_TXID.into(),
                f_vout: F_VOUT,
                f_value: F_VALUE,
                trigger: tier(&t_tx, None, t.payload_vout),
                levels: vec![TesrLevel {
                    extension: tier(&x_tx, Some(p.ext_csv(0)), x.payload_vout),
                    state: tier(&sp_tx, Some(SPINE_CSV), sp.payload_vout),
                }],
                m: 0,
                superseded_states: vec![],
                superseded_extensions: vec![],
                params: p,
                rgb: None,
            };
            (bundle, slot)
        }

        /// The full conveyed child: an honest-shaped parent segment plus the child's own two tiers,
        /// all built at `build_rate` and all declared at `declared_rate`.
        fn child_bundle(&self, build_rate: f64, declared_rate: f64) -> ChildTesrBundle {
            let p = self.params;
            let (parent, slot) = self.parent_segment(build_rate, declared_rate);
            let sp_txid = parent.current().state.txid.clone();
            let c = &self.child;

            let xc = mercurylib::tesr::build_extension(
                &sp_txid, slot, &c.address, NET, p.ext_csv(0), build_rate,
            )
            .expect("child extension");
            let xc_tx = cosign(&parse(&xc.tx_hex), slot, &c.spk, &c.kp);

            let sc = mercurylib::tesr::build_state(
                &xc.txid, xc.out_value, &self.receiver.address, NET, p.state_csv(0), build_rate,
            )
            .expect("child state");
            let sc_tx = cosign(&parse(&sc.tx_hex), xc.out_value, &c.spk, &c.kp);

            ChildTesrBundle {
                parent,
                parent_statechain_id: SID.into(),
                sp_vout: 0,
                child_statechain_id: CHILD_SID.into(),
                child_owner_exit_address: self.receiver.address.clone(),
                child_extension: tier(&xc_tx, Some(p.ext_csv(0)), xc.payload_vout),
                child_state: tier(&sc_tx, Some(p.state_csv(0)), sc.payload_vout),
                child_superseded_states: vec![],
                child_superseded_extensions: vec![],
                ancestors: vec![],
                rgb: None,
                // `verify_child_bundle` takes the parent's flat-backup COUNT as a parameter; the
                // transactions themselves are only validated by `verify_conveyed_child`, one level up.
                parent_flat_backups: vec![],
            }
        }

        /// The authority a receiver derives from the CHAIN and the coordinator. The forged rate does
        /// not touch any of it, which is exactly why `verify_bundle_bound` cannot be what refuses the
        /// attack.
        fn authority(&self) -> CoinAuthority {
            CoinAuthority {
                statechain_id: SID.into(),
                f_txid: F_TXID.into(),
                f_vout: F_VOUT,
                f_value: F_VALUE,
                f_spk_hex: hex::encode(self.agg.spk.as_bytes()),
                se_aggregate_pubkey: Some(self.agg.recorded_xonly.clone()),
            }
        }
    }

    /// The funding value is the CHAIN fact a receiver would have fetched. It is deliberately supplied
    /// even here: the ladders in this module are built at a forged RATE, not a forged funding value,
    /// so the trigger anchor is satisfied by construction and cannot be what refuses (or accepts)
    /// anything below. That is the point — this module isolates the yardstick.
    fn verify_root(b: &TesrBundle) -> Result<()> {
        verify_bundle_ex(b, NUM_SIGS, FLAT_BACKUPS, false, Some(F_VALUE))
    }

    fn verify_child(rig: &Rig, cb: &ChildTesrBundle) -> Result<()> {
        verify_child_bundle(
            cb,
            &hex::encode(rig.agg.spk.as_bytes()),
            F_VALUE,
            // The parent's census: one deposit backup + T + X + SP.
            1 + 3,
            1,
            Some(&rig.agg.recorded_xonly),
            true, // the parent segment is terminal, as the protocol requires
            // A derived child slot has no flat backup (CHILD_V2_BASELINE = 0) — just its two tiers.
            2,
            0,
            Some(&rig.child.recorded_xonly),
            &[],
            &rig.receiver.address,
        )
    }

    /// Every tier of a root ladder really is co-signed by `A`, so no `gap_` result below can be
    /// blamed on a forged signature.
    fn assert_root_genuinely_cosigned(b: &TesrBundle, rig: &Rig) {
        let tiers = b.exit_tiers();
        let txs: Vec<Transaction> = tiers.iter().map(|t| parse(&t.signed_tx)).collect();
        let mut prev = b.f_value;
        for (i, tx) in txs.iter().enumerate() {
            verify_tier_cosigned(tx, prev, &rig.agg.spk).unwrap_or_else(|e| {
                panic!("tier {i} is not a genuine co-sign, so this test would prove nothing: {e}")
            });
            prev = tx.output[tiers[i].payload_vout as usize].value;
        }
    }

    fn assert_child_genuinely_cosigned(cb: &ChildTesrBundle, rig: &Rig) {
        let sp = parse(&cb.parent.current().state.signed_tx);
        let funding = sp.output[cb.sp_vout as usize].value;
        let ext = parse(&cb.child_extension.signed_tx);
        verify_tier_cosigned(&ext, funding, &rig.child.spk)
            .expect("the child extension really is co-signed by A_child");
        let st = parse(&cb.child_state.signed_tx);
        verify_tier_cosigned(
            &st,
            ext.output[cb.child_extension.payload_vout as usize].value,
            &rig.child.spk,
        )
        .expect("and so is the child state below it");
    }

    /// A refusal must name the value law. These are the causes that would make a control test pass
    /// for the WRONG reason.
    fn assert_not_an_unrelated_refusal(msg: &str) {
        for unrelated in [
            "not co-signed",
            "num_sigs",
            "CSV",
            "does not spend",
            "pays the wrong output",
            "does not pay the aggregate",
            "colour",
            "decoy",
            "malformed ladder",
            "terminal",
            "Model A",
        ] {
            assert!(
                !msg.contains(unrelated),
                "refused for an UNRELATED reason ({unrelated:?}) — this test would be worthless: {msg}"
            );
        }
    }

    /// Every tier in this module is `[payload, P2A]`, so the value the forged schedule frees up is
    /// not paid to anybody — it is left to the miner as fee. That is what makes the tier RELAY, and
    /// it is the sense in which this attack destroys the receiver's money rather than transferring
    /// it. (Pocketing it as well needs a second payload output, which is
    /// `skim_root_attack_tests::gap_a_sum_preserving_redistribution_on_the_root_lane_is_still_accepted`
    /// — an orthogonal hole. Either way the receiver's loss is identical, and the receiver's loss is
    /// what the value laws exist to prevent.)
    fn miner_fee(tx: &Transaction, prev: u64) -> u64 {
        prev - tx.output.iter().map(|o| o.value).sum::<u64>()
    }

    // ── 1. THE ARITHMETIC: THE EQUALITY IS SOLVABLE FOR ANY FORWARD VALUE ───────────────────────

    /// **The claim that makes the binding load-bearing, as pure arithmetic against the real
    /// functions.**
    ///
    /// The conservation law is an EQUALITY, `Σ payload outputs == prev − committed_fee(rate) − P2A`.
    /// An attacker who cannot break an equality can still choose which equality they are asked to
    /// satisfy, because `rate` is theirs. `committed_fee` is linear in `rate`
    /// (`ceil(TIER_VBYTES · rate)`), so for any target forward `f < prev − P2A` the rate
    /// `(prev − f − P2A) / TIER_VBYTES` makes `f` the exact right-hand side. There is no rounding
    /// slack to hide behind and no bound to run into: this is a solve, not an approximation.
    ///
    /// Run forwards: 1 000 000 sat in, 1 010 sat out, at 7 990 sat/vB — and `tier_out_value` agrees
    /// to the satoshi, which is the number `verify_bundle_ex` and `verify_child_bundle` compare
    /// against.
    #[test]
    fn the_forged_yardstick_solves_the_conservation_equality_for_any_forward() {
        let prev = F_VALUE;

        // The honest law, for scale: one rung, 490 sat, 0.049 % of the coin.
        assert_eq!(mercurylib::tesr::committed_fee(HONEST_RATE), 250);
        assert_eq!(
            mercurylib::tesr::tier_out_value(prev, HONEST_RATE),
            Some(prev - HONEST_RUNG)
        );

        // THE SOLVE. Pick what the tier should forward; derive the rate that makes the equality true.
        // 1 010 is chosen so `prev − f − P2A` is a whole multiple of `TIER_VBYTES` and the rate comes
        // out an exact integer — the arithmetic below is then doing nothing that floating point could
        // be blamed for.
        let target = 1_010u64;
        let rate = (prev - target - mercurylib::tesr::P2A_VALUE) as f64
            / mercurylib::tesr::TIER_VBYTES as f64;
        assert_eq!(rate, 7_990.0, "the rate that makes 1 010 sat the law's expected forward");
        assert_eq!(
            mercurylib::tesr::tier_out_value(prev, rate),
            Some(target),
            "the verifier's own function agrees: at 7 990 sat/vB a tier forwarding 1 010 sat out of \
             1 000 000 satisfies the conservation equality EXACTLY"
        );
        // …and the multi-payload form the split state is measured by solves identically.
        assert_eq!(mercurylib::tesr::tier_out_total(prev, 1, rate), Some(target));

        // The same solve for a hundred different targets, so this is a property and not one lucky
        // pair. Every one of them is a value a tier may forward while the law reports success.
        for k in 1..=100u64 {
            let target = k * 125; // keeps `prev − target − P2A` ≡ 0 (mod TIER_VBYTES)
            let rate = (prev - target - mercurylib::tesr::P2A_VALUE) as f64
                / mercurylib::tesr::TIER_VBYTES as f64;
            assert_eq!(
                mercurylib::tesr::tier_out_value(prev, rate),
                Some(target),
                "the equality is satisfiable at a forward of {target} sat (rate {rate})"
            );
        }

        // MONOTONICITY, which is why the direction matters: a *higher* declared rate always demands
        // *less*. The comment at `verify_bundle_ex`'s value block argues the sender-declared rate is
        // "acceptable in this direction … because a higher declared rate makes the expected forward
        // value SMALLER, and a tier forwarding less than its own declared schedule demands is exactly
        // what the equality refuses". Both halves are true and the conclusion does not follow: the
        // attacker does not forward less than their schedule demands, they declare a schedule that
        // demands almost nothing and then meet it exactly.
        let mut last = u64::MAX;
        for rate in [2.0, 10.0, 100.0, 700.0, 2662.0, 7990.0] {
            let v = mercurylib::tesr::tier_out_value(prev, rate).expect("still fits");
            assert!(v < last, "expected forward must fall as the declared rate rises");
            last = v;
        }
    }

    /// The two per-rung prices this module trades on, pinned so a schedule change surfaces here
    /// rather than as a mystery three tests down. A rung is 490 sat plain at the committed
    /// 2 sat/vB; the coloured tier carries one whole extra P2TR-sized output (the opret) and costs
    /// 576.
    #[test]
    fn the_rung_prices_this_attack_inflates() {
        assert_eq!(
            mercurylib::tesr::committed_fee(HONEST_RATE) + mercurylib::tesr::P2A_VALUE,
            HONEST_RUNG,
            "a plain rung at 2 sat/vB is 125 vB * 2 + 240"
        );
        assert_eq!(
            crate::rgb::colored_committed_fee(1, HONEST_RATE) + mercurylib::tesr::P2A_VALUE,
            576,
            "a coloured rung at 2 sat/vB is 168 vB * 2 + 240 — one extra P2TR-sized output"
        );
        // And the forged ones, which is what the ladders below are built at.
        assert_eq!(
            mercurylib::tesr::committed_fee(FORGED_RATE) + mercurylib::tesr::P2A_VALUE,
            FORGED_RUNG,
            "the root-lane forged rung: 679x the honest one"
        );
        assert_eq!(
            mercurylib::tesr::committed_fee(FORGED_CHILD_RATE) + mercurylib::tesr::P2A_VALUE,
            FORGED_CHILD_RUNG,
            "the child-lane forged rung — the sweep document's own 87 740"
        );
    }

    /// **The receiver's preset is the only honest yardstick, and it is a CONSTANT.** `2ad2b2d`'s
    /// binding is exact equality against `TesrParams::for_network(network).committed_fee_rate`, which
    /// works only because every establish path builds at that same constant
    /// (`establish_auto` → `p.committed_fee_rate`, `build_colored_ladder_auto` likewise). If any
    /// shipped preset ever moved off 2.0, or the two presets diverged from each other in a way the
    /// network string could not resolve, that binding would start refusing honest traffic — so the
    /// property is pinned here rather than assumed.
    #[test]
    fn the_receivers_yardstick_is_a_per_network_constant() {
        for net in ["bitcoin", "mainnet", "regtest", "signet", "testnet"] {
            assert_eq!(
                mercurylib::tesr::TesrParams::for_network(net).committed_fee_rate,
                HONEST_RATE,
                "{net}: every shipped preset builds ladders at the same committed rate"
            );
        }
        // The comparison `verify_conveyed_child` makes, on the values this module forges.
        let want = mercurylib::tesr::TesrParams::for_network(NET).committed_fee_rate;
        assert_ne!(FORGED_RATE, want);
        assert_ne!(FORGED_CHILD_RATE, want);
        // …and the trap that check has to avoid: `max_fee_rate` is 1.0 on the regtest profile, BELOW
        // the 2.0 every honest ladder carries, so using it as the ceiling would refuse all legitimate
        // traffic. The two numbers are not interchangeable and the first draft of the check used the
        // wrong one.
        assert!(want > 1.0, "the committed rate is above the flat-backup fee ceiling, not below it");
    }

    // ── 2. NON-VACUITY: THE HONEST RIG IS ACCEPTED ─────────────────────────────────────────────

    /// **The control every `gap_` test below depends on.** The identical construction at the honest
    /// rate must be ACCEPTED — through the private entry point, the public wrapper, and the bound
    /// acceptance path the whole-coin claim lane actually calls
    /// (`transfer_receiver.rs` → `verify_bundle_bound`).
    #[test]
    fn honest_root_ladder_is_accepted() {
        let rig = rig();
        let b = rig.ladder(HONEST_RATE, HONEST_RATE);

        assert_eq!(b.trigger.out_value, F_VALUE - HONEST_RUNG);
        assert_eq!(b.levels[0].extension.out_value, F_VALUE - 2 * HONEST_RUNG);
        assert_eq!(
            b.levels[0].state.out_value,
            F_VALUE - 3 * HONEST_RUNG,
            "998 530 of a 1 000 000-sat coin reaches the owner — 99.85 %"
        );
        assert_root_genuinely_cosigned(&b, &rig);

        verify_root(&b).expect("an honest, fully co-signed root ladder must be ACCEPTED");
        verify_bundle(&b, NUM_SIGS, FLAT_BACKUPS).expect("…through the public wrapper too");
        verify_bundle_bound(&b, NUM_SIGS, FLAT_BACKUPS, &rig.authority())
            .expect("…and through the bound acceptance path the claim/SSP lanes call");
    }

    /// The child-lane control: an honest conveyed child is accepted, and the gap between what the
    /// claim path BOOKS (`SP.out[j]`, the slot) and what the child's exit chain can DELIVER is
    /// exactly two rungs — 980 sat. `VALUE-CONSERVATION-SWEEP.md` §8 downgrades that gap from theft
    /// to "a bounded convention"; this test is where the word *bounded* is checked, and
    /// `gap_a_forged_yardstick_child_is_still_accepted_by_the_sync_verifier` is where it stops being
    /// true.
    #[test]
    fn honest_child_bundle_is_accepted_and_its_booking_gap_is_two_rungs() {
        let rig = rig();
        let cb = rig.child_bundle(HONEST_RATE, HONEST_RATE);

        let sp = parse(&cb.parent.current().state.signed_tx);
        let slot = sp.output[0].value;
        assert_eq!(slot, F_VALUE - 3 * HONEST_RUNG, "the child's slot on SP");
        assert_eq!(cb.child_state.out_value, slot - 2 * HONEST_RUNG);
        assert_eq!(
            slot - cb.child_state.out_value,
            2 * HONEST_RUNG,
            "booked minus reachable is exactly two rungs — 980 sat, the §8 convention"
        );
        assert_child_genuinely_cosigned(&cb, &rig);

        verify_child(&rig, &cb).expect("an honest, fully co-signed child bundle must be ACCEPTED");
    }

    // ── 3. THE CONTROL THAT PROVES THE YARDSTICK IS LOAD-BEARING ───────────────────────────────

    /// **One `f64`, and the same bytes go from refused to accepted.**
    ///
    /// Identical signed transactions to `gap_a_forged_yardstick_root_ladder_is_still_accepted` —
    /// same txids, same witnesses, same outputs — with `fee_rate` put back to the honest 2.0. The
    /// value law now refuses every tier, naming the value it was funded with and what its payload
    /// outputs carry. Which is to say: the tiers ARE skimming; the verifier can see it perfectly
    /// well; it is measuring with the attacker's ruler.
    ///
    /// **The refusal is asserted twice, at two hops, on purpose.** The trigger-to-`F` anchor now
    /// makes tier 0 the FIRST hop to break — a ladder built at 2 662 sat/vB forwards 667 010 out of a
    /// funding output the chain says holds 1 000 000, and one honest rung off 1 000 000 is 999 510.
    /// Through `verify_bundle`, which has no chain fact and therefore no anchor, the same bytes fall
    /// instead to tier 1's RELATIVE law, with the numbers this test has always named. Both matter:
    /// the first shows the anchor catches a forged schedule at the root, the second shows the
    /// relative chain still catches it without one — so neither check is load-bearing alone.
    #[test]
    fn a_forged_yardstick_ladder_is_refused_when_the_rate_is_declared_honestly() {
        let rig = rig();
        let attack = rig.ladder(FORGED_RATE, FORGED_RATE);
        let same_bytes_honest_rate = rig.ladder(FORGED_RATE, HONEST_RATE);

        // The two bundles differ in exactly one field.
        assert_eq!(attack.trigger.signed_tx, same_bytes_honest_rate.trigger.signed_tx);
        assert_eq!(
            attack.levels[0].extension.signed_tx,
            same_bytes_honest_rate.levels[0].extension.signed_tx
        );
        assert_eq!(attack.levels[0].state.signed_tx, same_bytes_honest_rate.levels[0].state.signed_tx);
        assert_ne!(attack.fee_rate, same_bytes_honest_rate.fee_rate);

        // (a) ANCHORED — the chain fact in hand, so the very first hop is the one that breaks.
        let msg = verify_root(&same_bytes_honest_rate)
            .expect_err("measured against the RECEIVER's rate, this ladder is a skim and is refused")
            .to_string();
        assert!(
            msg.contains(&format!("the trigger is funded with {F_VALUE} sat, read from the ON-CHAIN funding output F")),
            "the refusal must name the on-chain funding value, got: {msg}"
        );
        assert!(
            msg.contains(&format!("payload outputs carry {}", F_VALUE - FORGED_RUNG)),
            "…and what the trigger actually forwards, got: {msg}"
        );
        assert!(
            msg.contains(&format!("expected exactly {}", F_VALUE - HONEST_RUNG)),
            "…and what one HONEST rung off the real F would have been, got: {msg}"
        );
        assert_not_an_unrelated_refusal(&msg);

        // (b) UNANCHORED — no chain fact, so the trigger's hop is skipped and tier 1's relative law
        //     is what refuses. These are the assertions this control was written with.
        let msg = verify_bundle(&same_bytes_honest_rate, NUM_SIGS, FLAT_BACKUPS)
            .expect_err("the relative laws refuse it too, with no anchor at all")
            .to_string();
        assert!(
            msg.contains(&format!("tier 1 is funded with {} sat", F_VALUE - FORGED_RUNG)),
            "the refusal must name the offending tier and its funding, got: {msg}"
        );
        assert!(
            msg.contains(&format!("payload outputs carry {}", F_VALUE - 2 * FORGED_RUNG)),
            "…and what it actually forwards, got: {msg}"
        );
        assert!(
            msg.contains("would leave the owner's exit chain"),
            "…and where the difference goes, got: {msg}"
        );
        assert_not_an_unrelated_refusal(&msg);

        // The converse control, so this cannot be dismissed as "the value law just dislikes low
        // forwards": an HONESTLY BUILT ladder declared at the FORGED rate is refused too, because the
        // law is an equality and honest outputs are now too LARGE for the inflated schedule.
        //
        // The accepted causes are deliberately two, and which one fires tells you whether GAP A is
        // still open. TODAY it is the value law, because nothing looks at the declared rate. Once
        // GAP A is closed the rate binding fires first and this assertion keeps holding — that is
        // intentional, so closing the hole does not break the control that motivated it.
        let honest_bytes_forged_rate = rig.ladder(HONEST_RATE, FORGED_RATE);
        let msg = verify_root(&honest_bytes_forged_rate)
            .expect_err("the equality is two-sided")
            .to_string();
        assert!(
            msg.contains(&format!("payload outputs carry {}", F_VALUE - 2 * HONEST_RUNG))
                || msg.contains(&format!("{FORGED_RATE}")),
            "must be refused either by the value law or by a rate binding, got: {msg}"
        );
        assert_not_an_unrelated_refusal(&msg);
    }

    /// The child-lane twin of the control above: identical child bytes, `fee_rate` honest, refused by
    /// the conveyed child's own conservation hop.
    #[test]
    fn a_forged_yardstick_child_is_refused_when_the_rate_is_declared_honestly() {
        let rig = rig();
        let cb = rig.child_bundle(FORGED_CHILD_RATE, HONEST_RATE);
        assert_child_genuinely_cosigned(&cb, &rig);

        let msg = verify_child(&rig, &cb)
            .expect_err("measured against the RECEIVER's rate this child is a skim")
            .to_string();
        assert!(
            msg.contains("payload outputs carry") || msg.contains("forwards only"),
            "the refusal must name the value law, got: {msg}"
        );
        assert_not_an_unrelated_refusal(&msg);
    }

    // ── 4. THE HOLES, AS THEY STAND TODAY ──────────────────────────────────────────────────────
    //
    // The three tests below assert what the SYNCHRONOUS verifiers do today, and what they do today is
    // accept a theft. They are tripwires, not endorsements: each fails the moment the hole is closed
    // and tells you how to flip it.

    /// **GAP A — the root lane has no yardstick binding at all, and the claim path calls it
    /// directly.**
    ///
    /// `verify_bundle_ex` mentions `fee_rate` in exactly two places: the two branches of
    /// `rung_forward`, i.e. only to measure against it. `verify_bundle_bound` — which
    /// `transfer_receiver.rs` calls on every laddered whole-coin claim, and `prepay_flat_census` on
    /// every SSP pre-pay — binds the statechain id, the funding outpoint, `f_value`, the aggregate
    /// address and the coordinator's recorded aggregate, and not the rate. `2ad2b2d` put the binding
    /// in `verify_conveyed_child`, which is on the CHILD lane only.
    ///
    /// So: a ladder declaring 2 662 sat/vB over a 1 000 000-sat coin is internally perfect, fully
    /// co-signed, structurally identical to an honest one, and delivers **1 030 sat** to the owner.
    /// The receiver books the on-chain funding value — `amount: tx0_output.value`, assigned at
    /// `transfer_receiver.rs:1486` — so a wallet displays 1 000 000 for a coin worth a thousandth of
    /// that.
    ///
    /// **And the flat backups are not the fallback they look like.** `T` is un-timelocked
    /// (`TRIGGER_SEQUENCE` disables the relative lock) and spends `F`; every prior owner retains a
    /// co-signed copy ([B1]). The moment one is broadcast, every flat backup — all of which also
    /// spend `F` — is void. The theft and the destruction of the slow path are one transaction.
    ///
    /// **The fix** is `2ad2b2d`'s, moved down a level: compare `bundle.fee_rate` to
    /// `TesrParams::for_network(&bundle.network).committed_fee_rate` at the top of `verify_bundle_ex`,
    /// before any value law. Honest bundles pass with equality — `the_receivers_yardstick_is_a_per_
    /// network_constant` above is the proof that they do.
    #[test]
    fn gap_a_forged_yardstick_root_ladder_is_still_accepted() {
        let rig = rig();
        let b = rig.ladder(FORGED_RATE, FORGED_RATE);

        // The theft, as arithmetic.
        assert_eq!(b.fee_rate, FORGED_RATE, "1 331x the rate any establish path builds at");
        assert_eq!(b.trigger.out_value, F_VALUE - FORGED_RUNG);
        assert_eq!(b.levels[0].extension.out_value, F_VALUE - 2 * FORGED_RUNG);
        assert_eq!(
            b.levels[0].state.out_value,
            1_030,
            "the owner's exit chain can deliver 1 030 sat of a 1 000 000-sat coin"
        );
        assert_eq!(
            F_VALUE - b.levels[0].state.out_value,
            998_970,
            "99.897 % of the coin never reaches the owner"
        );

        // Nothing structural distinguishes it from an honest ladder. Two outputs per tier, the payee
        // is right, the declared values are right, and every tier is a genuine co-sign.
        for (i, t) in b.exit_tiers().iter().enumerate() {
            let tx = parse(&t.signed_tx);
            assert_eq!(tx.output.len(), 2, "tier {i} is [payload, P2A] — no extra output to notice");
            assert_eq!(
                tx.output[t.payload_vout as usize].value, t.out_value,
                "tier {i}'s declared out_value is HONEST"
            );
            assert_eq!(
                tx.output[1].value,
                mercurylib::tesr::P2A_VALUE,
                "tier {i}'s anchor is untouched"
            );
        }
        let s = parse(&b.levels[0].state.signed_tx);
        assert_eq!(s.output[0].script_pubkey, rig.owner.spk, "and the OWNER is the payee throughout");
        assert_root_genuinely_cosigned(&b, &rig);

        // Where the money goes: each tier pays 332 750 sat to the miner and still relays.
        let t = parse(&b.trigger.signed_tx);
        assert_eq!(miner_fee(&t, F_VALUE), mercurylib::tesr::committed_fee(FORGED_RATE));
        assert!(
            t.output.iter().map(|o| o.value).sum::<u64>() < F_VALUE,
            "outputs stay below the input, so the tier is consensus-valid and would really confirm"
        );

        let r = verify_bundle_bound(&b, NUM_SIGS, FLAT_BACKUPS, &rig.authority());
        assert!(
            r.is_ok(),
            "GAP A HAS BEEN CLOSED — this tripwire has done its job. The root lane now binds \
             `fee_rate` to the receiver's own preset ({:?}); flip this test to `expect_err`, assert \
             the refusal names the declared rate, and strike GAP A from the module doc comment.",
            r.as_ref().err()
        );
        // …and through the other two root entry points, so no caller is accidentally safe.
        assert!(verify_root(&b).is_ok(), "GAP A: `verify_bundle_ex` accepts it");
        assert!(
            verify_bundle(&b, NUM_SIGS, FLAT_BACKUPS).is_ok(),
            "GAP A: the public `verify_bundle` wrapper accepts it"
        );
    }

    /// **GAP B — on the child lane the binding exists, but only above the synchronous verifier.**
    ///
    /// `verify_child_bundle` is `pub`, synchronous, and carries every conservation law the child lane
    /// has. The yardstick binding is not in it: it is in `verify_conveyed_child`, ~150 lines further
    /// up, behind an Electrum `transaction_get` and an `/info/config` HTTP round-trip. Any caller
    /// that reaches the verifier without going through the async wrapper — a watchtower pass, a
    /// re-verification of stored state, a future pre-pay path, the E2Es at
    /// `clients/tests/rust/src/sdk58_inladder_split.rs:85` and
    /// `sdk70_verifier_binding_adversarial.rs:573` — inherits none of it.
    ///
    /// The bundle below declares 700 sat/vB, is accepted by `verify_child_bundle`, and turns §8's
    /// "bounded convention" into an attacker-sized hole: the claim path books `SP.out[j]` = 736 780
    /// and the child's exit chain delivers 561 300. The gap is `2 × rung` in both cases — that is
    /// exactly the point, because `rung` is a function of the rate the attacker declared. 980 sat
    /// honest, 175 480 sat forged.
    ///
    /// **The fix** is the same one line as GAP A, in `verify_child_bundle` against
    /// `cb.parent.fee_rate` — belt and braces with the async check rather than instead of it, so the
    /// property holds for every caller of the verifier rather than for one caller of one wrapper.
    #[test]
    fn gap_a_forged_yardstick_child_is_still_accepted_by_the_sync_verifier() {
        let rig = rig();
        let cb = rig.child_bundle(FORGED_CHILD_RATE, FORGED_CHILD_RATE);

        let sp = parse(&cb.parent.current().state.signed_tx);
        let slot = sp.output[0].value;
        assert_eq!(slot, F_VALUE - 3 * FORGED_CHILD_RUNG, "736 780 — what the claim path BOOKS");
        assert_eq!(
            cb.child_state.out_value,
            slot - 2 * FORGED_CHILD_RUNG,
            "561 300 — what the child's exit chain can DELIVER"
        );
        assert_eq!(
            slot - cb.child_state.out_value,
            2 * FORGED_CHILD_RUNG,
            "the §8 'bounded convention' is 175 480 sat here, against 980 on an honest bundle — the \
             bound is the attacker's to choose"
        );
        assert_eq!(2 * FORGED_CHILD_RUNG, 175_480);
        assert_child_genuinely_cosigned(&cb, &rig);
        // Every tier is the honest two-output shape; there is nothing structural to catch.
        for t in [&cb.child_extension, &cb.child_state] {
            let tx = parse(&t.signed_tx);
            assert_eq!(tx.output.len(), 2);
            assert_eq!(tx.output[t.payload_vout as usize].value, t.out_value);
        }

        let r = verify_child(&rig, &cb);
        assert!(
            r.is_ok(),
            "GAP B HAS BEEN CLOSED — this tripwire has done its job. `verify_child_bundle` now binds \
             `cb.parent.fee_rate` itself rather than relying on `verify_conveyed_child` ({:?}); flip \
             this test to `expect_err` and strike GAP B from the module doc comment.",
            r.as_ref().err()
        );
    }

    /// **GAP C — the yardstick is an unvalidated `f64`, and a large one is a remote panic.**
    ///
    /// `committed_fee` is `(TIER_VBYTES as f64 * rate).ceil() as u64`. The cast SATURATES, so a huge
    /// declared rate yields `u64::MAX`, and `tier_out_value`'s `committed_fee(rate) + P2A_VALUE` is
    /// an unchecked add — a panic in any build with overflow checks on, which is every debug and test
    /// build in this workspace (no `[profile]` overrides in the root `Cargo.toml`). It is reached
    /// straight from `rung_forward` inside `verify_bundle_ex`, on a field a sender fills in, so it is
    /// a remote denial of service on the claim path and on the SSP pre-pay path.
    ///
    /// This is `VALUE-CONSERVATION-SWEEP.md` V5's "secondary, same site" item, which is still open:
    /// the document asks for `checked_add` and a rejection of non-finite / `<= 0` rates *in addition
    /// to* the binding, "because the arithmetic is public API".
    ///
    /// The test asserts the panic rather than an error, because a panic is what happens. When the
    /// arithmetic is fixed this test fails, and the fix is to assert the typed refusal instead.
    #[test]
    fn an_absurd_declared_rate_is_refused_not_panicked() {
        let rig = rig();
        // Structurally perfect, honestly built, absurdly declared.
        let b = rig.ladder(HONEST_RATE, 1e18);

        // GAP C IS NOW CLOSED. This ran under `catch_unwind` because an absurd rate PANICKED the
        // verifier: `committed_fee` saturates its `f64 -> u64` cast at `u64::MAX`, and the caller
        // then added `P2A_VALUE` to it, overflowing in debug. Both additions are checked now, so the
        // verifier REFUSES instead of unwinding — which matters because `fee_rate` is attacker-
        // supplied and a panic takes down the whole claim pass, not one coin.
        let r = verify_root(&b);
        assert!(r.is_err(), "an absurd declared rate must be refused, got {r:?}");

        // The underlying arithmetic, isolated — this is the public API the sweep asks to harden.
        assert_eq!(
            mercurylib::tesr::committed_fee(1e18),
            u64::MAX,
            "the f64 -> u64 cast saturates rather than erroring, and the caller then adds 240 to it"
        );
    }
}

/// # THE WRONG-PAYEE ATTACK, BUILT AND RUN
///
/// The third sibling of `skim_leaf_attack_tests` and `skim_root_attack_tests`, over the property those
/// two do NOT touch: **where a tier pays**, as opposed to how much it forwards.
///
/// `docs/utexo/VALUE-CONSERVATION-SWEEP.md` §1 states the class in one line — *"A signature over a tier
/// binds that tier's INPUT amount and nothing else"* — and then names four independent properties, of
/// which the FIRST is `payload_out(tx).script_pubkey == the aggregate it is supposed to pay`. V2(b)
/// records that the ancestor loop had no such check at all: `ext0 = seg.extension.payload_out(&ext_tx,…)`
/// was a bare index and `ext0.script_pubkey` was compared to nothing, while the very next line fed
/// `ext0.value` to the state's co-sign check as a prevout amount — i.e. it ASSUMED the spent output pays
/// `A_seg`. `d692c07` added the two refusals (`tesr.rs:5864` on the ancestor hop, `:6134` on the leaf).
/// Like every other commit in that series it was landed against HONEST traffic only. §8: *"No test proves
/// the attacks are now refused."* This module is that proof for the payee half.
///
/// **Why this variant is different from the skims, and why it is worse in one specific way.** A skim
/// STEALS: value leaves the exit chain into an output the attacker controls, and the value laws see the
/// arithmetic come up short. A wrong payee STRANDS: every number in the bundle is correct to the satoshi,
/// so **not one value law fires**. What breaks is downstream and invisible — `verify_tier_cosigned`
/// SYNTHESISES the prevout it checks against:
///
/// ```text
/// let prevout = TxOut { value: prevout_value, script_pubkey: agg_spk.clone() };   // tesr.rs:6455
/// ```
///
/// so the tier below is verified against a `TxOut` that is *asserted*, never observed. If the real
/// payload output pays a different key, that signature commits to a prevout **that does not exist on any
/// chain**. The state below it is unbroadcastable forever, the ancestor is required to be terminal
/// (`:5830`) so the SE will never co-sign a repair, a split child has no flat backup at all
/// (`CHILD_V2_BASELINE = 0`), and the receiver has meanwhile been credited the full funding value. The
/// attacker, holding the key the extension really pays, sweeps the whole segment the moment that
/// extension confirms.
///
/// **What the attacker holds, and why that is realistic.** These tests build a real, fully co-signed
/// three-segment chain (root → ancestor → leaf) while holding both halves of every aggregate key. That is
/// not a cheat — `cosign_tier_request` (lib/src/tesr.rs:503) is handed a sighash and a prevout AMOUNT,
/// never a transaction, so the blind SE cannot see which key a tier pays any more than it can see how the
/// tier splits its value. Every attack below asserts that `verify_tier_cosigned` STILL ACCEPTS the
/// tampered tier, and that the value laws are still satisfied to the satoshi, so the refusal can only be
/// coming from the payee binding.
///
/// **Non-vacuity, three ways.** `an_honest_{one,two}_deep_chain_is_accepted` run the constructions
/// untampered and assert `Ok(())`. Each attack test then repeats that control ON ITSELF via
/// `assert_untampered_twin_is_accepted` — the same rig with the payee restored is verified to PASS, and
/// its tier is verified to carry output values identical to the refused one, so the single difference
/// between `Ok` and `Err` is a scriptPubKey. Finally `assert_refusal_names_the_payee_not_the_value`
/// fails the test both if some unrelated structural check threw the bundle out AND if a value law fired
/// instead, either of which would make the refusal worthless as evidence.
///
/// **One test here pins a HOLE rather than a fix** — `gap_an_ancestor_split_state_may_mint_value_out_of_nothing`,
/// found while building the rig. Read it before trusting the ancestor lane's value story.
///
/// **The module also hosts the [CATS/V1+V2] SEGMENT-SHAPE battery** (the banner two thirds down).
/// Different property — WHAT SHAPE a segment is, not where it pays — but the same rig: it is the only
/// construction in the tree that builds a real, fully co-signed intermediate segment, and a shape
/// attack is only evidence if the same rig minus the re-label passes.
#[cfg(test)]
mod wrong_payee_attack_tests {
    use super::*;
    use electrum_client::bitcoin::{
        consensus::{deserialize, serialize},
        key::{TapTweak, TweakedPublicKey},
        secp256k1::{KeyPair, Message, Secp256k1, SecretKey},
        sighash::{Prevouts, SighashCache, TapSighashType},
        Address, Network, ScriptBuf, Transaction, Witness,
    };

    const NET: &str = "regtest";
    /// The parent coin's on-chain funding outpoint. Nothing fetches it — `verify_child_bundle` is pure
    /// and takes `F`'s scriptPubKey as a parameter.
    const F_TXID: &str = "5555555555555555555555555555555555555555555555555555555555555555";
    const F_VOUT: u32 = 0;
    const F_VALUE: u64 = 200_000;

    /// A party holding BOTH halves of an aggregate key — what a blind co-sign is equivalent to.
    struct Holder {
        kp: KeyPair,
        /// The P2TR address for the UNTWEAKED internal key (BIP-341, no script tree).
        address: String,
        spk: ScriptBuf,
        /// The x-only the coordinator would have on record: UNTWEAKED, as `/info/statechain` serves it.
        recorded_xonly: String,
    }

    fn holder(seed: u8) -> Holder {
        let secp = Secp256k1::new();
        let sk = SecretKey::from_slice(&[seed; 32]).expect("valid secret key");
        let kp = KeyPair::from_secret_key(&secp, &sk);
        let (xonly, _parity) = kp.x_only_public_key();
        let address = Address::p2tr(&secp, xonly, None, Network::Regtest);
        Holder {
            kp,
            spk: address.script_pubkey(),
            address: address.to_string(),
            recorded_xonly: hex::encode(xonly.serialize()),
        }
    }

    impl Holder {
        /// The scriptPubKey this holder's key would produce if the BIP-341 tweak were SKIPPED — the
        /// output key set to the raw internal x-only. A near-miss payee: same 32 bytes on the wire in
        /// the coordinator's records, a different output, and nobody can spend it with the aggregate.
        fn untweaked_spk(&self) -> ScriptBuf {
            let (xonly, _) = self.kp.x_only_public_key();
            ScriptBuf::new_v1_p2tr_tweaked(TweakedPublicKey::dangerous_assume_tweaked(xonly))
        }
    }

    /// Produce exactly the witness `verify_tier_cosigned` checks: a BIP-341 key-spend Schnorr signature
    /// by the aggregate over `TxOut { value: prevout_value, script_pubkey: agg_spk }` with
    /// `TapSighashType::All`. This IS the blind co-sign, performed locally — note that `agg_spk` is a
    /// PARAMETER here exactly as it is in the verifier, which is the whole point of this module.
    fn cosign(tx: &Transaction, prevout_value: u64, agg_spk: &ScriptBuf, kp: &KeyPair) -> Transaction {
        let secp = Secp256k1::new();
        let prevout = electrum_client::bitcoin::TxOut {
            value: prevout_value,
            script_pubkey: agg_spk.clone(),
        };
        let sighash = SighashCache::new(tx)
            .taproot_key_spend_signature_hash(0, &Prevouts::All(&[prevout]), TapSighashType::All)
            .expect("taproot key-spend sighash");
        let msg = Message::from_slice(sighash.as_ref()).expect("32-byte sighash");
        let sig = secp.sign_schnorr_no_aux_rand(&msg, &kp.tap_tweak(&secp, None).to_inner());
        let mut signed = tx.clone();
        signed.input[0].witness = Witness::from_slice(&[&sig[..]]);
        signed
    }

    fn parse(tx_hex: &str) -> Transaction {
        deserialize(&hex::decode(tx_hex).expect("hex")).expect("transaction")
    }

    fn as_hex(tx: &Transaction) -> String {
        hex::encode(serialize(tx))
    }

    /// A bundle tier describing `tx`, with `out_value` read from the transaction — i.e. declared
    /// HONESTLY. Every attack below keeps every declared field honest; the lie is a scriptPubKey.
    fn tier(tx: &Transaction, csv: Option<u16>, payload_vout: u32) -> TesrTier {
        TesrTier {
            txid: tx.txid().to_string(),
            signed_tx: as_hex(tx),
            out_value: tx.output[payload_vout as usize].value,
            csv,
            payload_vout,
        }
    }

    /// Which extension's payload output is redirected, and to whom.
    #[derive(Clone, Copy, PartialEq, Debug)]
    enum Payee {
        /// No tampering at all — the non-vacuity control.
        Honest,
        /// **The assigned attack.** The ANCESTOR segment's extension pays a key the attacker holds
        /// instead of the segment's own aggregate. Values are untouched, so every conservation law is
        /// satisfied exactly; the segment's split state below it is then co-signed against a prevout
        /// that will never exist.
        AncestorExtensionToAttacker,
        /// The same attack with a payee that is not a stranger but the LEAF CHILD's own aggregate — a
        /// key that genuinely appears in this very structure and is genuinely SE-registered. A verifier
        /// that only asked "is this some aggregate we know?" would wave it through.
        AncestorExtensionToTheChildsKey,
        /// The leaf hop's copy of the same attack: `child_extension` pays the attacker rather than
        /// `A_child`. This one is reachable at depth 1, on the shipped shallow shape.
        LeafExtensionToAttacker,
        /// The near-miss: `child_extension` pays a P2TR output built from `A_child`'s x-only with the
        /// BIP-341 tweak SKIPPED. The 32 bytes match the coordinator's record; the output does not.
        LeafExtensionToUntweakedAggregate,
    }

    /// **[CATS/V1] The SHAPE of the intermediate segment**, which is the whole subject of the
    /// `sender_declared_segment_shape_tests` below.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Ancestry {
        /// Depth 1 — `SP.out[0]` funds the leaf directly.
        None,
        /// The shipped shape: `X_a` (extension, `E0`) then `CSP` (spine split state, `0`).
        TwoTier,
        /// The CATS spine tip: ONE tier — the next batch's `SP2` at `SPINE_CSV`, re-anchored on the
        /// segment's own funding outpoint — with the retained cap `C` disclosed as superseded.
        Spine,
    }

    struct Rig {
        /// The root coin's aggregate — owner of `T`, `X`, `SP`.
        parent: Holder,
        /// The intermediate child segment's aggregate — owner of `X_a`, `CSP`.
        ancestor: Holder,
        /// The leaf child's aggregate — owner of `X_c`, `S_c`.
        child: Holder,
        /// Where the leaf state pays (Model A).
        receiver: Holder,
        /// The key a redirected payload output really pays. The attacker holds it and nobody else does.
        attacker: Holder,
        /// The root coin's exit address.
        sender: Holder,
        params: mercurylib::tesr::TesrParams,
        rate: f64,
    }

    /// Everything `verify_child_bundle` needs alongside the bundle: the facts a receiver reads off the
    /// chain and the coordinator, none of which any attack here touches.
    struct Facts {
        f_spk_hex: String,
        parent_num_sigs: u32,
        parent_flat_backups: u32,
        parent_xonly: String,
        child_num_sigs: u32,
        child_flat_backups: u32,
        child_xonly: String,
        ancestors: Vec<AncestorFacts>,
        receiver_address: String,
    }

    fn rig() -> Rig {
        let params = mercurylib::tesr::TesrParams::regtest();
        Rig {
            parent: holder(0x51),
            ancestor: holder(0x52),
            child: holder(0x53),
            receiver: holder(0x54),
            attacker: holder(0x55),
            sender: holder(0x56),
            rate: params.committed_fee_rate,
            params,
        }
    }

    /// What the whole rig looks like when nothing is tampered with, at depth 2:
    ///
    /// ```text
    ///   F (on-chain, 200 000)
    ///     └─ T  ─ pays A_parent   199 510
    ///         └─ X  ─ pays A_parent   199 020
    ///             └─ SP (spine) ─ out[0] pays A_ancestor   198 530      <- the ancestor's slot
    ///                 └─ X_a ─ pays A_ancestor   198 040                <- ATTACK SITE 1
    ///                     └─ CSP (spine) ─ out[0] pays A_child  197 550 <- the leaf's slot
    ///                         └─ X_c ─ pays A_child   197 060           <- ATTACK SITE 2
    ///                             └─ S_c ─ pays the receiver   196 570
    /// ```
    ///
    /// Each arrow is one 490-sat rung (`committed_fee(2.0) + P2A_VALUE` = 250 + 240) at the regtest
    /// committed rate. Depth 1 omits the ancestor segment: `SP.out[0]` funds `X_c` directly.
    impl Rig {
        fn build(&self, with_ancestor: bool, tamper: Payee) -> (ChildTesrBundle, Facts) {
            self.build_shaped(
                if with_ancestor { Ancestry::TwoTier } else { Ancestry::None },
                tamper,
            )
        }

        fn build_shaped(&self, ancestry: Ancestry, tamper: Payee) -> (ChildTesrBundle, Facts) {
            let p = self.params;
            let a = &self.parent;

            // ── the ROOT segment: T → X → SP, honest throughout and genuinely co-signed by A_parent ──
            let t = mercurylib::tesr::build_trigger(F_TXID, F_VOUT, F_VALUE, &a.address, NET, self.rate)
                .expect("trigger");
            let t_tx = cosign(&parse(&t.tx_hex), F_VALUE, &a.spk, &a.kp);

            let x = mercurylib::tesr::build_extension(
                &t.txid, t.out_value, &a.address, NET, p.ext_csv(0), self.rate,
            )
            .expect("extension");
            let x_tx = cosign(&parse(&x.tx_hex), t.out_value, &a.spk, &a.kp);

            // `SP` funds ONE slot. At depth 2 that slot belongs to the intermediate segment; at depth 1
            // it is the leaf child's directly.
            let slot_owner = if ancestry == Ancestry::None { &self.child } else { &self.ancestor };
            let slot = mercurylib::tesr::tier_out_total(x.out_value, 1, self.rate).expect("slot");
            let sp = mercurylib::tesr::build_split_state(
                &x.txid,
                x.out_value,
                &[(slot_owner.address.clone(), slot)],
                NET,
                SPINE_CSV,
                self.rate,
            )
            .expect("split state");
            let sp_tx = cosign(&parse(&sp.tx_hex), x.out_value, &a.spk, &a.kp);

            let parent = TesrBundle {
                version: 1,
                statechain_id: "parent-sid".into(),
                network: NET.into(),
                fee_rate: self.rate,
                agg_address: a.address.clone(),
                owner_exit_address: self.sender.address.clone(),
                f_txid: F_TXID.into(),
                f_vout: F_VOUT,
                f_value: F_VALUE,
                trigger: tier(&t_tx, None, t.payload_vout),
                levels: vec![TesrLevel {
                    extension: tier(&x_tx, Some(p.ext_csv(0)), x.payload_vout),
                    state: tier(&sp_tx, Some(SPINE_CSV), sp.payload_vout),
                }],
                m: 0,
                superseded_states: vec![],
                superseded_extensions: vec![],
                params: p,
                rgb: None,
            };

            // ── the optional ANCESTOR segment: X_a over SP.out[0], then its own spine split state ────
            let mut ancestors: Vec<ChildSegment> = vec![];
            let mut ancestor_facts: Vec<AncestorFacts> = vec![];
            let (leaf_funding_txid, leaf_slot) = if ancestry == Ancestry::Spine {
                // ── [CATS] the ONE-TIER SPINE segment ────────────────────────────────────────────
                //
                // The sender's tip at this level. Its retained cap `C` sits over `SP.out[0]` at `D0`;
                // the next batch's split state `SP2` spends the SAME outpoint at `SPINE_CSV = 0` and
                // out-races it, so `C` is disclosed as superseded and the segment's LIVE tier is
                // `SP2` — one tier, re-anchored on the segment's own funding outpoint, no extension.
                let g = &self.ancestor;
                let cap = mercurylib::tesr::build_state_from(
                    &sp.txid, sp.payload_vout, slot, &g.address, NET, p.state_csv(0), self.rate,
                )
                .expect("spine cap");
                let cap_tx = cosign(&parse(&cap.tx_hex), slot, &g.spk, &g.kp);

                let slot2 = mercurylib::tesr::tier_out_total(slot, 1, self.rate).expect("leaf slot");
                let sp2 = mercurylib::tesr::build_split_state_from(
                    &sp.txid,
                    sp.payload_vout,
                    slot,
                    &[(self.child.address.clone(), slot2)],
                    NET,
                    SPINE_CSV,
                    self.rate,
                )
                .expect("the next batch's spine split state");
                let sp2_tx = cosign(&parse(&sp2.tx_hex), slot, &g.spk, &g.kp);

                ancestors.push(ChildSegment {
                    statechain_id: "ancestor-sid".into(),
                    funding_vout: 0,
                    extension: None,
                    state: tier(&sp2_tx, Some(SPINE_CSV), sp2.payload_vout),
                    superseded_states: vec![tier(&cap_tx, Some(p.state_csv(0)), cap.payload_vout)],
                    superseded_extensions: vec![],
                });
                ancestor_facts.push(AncestorFacts {
                    // [V2] ONE tier + ONE superseded. Under the literal `2` this segment would have
                    // been expected to account for THREE co-signs.
                    num_sigs: CHILD_V2_BASELINE + 1 + 1,
                    aggregate_pubkey: Some(g.recorded_xonly.clone()),
                    terminal: true,
                });
                (sp2_tx.txid().to_string(), slot2)
            } else if ancestry == Ancestry::TwoTier {
                let g = &self.ancestor;
                let xa = mercurylib::tesr::build_extension(
                    &sp.txid, slot, &g.address, NET, p.ext_csv(0), self.rate,
                )
                .expect("ancestor extension");
                let mut xa_tx = parse(&xa.tx_hex);
                // ── ATTACK SITE 1 ────────────────────────────────────────────────────────────────
                // One field. The value stays exactly `slot − one rung`, the P2A anchor is untouched,
                // the output count is unchanged, `out_value` below is still read off this very
                // transaction. Only the 32 bytes of the key change.
                match tamper {
                    Payee::AncestorExtensionToAttacker => {
                        xa_tx.output[xa.payload_vout as usize].script_pubkey = self.attacker.spk.clone();
                    }
                    Payee::AncestorExtensionToTheChildsKey => {
                        xa_tx.output[xa.payload_vout as usize].script_pubkey = self.child.spk.clone();
                    }
                    _ => {}
                }
                // The blind SE co-signs the redirected output exactly as it co-signs an honest one: it
                // is handed `(sighash, prevout amount)` and never sees an output at all.
                let xa_tx = cosign(&xa_tx, slot, &g.spk, &g.kp);

                let slot2 = mercurylib::tesr::tier_out_total(xa.out_value, 1, self.rate)
                    .expect("leaf slot");
                let csp = mercurylib::tesr::build_split_state(
                    &xa_tx.txid().to_string(),
                    xa.out_value,
                    &[(self.child.address.clone(), slot2)],
                    NET,
                    SPINE_CSV,
                    self.rate,
                )
                .expect("ancestor split state");
                // THE STRAND, made concrete: this co-sign is taken against `A_ancestor`'s spk, because
                // that is what the SE is told the prevout is. When `X_a` really pays someone else, this
                // signature is valid for a `TxOut` that exists nowhere.
                let csp_tx = cosign(&parse(&csp.tx_hex), xa.out_value, &g.spk, &g.kp);

                ancestors.push(ChildSegment {
                    statechain_id: "ancestor-sid".into(),
                    funding_vout: 0,
                    extension: Some(tier(&xa_tx, Some(p.ext_csv(0)), xa.payload_vout)),
                    state: tier(&csp_tx, Some(SPINE_CSV), csp.payload_vout),
                    superseded_states: vec![],
                    superseded_extensions: vec![],
                });
                ancestor_facts.push(AncestorFacts {
                    // A derived segment has no flat backup: just its two tiers, nothing superseded.
                    num_sigs: CHILD_V2_BASELINE + 2,
                    aggregate_pubkey: Some(g.recorded_xonly.clone()),
                    terminal: true,
                });
                (csp_tx.txid().to_string(), slot2)
            } else {
                (sp_tx.txid().to_string(), slot)
            };

            // ── the LEAF: X_c over the funding slot, then S_c paying the receiver ────────────────────
            let c = &self.child;
            let xc = mercurylib::tesr::build_extension(
                &leaf_funding_txid, leaf_slot, &c.address, NET, p.ext_csv(0), self.rate,
            )
            .expect("child extension");
            let mut xc_tx = parse(&xc.tx_hex);
            // ── ATTACK SITE 2 ───────────────────────────────────────────────────────────────────
            match tamper {
                Payee::LeafExtensionToAttacker => {
                    xc_tx.output[xc.payload_vout as usize].script_pubkey = self.attacker.spk.clone();
                }
                Payee::LeafExtensionToUntweakedAggregate => {
                    xc_tx.output[xc.payload_vout as usize].script_pubkey = c.untweaked_spk();
                }
                _ => {}
            }
            let xc_tx = cosign(&xc_tx, leaf_slot, &c.spk, &c.kp);

            let sc = mercurylib::tesr::build_state(
                &xc_tx.txid().to_string(),
                xc.out_value,
                &self.receiver.address,
                NET,
                p.state_csv(0),
                self.rate,
            )
            .expect("child state");
            let sc_tx = cosign(&parse(&sc.tx_hex), xc.out_value, &c.spk, &c.kp);

            let cb = ChildTesrBundle {
                parent,
                parent_statechain_id: "parent-sid".into(),
                sp_vout: 0,
                child_statechain_id: "child-sid".into(),
                child_owner_exit_address: self.receiver.address.clone(),
                child_extension: tier(&xc_tx, Some(p.ext_csv(0)), xc.payload_vout),
                child_state: tier(&sc_tx, Some(p.state_csv(0)), sc.payload_vout),
                child_superseded_states: vec![],
                child_superseded_extensions: vec![],
                ancestors,
                rgb: None,
                parent_flat_backups: vec![],
            };
            let facts = Facts {
                f_spk_hex: hex::encode(self.parent.spk.as_bytes()),
                // The root's census: one deposit backup + T + X + SP, nothing superseded.
                parent_num_sigs: 1 + 3,
                parent_flat_backups: 1,
                parent_xonly: self.parent.recorded_xonly.clone(),
                child_num_sigs: 2,
                child_flat_backups: 0,
                child_xonly: self.child.recorded_xonly.clone(),
                ancestors: ancestor_facts,
                receiver_address: self.receiver.address.clone(),
            };
            (cb, facts)
        }
    }

    fn verify(cb: &ChildTesrBundle, f: &Facts) -> Result<()> {
        verify_child_bundle(
            cb,
            &f.f_spk_hex,
            // The chain fact. Every segment here conserves value exactly — the tamper is a PAYEE key,
            // never an amount — so the trigger anchor is satisfied and the payee binding stays the
            // only thing that can refuse.
            F_VALUE,
            f.parent_num_sigs,
            f.parent_flat_backups,
            Some(&f.parent_xonly),
            true, // the root segment is terminal, as the protocol requires
            f.child_num_sigs,
            f.child_flat_backups,
            Some(&f.child_xonly),
            &f.ancestors,
            &f.receiver_address,
        )
    }

    /// The refusal must name WHERE THE TIER PAYS. Two ways this test could be worthless, both checked:
    /// the bundle was thrown out by some unrelated structural check, or a VALUE law fired — which would
    /// mean the construction accidentally failed to conserve and the payee binding was never reached.
    fn assert_refusal_names_the_payee_not_the_value(msg: &str) {
        assert!(
            msg.contains("does not pay"),
            "the refusal must name the payee, got: {msg}"
        );
        for value_law in [
            "payload outputs carry",
            "forwards only",
            "expected exactly",
            "skimmed",
            "cannot carry a tier",
            "out_value",
        ] {
            assert!(
                !msg.contains(value_law),
                "a VALUE law fired ({value_law:?}) — the construction did not conserve and this test \
                 proves nothing about the payee binding: {msg}"
            );
        }
        for unrelated in [
            "not co-signed",
            "num_sigs",
            "CSV",
            "does not spend",
            "colour",
            "decoy",
            "terminal",
            "fail-closed",
            "hex",
            "not a transaction",
        ] {
            assert!(
                !msg.contains(unrelated),
                "the bundle was refused for an UNRELATED reason ({unrelated:?}) — this test would be \
                 worthless: {msg}"
            );
        }
    }

    /// The value laws, re-computed here from the tier's own funding, so an attack test can state
    /// positively that they are SATISFIED rather than merely hoping they are. Returns
    /// `(Σ payload outputs, what the law expects)`.
    fn payload_sum_and_expectation(tx: &Transaction, funding: u64, rate: f64) -> (u64, u64) {
        let got: u64 = tx
            .output
            .iter()
            .filter(|o| {
                o.script_pubkey.as_bytes() != mercurylib::tesr::P2A_SCRIPT_BYTES
                    && !o.script_pubkey.is_op_return()
            })
            .map(|o| o.value)
            .sum();
        (got, mercurylib::tesr::tier_out_value(funding, rate).expect("rung"))
    }

    /// **The paired non-vacuity control.** Rebuild the SAME rig with no tampering, assert the verifier
    /// ACCEPTS it, and assert that the tier which was just refused carries output values identical to
    /// its accepted twin — so the only thing that changed between `Ok` and `Err` is a scriptPubKey.
    /// This is what makes each attack test self-contained: a refusal is only evidence if the identical
    /// bundle minus the tampering passes.
    fn assert_untampered_twin_is_accepted(
        rig: &Rig,
        with_ancestor: bool,
        pick: impl Fn(&ChildTesrBundle) -> Transaction,
        tampered: &Transaction,
    ) {
        let (honest, facts) = rig.build(with_ancestor, Payee::Honest);
        verify(&honest, &facts).expect(
            "the SAME construction without the tampering must PASS — otherwise the refusal proves \
             nothing about the payee binding",
        );
        let twin = pick(&honest);
        assert_eq!(
            twin.output.iter().map(|o| o.value).collect::<Vec<_>>(),
            tampered.output.iter().map(|o| o.value).collect::<Vec<_>>(),
            "the accepted twin and the refused tier carry IDENTICAL output values"
        );
        assert_ne!(
            twin.output[0].script_pubkey, tampered.output[0].script_pubkey,
            "…and the one difference between them is the payee"
        );
    }

    // ── THE NON-VACUITY CONTROLS ───────────────────────────────────────────────────────────────────

    /// **The control the depth-2 attacks depend on.** The identical three-segment construction,
    /// untampered, must be ACCEPTED. Without this a refusal proves nothing: an ancestor segment is the
    /// most intricate shape this verifier takes and could be malformed in a dozen unrelated ways.
    #[test]
    fn an_honest_two_deep_chain_is_accepted() {
        let rig = rig();
        let (cb, facts) = rig.build(true, Payee::Honest);

        // The arithmetic every attack below leaves UNTOUCHED, stated once so a schedule change surfaces
        // here rather than as a mystery three tests down. A plain rung at 2 sat/vB is 490 sat.
        let sp: Transaction = parse(&cb.parent.current().state.signed_tx);
        assert_eq!(sp.output[0].value, 198_530, "the ancestor segment's slot on SP");
        assert_eq!(sp.output[0].script_pubkey, rig.ancestor.spk, "…and it pays A_ancestor");
        let xa: Transaction = parse(&cb.ancestors[0].extension.as_ref().unwrap().signed_tx);
        assert_eq!(xa.output[0].value, 198_530 - 490, "the ancestor extension forwards one rung less");
        assert_eq!(xa.output[0].script_pubkey, rig.ancestor.spk, "…to its OWN aggregate");
        let csp: Transaction = parse(&cb.ancestors[0].state.signed_tx);
        assert_eq!(csp.output[0].value, 198_530 - 2 * 490, "the leaf's slot on CSP");
        assert_eq!(csp.output[0].script_pubkey, rig.child.spk, "…paying A_child");
        assert_eq!(cb.child_extension.out_value, 198_530 - 3 * 490);
        assert_eq!(cb.child_state.out_value, 198_530 - 4 * 490);

        verify(&cb, &facts).expect("an honest, fully co-signed two-deep child bundle must be ACCEPTED");
    }

    /// The depth-1 control, for the leaf attacks — the shipped shallow shape, with no ancestor segment
    /// at all, so a leaf refusal cannot be blamed on the ancestor scaffolding.
    #[test]
    fn an_honest_one_deep_chain_is_accepted() {
        let rig = rig();
        let (cb, facts) = rig.build(false, Payee::Honest);
        assert!(cb.ancestors.is_empty(), "depth 1: SP funds the leaf directly");
        assert_eq!(cb.child_extension.out_value, 198_530 - 490);
        assert_eq!(cb.child_state.out_value, 198_530 - 2 * 490);
        verify(&cb, &facts).expect("an honest depth-1 child bundle must be ACCEPTED");
    }

    // ── THE ATTACKS ────────────────────────────────────────────────────────────────────────────────

    /// **THE ASSIGNED ATTACK.** The ancestor segment's extension spends its 198 530-sat funding output,
    /// forwards exactly 198 040 — the correct amount, to the satoshi — and pays it to a key only the
    /// attacker holds. Nothing is skimmed. Nothing is minted. Every declared field agrees with its
    /// transaction.
    ///
    /// What it destroys is the segment below: `CSP` was co-signed against
    /// `TxOut { value: 198_040, script_pubkey: A_ancestor }`, an output that this chain never creates.
    /// The test asserts that directly — the co-sign verifies against the SYNTHESISED prevout and fails
    /// against the REAL one — which is the stranding, in two lines, rather than as an argument.
    #[test]
    fn an_ancestor_extension_paying_a_foreign_key_is_refused() {
        let rig = rig();
        let (cb, facts) = rig.build(true, Payee::AncestorExtensionToAttacker);

        let sp: Transaction = parse(&cb.parent.current().state.signed_tx);
        let funding = sp.output[0].value;
        let xa: Transaction = parse(&cb.ancestors[0].extension.as_ref().unwrap().signed_tx);

        // 1. THE VALUE LAWS ARE SATISFIED — this is not a skim wearing a different hat.
        let (got, expect) = payload_sum_and_expectation(&xa, funding, rig.rate);
        assert_eq!(got, expect, "Σ payload outputs is EXACTLY the law's expected total");
        assert_eq!(xa.output.len(), 2, "payload + P2A anchor — no extra output to hide value in");
        assert_eq!(
            cb.ancestors[0].extension.as_ref().unwrap().out_value, xa.output[0].value,
            "the declared value is read off the transaction — every declared field is honest"
        );

        // 2. THE LIE, isolated: 32 bytes.
        assert_eq!(xa.output[0].script_pubkey, rig.attacker.spk, "the payload output pays the ATTACKER");
        assert_ne!(xa.output[0].script_pubkey, rig.ancestor.spk, "…not the segment's own aggregate");

        // 3. THE SE REALLY CO-SIGNED IT. A blind co-sign carries no information about the payee.
        verify_tier_cosigned(&xa, funding, &rig.ancestor.spk)
            .expect("the blind SE co-signs the redirected tier — this is not a forgery");

        // 4. THE STRAND, demonstrated. `CSP`'s signature verifies against the prevout the verifier
        //    SYNTHESISES, and against nothing that will ever be on a chain.
        let csp: Transaction = parse(&cb.ancestors[0].state.signed_tx);
        verify_tier_cosigned(&csp, xa.output[0].value, &rig.ancestor.spk)
            .expect("valid against the ASSUMED prevout — which is why the co-sign check cannot see this");
        verify_tier_cosigned(&csp, xa.output[0].value, &rig.attacker.spk).expect_err(
            "…and INVALID against the output the chain would really carry: CSP is unbroadcastable \
             forever, while the attacker sweeps the segment the moment X_a confirms",
        );

        let e = verify(&cb, &facts).expect_err("an ancestor extension paying a foreign key must be REFUSED");
        let msg = e.to_string();
        assert!(
            msg.contains("ancestor 0") && msg.contains("segment's aggregate"),
            "the refusal must name the hop and the key it should have paid, got: {msg}"
        );
        assert!(
            msg.contains("prevout that does not exist"),
            "…and the consequence, got: {msg}"
        );
        assert_refusal_names_the_payee_not_the_value(&msg);

        // 5. NON-VACUITY, paired inside this test rather than only next door: the identical
        //    construction with the payee restored is ACCEPTED, and the two extensions carry
        //    byte-identical VALUES. The refusal cannot be about anything but the key.
        assert_untampered_twin_is_accepted(&rig, true, |b| {
            parse(&b.ancestors[0].extension.as_ref().unwrap().signed_tx)
        }, &xa);
    }

    /// The same attack with a payee chosen to defeat a lazier check: the ancestor extension pays the
    /// **leaf child's** aggregate — a key that is genuinely SE-registered, genuinely appears in this
    /// bundle, and which `verify_child_bundle` will itself bind to `CSP.out[0]` a few lines later. Only
    /// a check that compares against THIS SEGMENT's own aggregate refuses it.
    ///
    /// Note what this shape would do if it were accepted: `CSP` spends `X_a.out[0]` under `A_ancestor`'s
    /// key, which no longer owns it, so the leaf's entire funding chain is dead — while the child's own
    /// two tiers verify perfectly and the receiver is credited 197 550 sat.
    #[test]
    fn an_ancestor_extension_paying_a_different_real_aggregate_is_refused() {
        let rig = rig();
        let (cb, facts) = rig.build(true, Payee::AncestorExtensionToTheChildsKey);

        let sp: Transaction = parse(&cb.parent.current().state.signed_tx);
        let xa: Transaction = parse(&cb.ancestors[0].extension.as_ref().unwrap().signed_tx);
        let (got, expect) = payload_sum_and_expectation(&xa, sp.output[0].value, rig.rate);
        assert_eq!(got, expect, "value conservation is exact — only the payee is wrong");
        assert_eq!(
            xa.output[0].script_pubkey, rig.child.spk,
            "the payee is the LEAF's aggregate: a real, server-registered key from this same bundle"
        );
        // And it is the key the verifier is about to accept as `A_child` for the leaf, one hop down —
        // so "is this a key we recognise?" is not a check that could have caught this.
        let csp: Transaction = parse(&cb.ancestors[0].state.signed_tx);
        assert_eq!(csp.output[0].script_pubkey, rig.child.spk);
        verify_tier_cosigned(&xa, sp.output[0].value, &rig.ancestor.spk)
            .expect("genuinely co-signed by the segment's aggregate");

        let e = verify(&cb, &facts).expect_err("paying the WRONG real aggregate must still be REFUSED");
        let msg = e.to_string();
        assert!(msg.contains("ancestor 0"), "got: {msg}");
        assert_refusal_names_the_payee_not_the_value(&msg);
        assert_untampered_twin_is_accepted(&rig, true, |b| {
            parse(&b.ancestors[0].extension.as_ref().unwrap().signed_tx)
        }, &xa);
    }

    /// The leaf hop's copy, at depth 1 — the shipped shallow shape. `child_extension` forwards exactly
    /// one rung less than its funding, to the attacker's key. `child_state` then pays the receiver the
    /// right amount and declares it honestly, so Model A and the value-gate binding both pass; the coin
    /// is simply unreachable, and a split child has no flat backup to fall back on.
    #[test]
    fn a_leaf_child_extension_paying_a_foreign_key_is_refused() {
        let rig = rig();
        let (cb, facts) = rig.build(false, Payee::LeafExtensionToAttacker);

        let sp: Transaction = parse(&cb.parent.current().state.signed_tx);
        let funding = sp.output[0].value;
        let xc: Transaction = parse(&cb.child_extension.signed_tx);
        let (got, expect) = payload_sum_and_expectation(&xc, funding, rig.rate);
        assert_eq!(got, expect, "the leaf extension conserves exactly");
        assert_eq!(
            cb.child_extension.out_value, xc.output[0].value,
            "and its declared out_value is the signed one"
        );
        assert_eq!(xc.output[0].script_pubkey, rig.attacker.spk, "but it pays the ATTACKER");

        // Model A still holds one tier further down — the state really does pay the receiver.
        let sc: Transaction = parse(&cb.child_state.signed_tx);
        assert_eq!(sc.output[0].script_pubkey, rig.receiver.spk);
        assert_eq!(sc.output[0].value, cb.child_state.out_value);
        verify_tier_cosigned(&xc, funding, &rig.child.spk).expect("genuinely co-signed by A_child");
        verify_tier_cosigned(&sc, xc.output[0].value, &rig.child.spk)
            .expect("and so is the state, against the prevout the verifier assumes");
        verify_tier_cosigned(&sc, xc.output[0].value, &rig.attacker.spk)
            .expect_err("but not against the one the chain would carry — the child is stranded");

        let e = verify(&cb, &facts).expect_err("a leaf extension paying a foreign key must be REFUSED");
        let msg = e.to_string();
        assert!(
            msg.contains("child extension") && msg.contains("A_child"),
            "the refusal must name the leaf hop and A_child, got: {msg}"
        );
        assert!(msg.contains("prevout that does not exist"), "…and the consequence, got: {msg}");
        assert_refusal_names_the_payee_not_the_value(&msg);
        assert_untampered_twin_is_accepted(&rig, false, |b| parse(&b.child_extension.signed_tx), &xc);
    }

    /// The near-miss, and the reason the check compares whole scriptPubKeys rather than key material.
    /// `child_extension` pays a P2TR output whose OUTPUT KEY is `A_child`'s x-only with the BIP-341
    /// tweak skipped. The 32 bytes match the coordinator's `/info/statechain` record exactly, so a
    /// verifier that compared the recorded aggregate to `taproot_key_hex(out.script_pubkey)` would see
    /// what it expected — but the aggregate cannot produce a key-spend signature for that output, so the
    /// tier below is signed against a prevout that does not exist, exactly as in the foreign-key case.
    #[test]
    fn a_leaf_child_extension_paying_the_untweaked_aggregate_is_refused() {
        let rig = rig();
        let (cb, facts) = rig.build(false, Payee::LeafExtensionToUntweakedAggregate);

        let xc: Transaction = parse(&cb.child_extension.signed_tx);
        let paid = &xc.output[0].script_pubkey;
        assert_ne!(*paid, rig.child.spk, "the output key is NOT the tweaked aggregate");
        assert_eq!(
            hex::encode(&paid.as_bytes()[2..34]),
            rig.child.recorded_xonly,
            "…yet the 32 bytes on the wire are exactly the aggregate the coordinator has on record"
        );
        let sp: Transaction = parse(&cb.parent.current().state.signed_tx);
        let (got, expect) = payload_sum_and_expectation(&xc, sp.output[0].value, rig.rate);
        assert_eq!(got, expect, "value conservation is exact");
        verify_tier_cosigned(&xc, sp.output[0].value, &rig.child.spk)
            .expect("genuinely co-signed — the tweak is missing from the PAYEE, not from the signature");

        let e = verify(&cb, &facts).expect_err("an untweaked payee must be REFUSED");
        let msg = e.to_string();
        assert!(msg.contains("child extension") && msg.contains("A_child"), "got: {msg}");
        assert_refusal_names_the_payee_not_the_value(&msg);
        assert_untampered_twin_is_accepted(&rig, false, |b| parse(&b.child_extension.signed_tx), &xc);
    }

    /// **A HOLE THIS MODULE FOUND WHILE BUILDING THE ABOVE, PINNED AS A TRIPWIRE.**
    ///
    /// The ancestor loop binds the extension hop's value (`tesr.rs:5870-5895`, `d692c07`) and the leaf
    /// binds both of its own (`4e165e6`). **The ancestor segment's STATE — the spine `CSP` whose outputs
    /// fund the level below — is bound by no value law at all.** Between the extension's Σ check and
    /// `cur_tx = st_tx` (`:5986`) the state is parsed, checked to spend the extension's payload output,
    /// co-signed against `ext0.value`, and CSV-bounded; its OUTPUT VALUES are never compared to anything.
    ///
    /// That matters because the very next thing the function does is read the leaf's funding out of it —
    /// `sp_out = cur_tx.output[cb.sp_vout]` — and the receiver books that number. So an intermediate
    /// segment can MINT: `CSP` declares `out[0]` far larger than `X_a` holds, the leaf's two hops then
    /// conserve perfectly from the inflated figure, every census balances, and `verify_child_bundle`
    /// returns `Ok(())` over a chain that is consensus-invalid at `CSP` (outputs exceed input) and can
    /// therefore never confirm. This is finding V2(a) of the sweep, closed on the extension hop and left
    /// open on the state hop.
    ///
    /// It is NOT the attack this module was written for, so it is recorded as an executable tripwire
    /// rather than argued in prose: the test asserts the bundle is currently ACCEPTED, and FAILS LOUDLY
    /// the day someone closes the hole — at which point the fix is to turn this into a refusal assertion
    /// alongside the others.
    #[test]
    fn gap_an_ancestor_split_state_may_mint_value_out_of_nothing() {
        let rig = rig();
        let (mut cb, mut facts) = rig.build(true, Payee::Honest);
        let g = &rig.ancestor;

        let xa: Transaction = parse(&cb.ancestors[0].extension.as_ref().unwrap().signed_tx);
        let honest_slot = parse(&cb.ancestors[0].state.signed_tx).output[0].value;
        // Mint: hand the leaf a slot ten times what the segment above it actually holds. `X_a` carries
        // 198 040 sat; `CSP` will now claim to pay out 1 975 500.
        let minted = honest_slot * 10;
        let csp = mercurylib::tesr::build_split_state(
            &xa.txid().to_string(),
            // `build_split_state` enforces conservation itself, so it is told a funding figure that
            // matches the mint — the transaction it returns is what a hostile builder would emit
            // directly, and nothing on the receive side ever compares it to `X_a`.
            minted + (xa.output[0].value - honest_slot),
            &[(rig.child.address.clone(), minted)],
            NET,
            SPINE_CSV,
            rig.rate,
        )
        .expect("a minting split state");
        let csp_tx = cosign(&parse(&csp.tx_hex), xa.output[0].value, &g.spk, &g.kp);
        cb.ancestors[0].state = tier(&csp_tx, Some(SPINE_CSV), csp.payload_vout);

        // Rebuild the leaf honestly over the inflated slot — every law it is subject to is satisfied.
        let c = &rig.child;
        let xc = mercurylib::tesr::build_extension(
            &csp_tx.txid().to_string(), minted, &c.address, NET, rig.params.ext_csv(0), rig.rate,
        )
        .expect("child extension");
        let xc_tx = cosign(&parse(&xc.tx_hex), minted, &c.spk, &c.kp);
        let sc = mercurylib::tesr::build_state(
            &xc_tx.txid().to_string(), xc.out_value, &rig.receiver.address, NET,
            rig.params.state_csv(0), rig.rate,
        )
        .expect("child state");
        let sc_tx = cosign(&parse(&sc.tx_hex), xc.out_value, &c.spk, &c.kp);
        cb.child_extension = tier(&xc_tx, Some(rig.params.ext_csv(0)), xc.payload_vout);
        cb.child_state = tier(&sc_tx, Some(rig.params.state_csv(0)), sc.payload_vout);
        facts.ancestors[0].num_sigs = CHILD_V2_BASELINE + 2;

        // The mint is real: CSP's outputs exceed what it spends, so it can never confirm.
        let spends = xa.output[0].value;
        let pays: u64 = csp_tx.output.iter().map(|o| o.value).sum();
        assert!(
            pays > spends,
            "the tripwire's premise: CSP pays {pays} while spending {spends} — consensus-invalid"
        );

        match verify(&cb, &facts) {
            Ok(()) => { /* GAP as described: V2(a) survives on the ancestor STATE hop. */ }
            Err(e) => panic!(
                "THE ANCESTOR-STATE VALUE GAP HAS BEEN CLOSED — this tripwire has done its job. \
                 `verify_child_bundle` now refuses a minting ancestor split state ({e}); replace this \
                 test with an assertion that the refusal names the value law, and strike the gap from \
                 the module doc comment."
            ),
        }
    }

    // ═══════════════════════════════════════════════════════════════════════════════════════════════
    // [CATS/V1+V2] SENDER-DECLARED SEGMENT SHAPE — the attack the census does NOT close
    // ═══════════════════════════════════════════════════════════════════════════════════════════════
    //
    // `ChildSegment::extension` is an `Option`, so for the first time a segment's SHAPE is a field a
    // sender fills in. `PARTIAL-PAYMENT-ECONOMICS.md` §4.5 originally licensed that with "it still
    // fails closed via the exact-equality census (a dropped tier leaves `expected` one short of
    // `num_sigs`)". **That sentence is false**, and the tests below are the executable proof:
    //
    //   A dropped tier is not lost — it is RE-DECLARED as superseded, `verify_superseded_segment`
    //   counts it (it returns `sups.len()`), and `expected` moves by exactly the same 1 the other
    //   way. `CHILD_V2_BASELINE + 1 + 1` and `CHILD_V2_BASELINE + 2 + 0` are the SAME NUMBER.
    //
    // Every test here therefore asserts the census arithmetic BALANCES before asserting the refusal,
    // so it is on the record that the refusal comes from the structural derivation and not from the
    // count. The three layers, one test each:
    //
    //   (1) the prevout re-anchor — the lone tier must spend the segment's own funding outpoint;
    //   (2) the `[0,0]` CSV pin  — which is why the lone tier's bound must never be widened;
    //   (3) the dead knob        — `superseded_extensions` has no honest writer on a spine segment.
    //
    // Plus the V2 census term itself, and the exit-chain/label pairing that has to move with V1.

    /// **THE NON-VACUITY CONTROL for everything below.** An honest one-tier spine ancestor — the
    /// shape CATS actually emits — must be ACCEPTED. Without this every refusal below could be the
    /// verifier simply not understanding the shape at all.
    #[test]
    fn an_honest_one_tier_spine_ancestor_is_accepted() {
        let rig = rig();
        let (cb, facts) = rig.build_shaped(Ancestry::Spine, Payee::Honest);

        let seg = &cb.ancestors[0];
        assert!(seg.extension.is_none(), "the premise: this segment has ONE tier");
        assert_eq!(seg.superseded_states.len(), 1, "…and its retained cap is disclosed");
        // The lone tier really does re-anchor on the funding outpoint — the fact the derivation reads.
        let sp: Transaction = parse(&cb.parent.current().state.signed_tx);
        let lone: Transaction = parse(&seg.state.signed_tx);
        assert_eq!(lone.input[0].previous_output.txid, sp.txid());
        assert_eq!(lone.input[0].previous_output.vout, seg.funding_vout);
        assert_eq!(lone.input[0].sequence.0 as u16, SPINE_CSV, "…at the spine CSV");
        // …and the cap it out-races spends the very same outpoint, one full D0 later.
        let cap: Transaction = parse(&seg.superseded_states[0].signed_tx);
        assert_eq!(cap.input[0].previous_output, lone.input[0].previous_output);
        assert!(cap.input[0].sequence.0 as u16 > SPINE_CSV, "the cap must LOSE the race");

        verify(&cb, &facts).expect("an honest one-tier CATS spine ancestor must be ACCEPTED");
    }

    /// **(1) THE PREVOUT RE-ANCHOR — the single load-bearing check.**
    ///
    /// A real `[extension, state]` segment, re-labelled `extension: None` with the extension parked in
    /// `superseded_states` (which side-steps the dead knob entirely). Nothing is re-signed; the census
    /// balances to the satoshi. What refuses it is that the surviving tier spends its extension's
    /// payload output rather than the segment's funding outpoint — an input committed by the taproot
    /// SIGHASH_ALL sighash, so the sender cannot repoint it without invalidating the SE's signature.
    ///
    /// Left open, this is [P0-1] re-opened through a new door: the segment's declared exit chain loses
    /// its 720-block extension rung, and `check_exit_headroom` admits a child near the epoch boundary
    /// whose real exit cannot finish.
    ///
    /// **What happens if the re-anchor is removed, measured rather than assumed.** Deleting the check
    /// and re-running this test gives *"ancestor 0: state not co-signed by its aggregate"* — the
    /// re-labelled reading synthesises the prevout as `{fund_out.value, seg_spk}` while the state was
    /// really co-signed against `ext0.value`, one rung lower, so the signature misses. That is an
    /// ARITHMETIC coincidence, not a defence: the difference is a term the attacker chooses, and
    /// [`the_reanchor_is_the_only_barrier_once_the_prevout_amounts_are_equalised`] builds the segment
    /// that erases it.
    #[test]
    fn a_two_tier_segment_relabelled_as_a_spine_is_refused_by_the_prevout_reanchor() {
        let rig = rig();
        let (mut cb, facts) = rig.build(true, Payee::Honest);

        let real_ext = cb.ancestors[0].extension.take().expect("the two-tier rig has one");
        // Re-declared, not dropped — this is the move §4.5 missed.
        cb.ancestors[0].superseded_states.push(real_ext);

        // THE CENSUS CANNOT SEE THIS. Both readings of the same segment reach the same total, so the
        // SE's count is satisfied either way and `expected` is untouched.
        let sigs = facts.ancestors[0].num_sigs;
        assert_eq!(sigs, CHILD_V2_BASELINE + 2 + 0, "as a two-tier segment");
        assert_eq!(sigs, CHILD_V2_BASELINE + 1 + 1, "…and as a one-tier one. Identical.");

        let msg = verify(&cb, &facts)
            .expect_err("a two-tier segment declared as a spine must be REFUSED")
            .to_string();
        assert!(
            msg.contains("the lone tier does not spend the segment's funding outpoint"),
            "the refusal must name the re-anchor, not something incidental: {msg}"
        );
        assert!(
            !msg.contains("num_sigs"),
            "…and it must NOT come from the census, which balances exactly: {msg}"
        );
    }

    /// **THE RE-ANCHOR, WITH ITS ONE FALLBACK REMOVED — the same attack, arithmetic and all.**
    ///
    /// [`a_two_tier_segment_relabelled_as_a_spine_is_refused_by_the_prevout_reanchor`] records that a
    /// verifier without the re-anchor still misses, because the prevout AMOUNT the re-labelled reading
    /// synthesises (`fund_out.value`) is one rung above the one the state was co-signed against
    /// (`ext0.value`). That gap is the sender's to close: nothing applies a Σ-law to a tier that is
    /// only ever parsed as a *superseded state* (the ancestor split-state value gap is still open —
    /// see `gap_an_ancestor_split_state_may_mint_value_out_of_nothing`), so the extension can simply
    /// forward its funding value UNCHANGED. Then `ext0.value == fund_out.value`, the co-sign verifies
    /// under both readings, and the re-anchor is the only thing left standing.
    ///
    /// Verified by deleting the re-anchor and re-running: this bundle is ACCEPTED without it.
    #[test]
    fn the_reanchor_is_the_only_barrier_once_the_prevout_amounts_are_equalised() {
        let rig = rig();
        let (mut cb, mut facts) = rig.build(true, Payee::Honest);
        let g = &rig.ancestor;
        let sp: Transaction = parse(&cb.parent.current().state.signed_tx);
        let slot = sp.output[0].value;

        // A "fat" extension over the segment's funding outpoint: same payee, same output count, same
        // P2A anchor — but it forwards the WHOLE funding value instead of funding minus one rung.
        let fat = mercurylib::tesr::build_extension_from(
            &sp.txid().to_string(), 0, slot, &g.address, NET, rig.params.ext_csv(0), rig.rate,
        )
        .expect("ancestor extension");
        let mut fat_tx = parse(&fat.tx_hex);
        fat_tx.output[fat.payload_vout as usize].value = slot;
        let fat_tx = cosign(&fat_tx, slot, &g.spk, &g.kp);

        // The segment's real split state, over the fat extension's payload output — and therefore
        // co-signed against a prevout amount that is ALSO the funding outpoint's value.
        let leaf_slot = mercurylib::tesr::tier_out_total(slot, 1, rig.rate).expect("leaf slot");
        let csp = mercurylib::tesr::build_split_state_from(
            &fat_tx.txid().to_string(),
            fat.payload_vout,
            slot,
            &[(rig.child.address.clone(), leaf_slot)],
            NET,
            SPINE_CSV,
            rig.rate,
        )
        .expect("ancestor split state");
        let csp_tx = cosign(&parse(&csp.tx_hex), slot, &g.spk, &g.kp);

        // Re-hang the leaf so the rest of the bundle is coherent — the refusal must come from the
        // segment, not from a stale child.
        let c = &rig.child;
        let xc = mercurylib::tesr::build_extension(
            &csp_tx.txid().to_string(), leaf_slot, &c.address, NET, rig.params.ext_csv(0), rig.rate,
        )
        .expect("child extension");
        let xc_tx = cosign(&parse(&xc.tx_hex), leaf_slot, &c.spk, &c.kp);
        let sc = mercurylib::tesr::build_state(
            &xc_tx.txid().to_string(), xc.out_value, &rig.receiver.address, NET,
            rig.params.state_csv(0), rig.rate,
        )
        .expect("child state");
        let sc_tx = cosign(&parse(&sc.tx_hex), xc.out_value, &c.spk, &c.kp);
        cb.child_extension = tier(&xc_tx, Some(rig.params.ext_csv(0)), xc.payload_vout);
        cb.child_state = tier(&sc_tx, Some(rig.params.state_csv(0)), sc.payload_vout);

        // THE RE-LABEL: one tier, the extension re-declared as a superseded state.
        cb.ancestors[0].extension = None;
        cb.ancestors[0].state = tier(&csp_tx, Some(SPINE_CSV), csp.payload_vout);
        cb.ancestors[0].superseded_states =
            vec![tier(&fat_tx, Some(rig.params.ext_csv(0)), fat.payload_vout)];
        facts.ancestors[0].num_sigs = CHILD_V2_BASELINE + 1 + 1;

        // The fallback is provably dead: the amount the re-labelled reading synthesises is exactly
        // the one this state really was co-signed against.
        verify_tier_cosigned(&csp_tx, slot, &g.spk).expect(
            "the lone tier must verify against the FUNDING outpoint's value — otherwise this test is \
             just the previous one again, refused by arithmetic",
        );

        let msg = verify(&cb, &facts)
            .expect_err("the re-anchor must refuse this on structure alone")
            .to_string();
        assert!(
            msg.contains("the lone tier does not spend the segment's funding outpoint"),
            "the refusal must be the re-anchor and nothing else: {msg}"
        );
    }

    /// **(2) THE `[0,0]` CSV PIN — why the lone tier's bound must never be widened.**
    ///
    /// The mirror-image re-label: keep the real EXTENSION as the lone tier (it genuinely spends the
    /// funding outpoint, so check (1) passes) and park the real split state in `superseded_states`.
    /// The only thing separating an extension from a spine tier here is the timelock — and note that
    /// `[e_floor,e0]` is a strict SUBSET of `[d_floor,d0]`, so extension-vs-state was never
    /// CSV-separable at all. `[0,0]` is the sole disjoint interval, which is exactly why "the lone
    /// tier might be either kind, so allow `[d_floor,d0]`" would delete the last structural layer.
    ///
    /// **Measured, not assumed:** widening the `None` branch to `[d_floor, d0]` and re-running gives
    /// *"superseded state 0: CSV 0 outside bounds [6,24]"* — the parked split state falls foul of the
    /// superseded battery's own band. So in THIS construction the widening is still caught, by an
    /// incidental layer that only fires because the parked tier happens to be a spine tier. The pin is
    /// what keeps the lone tier's KIND structural instead of inferred, and it is what fires first.
    #[test]
    fn a_real_extension_passed_off_as_the_lone_spine_tier_is_refused_by_the_csv_pin() {
        let rig = rig();
        let (mut cb, facts) = rig.build(true, Payee::Honest);

        let real_ext = cb.ancestors[0].extension.take().expect("the two-tier rig has one");
        let real_state = std::mem::replace(&mut cb.ancestors[0].state, real_ext);
        cb.ancestors[0].superseded_states.push(real_state);

        // The re-anchor is SATISFIED — an extension does spend its segment's funding outpoint — so
        // this variant is not caught by (1). Stated positively so the test cannot silently become a
        // second copy of the one above.
        let sp: Transaction = parse(&cb.parent.current().state.signed_tx);
        let lone: Transaction = parse(&cb.ancestors[0].state.signed_tx);
        assert_eq!(lone.input[0].previous_output.txid, sp.txid());
        assert_eq!(lone.input[0].previous_output.vout, cb.ancestors[0].funding_vout);
        let ext_csv = lone.input[0].sequence.0 as u16;
        assert!(
            ext_csv >= rig.params.e_floor && ext_csv <= rig.params.e0,
            "the lone tier is a genuine, in-schedule EXTENSION at CSV {ext_csv}"
        );
        assert!(
            ext_csv >= rig.params.d_floor && ext_csv <= rig.params.d0,
            "…and it also sits inside the STATE band — widening the bound would admit it"
        );

        let msg = verify(&cb, &facts)
            .expect_err("an extension posing as the lone spine tier must be REFUSED")
            .to_string();
        assert!(
            msg.contains("SPINE state") && msg.contains(&format!("CSV {ext_csv} outside [0,0]")),
            "the refusal must be the CSV pin: {msg}"
        );
    }

    /// **(3) THE DEAD KNOB.** A spine segment has no extension rung, so `superseded_extensions` has no
    /// honest writer on one. Free, independent of the re-anchor, and it closes the re-declaration
    /// route head-on: wherever a re-labelled segment's dropped extension is parked, one of the two
    /// checks is looking at it.
    #[test]
    fn a_spine_segment_disclosing_superseded_extensions_is_refused() {
        let rig = rig();
        let (mut cb, mut facts) = rig.build_shaped(Ancestry::Spine, Payee::Honest);

        // A genuine, fully co-signed extension over the segment's funding outpoint — the very tier a
        // re-labelled two-tier segment would need to park somewhere.
        let g = &rig.ancestor;
        let sp: Transaction = parse(&cb.parent.current().state.signed_tx);
        let slot = sp.output[0].value;
        let decoy = mercurylib::tesr::build_extension_from(
            &sp.txid().to_string(), 0, slot, &g.address, NET, rig.params.ext_csv(0), rig.rate,
        )
        .expect("a real extension");
        let decoy_tx = cosign(&parse(&decoy.tx_hex), slot, &g.spk, &g.kp);
        cb.ancestors[0]
            .superseded_extensions
            .push(tier(&decoy_tx, Some(rig.params.ext_csv(0)), decoy.payload_vout));
        // Every other layer would have waved it through: it is co-signed, in the extension band, and
        // it loses the race for the funding outpoint to the live spine tier at CSV 0. So the census is
        // paid its extra slot and balances.
        facts.ancestors[0].num_sigs = CHILD_V2_BASELINE + 1 + 2;

        let msg = verify(&cb, &facts)
            .expect_err("a spine segment cannot supersede an extension rung it never had")
            .to_string();
        assert!(
            msg.contains("no extension rung to supersede"),
            "the refusal must name the dead knob: {msg}"
        );
    }

    /// **[V2] THE CENSUS TIER TERM IS DERIVED, AND THAT IS WHY V1 AND V2 ARE ONE COMMIT.**
    ///
    /// The old term was the literal `CHILD_V2_BASELINE + 2 + superseded`. Against a bundle that
    /// discloses only ONE tier that expectation is one too HIGH — i.e. a free census slot, absorbing
    /// exactly one co-sign the bundle never shows the receiver. A hidden rival state over this
    /// segment's funding outpoint is precisely what that slot pays for, and the mismatch fails OPEN.
    #[test]
    fn the_census_tier_term_is_derived_so_a_one_tier_segment_has_no_free_slot() {
        let rig = rig();
        let (cb, mut facts) = rig.build_shaped(Ancestry::Spine, Payee::Honest);

        let disclosed = CHILD_V2_BASELINE + 1 + 1; // one tier + one superseded cap
        assert_eq!(facts.ancestors[0].num_sigs, disclosed, "the honest count");
        // What the LITERAL `2` would have expected — and therefore admitted.
        facts.ancestors[0].num_sigs = CHILD_V2_BASELINE + 2 + 1;

        let msg = verify(&cb, &facts)
            .expect_err("one co-sign more than the bundle accounts for must be REFUSED")
            .to_string();
        assert!(
            msg.contains("num_sigs mismatch") && msg.contains(&format!("accounts for {disclosed}")),
            "the refusal must be the census, naming the DERIVED expectation: {msg}"
        );
    }

    /// **[hazard: the two loops] `child_exit_chain` and `child_exit_labels` are reconciled only by a
    /// length check**, so V1 has to move both or neither. Guard one and not the other and every CATS
    /// bundle is refused as an internal error; leave them length-equal but MIS-PAIRED and
    /// `bind_declared_csv` compares a state's declared CSV against an extension's signed one.
    #[test]
    fn a_spine_segment_contributes_exactly_one_entry_to_the_exit_chain_and_its_labels() {
        let rig = rig();
        let (spine, _) = rig.build_shaped(Ancestry::Spine, Payee::Honest);
        let (two_tier, _) = rig.build_shaped(Ancestry::TwoTier, Payee::Honest);

        assert_eq!(
            child_exit_chain(&two_tier).len() - child_exit_chain(&spine).len(),
            1,
            "one tier fewer in the chain — this IS the flat-in-depth win CATS is built for"
        );
        // `child_exit_chain_bound` is the length check: it errors "internal: …" if the two loops
        // disagree, and it is the ONLY caller that would notice.
        let bound = child_exit_chain_bound(&spine).expect(
            "chain and labels must agree — a disagreement here refuses every CATS bundle as an \
             internal error",
        );
        assert_eq!(bound.len(), child_exit_chain(&spine).len());
        // T, X, SP, SP2, X_c, S_c — and the ancestor entry is the SPINE tier, read off its signature.
        assert_eq!(bound.len(), 6);
        assert_eq!(bound[3].1, Some(SPINE_CSV), "the ancestor's lone tier, bound to its nSequence");
    }
}
