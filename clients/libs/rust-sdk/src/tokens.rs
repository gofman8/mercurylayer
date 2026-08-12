//! Tokens on RGB rails: issuance, balances and off-chain token transfers.
//!
//! The token standard is RGB (rgb-lib): assets are client-validated contracts whose allocations
//! ride statechain coins. A token transfer is a **colored off-chain split** — one SE-co-signed,
//! un-broadcast tx carving a sub-coin that carries the exact token amount (plus the sender's
//! change) — followed by the same branch-carrying key handover used for sats. The consignment
//! travels inside the transfer message (BackupTx.rgb_consignment); the receiver validates it
//! off-chain against the branch and books the balance under the consignment's verified contract.

use anyhow::{anyhow, Result};
use mercury_rgb::{RgbWallet, ValidationVerdict};
use mercurylib::wallet::CoinStatus;
use mercuryrustlib::rgb::{TierRole, TierSeal};
use serde::{Deserialize, Serialize};

use crate::types::{SdkError, TokenBalance, TransferResult, TransferredCoin};
use crate::wallet::UtexoWallet;

// The global `TOKEN_BLINDING: u64 = 777` that used to live here is GONE. Its doc comment claimed
// "a fixed value is fine"; `docs/utexo/CTESR-GATE.md` §2.2/§4.2 measured that claim and it is
// conditionally FALSE. Two RGB transitions over the SAME parent outpoint with equal amounts and
// equal blinding collapse to one `OpId` and one `BundleId`, and rgb-lib then resolves that BundleId
// to whichever rival witness has the numerically smallest INTERNAL (little-endian) txid — an
// arbitrary hash lottery. The losing transition's consignment embeds the rival's witness, so no
// branch the receiver can try will validate; the allocation is simply unclaimable off-chain.
//
// Every colouring in this module now derives its blinding from a `TierSeal`
// (`H(statechain_id ‖ role ‖ tier_index ‖ rung)`), which is unique over the whole
// `(parent outpoint, role, index)` space and deterministically re-derivable by the receiver.
// RECORDED SAFE DEFAULTS IN THIS FILE — reviewed, deliberately left alone.
// ------------------------------------------------------------------------
// The audited defect class is a failure that presents as a benign empty/idle result, and the class
// is defined by DIRECTION, not by spelling: a default that leads to MORE work or LESS spending is
// safe; one that leads to LESS protection is the bug. The remaining `unwrap_or_default()`s here are
// all of the first kind, and are listed so the next reviewer does not have to re-derive it:
//
//  * `format!("{}:{}", coin.utxo_txid.clone().unwrap_or_default(), coin.utxo_vout.unwrap_or_default())`
//    — building an outpoint key for a coin, in the carrier-selection loops. A coin missing a txid
//    yields ":0", which matches NO RGB allocation, so the coin is simply not selected as a carrier.
//    The failure direction is "spend nothing", i.e. fail closed. (The later `carrier_op` uses of the
//    same shape are on a coin that was ALREADY selected by matching a real allocation with this very
//    key, so a missing txid there is unreachable by construction.)
//  * `coin.amount.unwrap_or_default()` — a coin of unknown value reads as 0 sats, which fails the
//    "carrier too small" and backup-fee-floor checks below and refuses the operation. Fail closed.
//  * `rec.piece_id.clone().unwrap_or_default()` in the recovery REPORT — a display field on an
//    outcome struct. The durable state is the journal row, which is unaffected.
//  * the `SystemTime::duration_since(UNIX_EPOCH)` fallback in `journal_upsert` — a timestamp used
//    only to order the open-entry scan. A pre-1970 clock degrades ordering, never durability.
//
// Everything that reads a coin's PROTECTION (backup rows, exit branch, consignment envelope, spend
// generation, rejection marker) goes through `read_backup_rows`, which refuses to turn an unreadable
// database into an empty answer. `scripts/ci/deny-swallowed-backup-reads.sh` keeps it that way.

/// The committed fee rate every TES-R tier bakes in — `TesrParams::committed_fee_rate`, which is
/// `2.0` on mainnet AND regtest. It is a PROTOCOL constant, not the live network fee rate: a tier is
/// pre-signed years before it is broadcast, so its fee is fixed at build time and topped up by the
/// P2A anchor if the mempool has moved.
#[allow(dead_code)] // read by the derivation tests below; the constant IS the documentation
pub(crate) const TIER_COMMITTED_FEE_RATE: f64 = 2.0;

/// Fee-rate head-room [`TOKEN_PIECE_SATS`] is derived at: the piece must still carry a full coloured
/// ROOT ladder if the committed tier fee rate is DOUBLED. Raising `committed_fee_rate` is the one
/// change that can retroactively strand every piece already in circulation (a piece's sats are fixed
/// the moment it is carved, the floor is not), so the head-room is bought up front.
#[allow(dead_code)] // read by the derivation tests below
pub(crate) const PIECE_FEE_RATE_HEADROOM: f64 = 2.0;

/// Sats carried by a token-piece sub-coin.
///
/// # This number is DERIVED. Do not round it.
///
/// A token piece is a coin like any other: its receiver claims it and `claim()` ladders it, and once
/// the carrier is coloured that ladder is a COLOURED one. A coloured rung costs
/// `colored_committed_fee(1, rate) + P2A_VALUE` = `ceil(rgb::colored_tier_vbytes(1) * rate) + 240`,
/// which is dearer than a plain rung by the opret's `P2TR_OUT_VBYTES * rate` PLUS the 1 vB of the
/// explicit `SIGHASH_ALL` byte ([D4]). So, at the protocol committed rate of
/// [`TIER_COMMITTED_FEE_RATE`] = 2 sat/vB:
///
/// | floor | tiers | value |
/// |---|---|---|
/// | coloured ROOT ladder (`tesr::colored_ladder_floor`) | `T`, `X_0`, `S_0` | `3·576 + 330` = **2_058** |
/// | coloured CHILD ladder (`tesr::colored_child_floor`) | `ext_child`, `state_child` | `2·576 + 330` = **1_482** |
///
/// The legacy value was **1_500**, which sits between the two — and that gap was a trap, not a
/// margin. A 1_500-sat piece clears the CHILD floor, so a coloured in-ladder split will happily
/// carve one; it does NOT clear the ROOT floor, so the moment its receiver claims it as a coin of
/// its own the colouring is refused and the piece falls back to the flat lane. Retiring the flat
/// lane with `TOKEN_PIECE_SATS = 1_500` would therefore strand every received piece.
///
/// The value below is the coloured ROOT floor computed at
/// `TIER_COMMITTED_FEE_RATE * PIECE_FEE_RATE_HEADROOM` = 4 sat/vB:
/// `3 · (ceil(168 · 4.0) + 240) + 330` = `3 · (672 + 240) + 330` = **3_066**.
/// It clears the coloured root floor at 2 sat/vB with 1_008 sat of head-room, and still clears it
/// exactly if the committed rate ever doubles.
///
/// **[D4] moved this number 3_054 → 3_066.** The coloured tier's true signed vsize is 168 vB, not
/// 167: a TES-R taproot signature is serialised with an explicit `SIGHASH_ALL` byte, so the witness
/// item is 65 bytes. The old 3_054 was the 4-sat/vB root floor of a 167-vB model, and left the
/// head-room at 1.988x rather than the 2x this constant claims.
///
/// `pub` (was `pub(crate)`) so the E2Es and the granularity model pin THIS constant rather than a
/// hand-copied literal — a copy is how the number went stale in the first place.
/// [`token_piece_sats_is_the_coloured_root_floor`] is the derivation, executable: it recomputes the
/// floors from the real `tesr` functions and fails if this constant ever drops below them.
pub const TOKEN_PIECE_SATS: u64 = 3_066;

/// **Chained token sends one carrier supports — PER LANE. [P0-6]**
///
/// There is no single such number, and the former global `CARRIER_SEND_DEPTH = 5` was a claim about
/// whichever of the two coloured spend lanes the reader happened to have in mind:
///
/// | lane | selected by | second send off the same carrier |
/// |---|---|---|
/// | LEGACY flat coloured split (`create_colored_split_tx`) | `SdkConfig::colored_ladder == false` | allowed — the change is a plain sub-coin and splits again |
/// | CTES-R coloured in-ladder split (`colored_in_ladder_pay`) | `SdkConfig::colored_ladder == true` | **STRUCTURALLY REFUSED** |
///
/// On the CTES-R lane the change of a coloured split is a depth-1 COLOURED CHILD, and three guards
/// in `mercuryrustlib::tesr` each independently refuse to carve a second piece out of it:
/// `refuse_uncolored_over_colored_child` (an uncoloured tier over a sealed output burns the
/// allocation), `ChildTesrBundle::colored_child_txids` (MAX_COLORED_ADOPT_DEPTH — a
/// coloured grandchild has no derivable seal schedule), and `colored_in_ladder_pay` itself, which
/// loads a ROOT `tesr-` bundle and a child has none. So the CTES-R depth is **1**, not 5, and no
/// amount of extra sats buys a sixth — or a second — send on that lane.
///
/// [`the_two_coloured_lanes_have_different_send_depths`] pins both numbers against the real guards
/// and the real sizing functions.
pub(crate) const LEGACY_CARRIER_SEND_DEPTH: u64 = 5;
/// The CTES-R lane's hard cap. Not a sizing choice — a structural property of the coloured child
/// bundle (see [`LEGACY_CARRIER_SEND_DEPTH`] for the three guards that enforce it).
#[allow(dead_code)] // read by the derivation tests and by config's margin derivation
pub(crate) const CTESR_CARRIER_SEND_DEPTH: u64 = 1;

/// The split fee reserve on the LEGACY lane for carriers this size: `split_fee_reserve` floors at
/// 300 sat below a 30_000-sat parent.
const LEGACY_SPLIT_RESERVE_FLOOR: u64 = 300;
/// The tail the LEGACY lane's last change must land on: `min_split_output(TIER_COMMITTED_FEE_RATE)`
/// = `DUST_LIMIT + 112 vB of backup at 2 sat/vB` = `330 + 224`. Not expressible as a `const fn` (the
/// rate is an `f64`), so [`carrier_supports_the_full_send_depth`] recomputes it from the REAL
/// `min_split_output` and fails if it ever moves.
const LEGACY_CARRIER_TAIL: u64 = 554;

/// Sats a LEGACY-lane carrier needs to afford `send_depth` chained flat splits: each send consumes a
/// piece plus the floored fee reserve, and the last change must still clear the sub-coin floor.
pub(crate) const fn legacy_carrier_sats(send_depth: u64) -> u64 {
    send_depth * (TOKEN_PIECE_SATS + LEGACY_SPLIT_RESERVE_FLOOR) + LEGACY_CARRIER_TAIL
}

/// Sats a CTES-R-lane carrier needs to afford `send_depth` (i.e. one) coloured in-ladder split into
/// a piece plus a change child. **DERIVED from the REAL coloured sizing functions**, walking exactly
/// the chain `colored_in_ladder_pay` walks:
///
/// `F` → `T` → `X_0` → `SP`(2 children) → { piece, change }, where every rung is a coloured tier.
///
/// Not a `const fn` for the same reason as [`LEGACY_CARRIER_TAIL`] (the rate is an `f64`), which is
/// why [`TOKEN_CARRIER_SATS`] takes the max of the two lanes in a TEST rather than in the `const`.
#[allow(dead_code)] // read by the derivation tests; the function IS the documentation
pub(crate) fn ctesr_carrier_sats(send_depth: u64, fee_rate: f64) -> u64 {
    use mercurylib::tesr::P2A_VALUE;
    use mercuryrustlib::rgb::colored_committed_fee;
    use mercuryrustlib::tesr::{colored_child_floor, COLORED_LADDER_DUST};
    // `SP`'s children: `send_depth` pieces plus the change child that keeps the rest of the
    // allocation. Each must clear `colored_child_floor` (its own two coloured rungs + dust); the
    // pieces are carved at `TOKEN_PIECE_SATS`, which already exceeds that floor.
    let n_children = (send_depth + 1) as usize;
    let at_sp = send_depth * TOKEN_PIECE_SATS + colored_child_floor(fee_rate, COLORED_LADDER_DUST);
    // Add back the rungs between `F` and `SP`, each the exact inverse of the REAL
    // `colored_tier_out_total` (`prev − colored_committed_fee(n) − P2A`): `SP` carries `n_children`
    // payload outputs, `X_0` and `T` one each.
    at_sp
        + (colored_committed_fee(n_children, fee_rate) + P2A_VALUE)
        + 2 * (colored_committed_fee(1, fee_rate) + P2A_VALUE)
}

/// Sats a freshly-issued token carrier is funded with.
///
/// **DERIVED, and derived from the LARGER of the two lanes' requirements. [P0-6]**
///
/// A carrier is FUNDED at issuance, before the lane its future sends will take is known: the lane is
/// chosen per spend by `SdkConfig::colored_ladder`, which a wallet may flip at any time between
/// issuing a carrier and spending it. Sizing for one lane therefore has to mean sizing for both, and
/// the failure directions are not symmetric — an over-sized carrier parks sats in a change child,
/// an under-sized one is refused at spend time with the carrier already terminalized. Fail closed:
///
/// | lane | derived requirement |
/// |---|---|
/// | LEGACY, `LEGACY_CARRIER_SEND_DEPTH` = 5 | `5 · (3_066 + 300) + 554` = **17_384** |
/// | CTES-R, `CTESR_CARRIER_SEND_DEPTH` = 1 | `T`+`X_0`+`SP(2)` over piece + child floor = **6_362** |
///
/// so the carrier is **17_384**, the max. The SATS did not move; what moved is that they are now
/// derived from BOTH lanes and that the "splits exactly five times" claim is scoped to the lane
/// where it is true. On the CTES-R lane 17_384 buys ONE send and the remaining ~11_000 sat land in
/// the depth-1 change child, which can only be moved whole or exited — that is a real cost of the
/// coloured lane, recorded here rather than hidden behind a number that reads like five sends.
///
/// `pub` so the E2Es filter carriers by THIS constant instead of a copied literal.
/// [`carrier_supports_the_full_send_depth`] walks the five LEGACY sends through the REAL split
/// guard; [`the_two_coloured_lanes_have_different_send_depths`] recomputes both rows of the table
/// above from the REAL coloured sizing functions and fails if either lane outgrows this constant.
pub const TOKEN_CARRIER_SATS: u64 = legacy_carrier_sats(LEGACY_CARRIER_SEND_DEPTH);

/// **[K>1] COLOURED K > 1 IS REFUSED BY NAME. The plain lane only.**
///
/// One `SP` may carry K payee payloads on the PLAIN lane. On the COLOURED lane it may not, and the
/// reason is not that the builder cannot do it — [`mercuryrustlib::tesr::build_colored_in_ladder_split`]
/// is already N-ary. It is that a coloured `SP`'s K payloads share ONE seal blinding.
///
/// `mercuryrustlib::rgb::build_colored_tier` derives a single `let blinding = seal.blinding()`
/// (`clients/libs/rust/src/rgb.rs:1245`) and passes it once for an `output_map` covering every
/// payload; `mercuryrustlib::tesr::colored_tier_seal` takes the parent statechain id, the role, `m`
/// and the CSV — nothing child-specific. A concealed seal commits to `(method, txid, vout, blinding)`,
/// so a payee who holds their own piece knows `B`, knows the witness txid, and can enumerate the
/// vouts: **K tries de-conceal every sibling seal in the batch**.
///
/// At K = 1 that leaks the sender's own change seal to the one payee already transacting with them —
/// a cost this lane has always paid and §4.5 accepts. At K > 1 it makes mutually unrelated payees and
/// their exact allocations linkable to each other, which is not a cost anybody agreed to and cannot
/// be undone once the consignment is out. It is not theft — a seal is not spendable without the key —
/// but concealment across a coloured batch is worth **zero bits**, and a privacy property that is
/// silently zero is worse than one that is absent.
///
/// The fix is per-output blinding (§4.5 item 3), which is a change to the coloured tier builder, its
/// seal derivation and the receiver's resolution — a separate commit with its own anti-collision
/// argument (rival tiers over one outpoint must not share a blinding or their `BundleId`s collapse).
/// Until it lands the capability is refused rather than shipped un-private.
///
/// ⚠️ **This removes a capability, and there is no fallback within one carrier.** A coloured carrier
/// is split exactly once (`SP` terminalizes it, and the change is a depth-1 coloured child no guard
/// will split again — see [`LEGACY_CARRIER_SEND_DEPTH`]), so "pay them one at a time" means one
/// CARRIER per recipient, via [`UtexoWallet::transfer_tokens`] or the multi-carrier lane.
pub(crate) fn refuse_colored_multi_payee(payees: usize) -> Result<()> {
    if payees <= 1 {
        return Ok(());
    }
    Err(anyhow!(
        "coloured K > 1 refused: this batch pays {payees} recipients out of ONE coloured carrier, \
         and a coloured split state gives all {payees} payload outputs the SAME seal blinding \
         (`build_colored_tier` derives one `seal.blinding()` for the whole output map). A concealed \
         seal commits to (method, txid, vout, blinding), so each payee could de-conceal every other \
         payee's seal and allocation in {payees} tries — concealment across the batch would be worth \
         zero bits. Per-output blinding is a separate change; until it lands, pay coloured recipients \
         one per carrier (`transfer_tokens`). Nothing has been co-signed and the carrier is untouched."
    ))
}

/// Rung-space flag that separates the BATCH split lane from the single-recipient split lane.
///
/// Both lanes spend the same carrier under `TierRole::Split` at the same `tier_index` (the carrier's
/// spend generation), so only the `rung` can tell them apart, and both key it on the transition's
/// arity. The single lane's arity is always 2 (piece + change). A batch's arity is `n + 1`, and
/// `batch_transfer_tokens` rejects only an EMPTY recipient list — so a batch of ONE recipient also
/// has arity 2 and would derive the byte-identical seal. Setting the high bit puts every batch rung
/// in a range the single lane can never reach (a split's arity is bounded by its output count, far
/// below `2^31`), which restores uniqueness at every arity including 1.
const BATCH_SPLIT_RUNG_FLAG: u32 = 0x8000_0000;

/// THE single-recipient split lane's seal (`transfer_tokens`). `arity` is the transition's output
/// count (piece + change = 2); `generation` is the carrier's spend generation (`parent_backups`).
///
/// This is the ONLY place the single lane's rung is expressed. Both split lanes go through these two
/// functions so the disjointness they promise is a property of the code that actually runs, and so
/// the unit tests below can exercise the real derivation instead of restating it.
fn single_split_seal(carrier_id: &str, generation: u32, arity: u32) -> TierSeal {
    TierSeal::new(carrier_id, TierRole::Split, generation, arity)
}

/// THE batch split lane's seal (`batch_transfer_tokens`): the same derivation moved into a disjoint
/// rung space by [`BATCH_SPLIT_RUNG_FLAG`]. `arity` is `recipients + 1` (the change output).
fn batch_split_seal(carrier_id: &str, generation: u32, arity: u32) -> TierSeal {
    debug_assert_eq!(
        arity & BATCH_SPLIT_RUNG_FLAG,
        0,
        "a split arity must never reach the lane-separating flag bit"
    );
    TierSeal::new(carrier_id, TierRole::Split, generation, BATCH_SPLIT_RUNG_FLAG | arity)
}

/// The CTES-R build-time collision assert (`docs/utexo/CTESR-GATE.md` §3.1), for the lanes that
/// colour through `RgbWallet::color` (`create_colored_split_tx` / `create_colored_combine_tx`)
/// rather than through `mercuryrustlib::rgb::build_colored_tier`, which carries the guard itself.
///
/// Two RGB transitions over the same parent outpoint(s) with equal amounts and an equal seal
/// blinding collapse to one `OpId` and one `BundleId`; rgb-lib resolves that BundleId to whichever
/// rival witness has the smallest INTERNAL (little-endian) txid. The loser's consignment therefore
/// comes back carrying the RIVAL's witness, and no branch the receiver can try will validate. So the
/// absence of our own witness from the consignment we just produced is proof that the seal blinding
/// collided — a sender-side defect that must never be handed to a receiver as an unvalidatable
/// consignment. Fail closed here instead.
fn assert_own_witness(lane: &str, txid: &str, consignment: &str, blinding: u64) -> Result<()> {
    let witnesses = mercury_rgb::consignment_witness_txids(consignment)?;
    if !witnesses.iter().any(|w| w == txid) {
        return Err(anyhow!(
            "{lane} {txid}: its OWN witness is absent from the consignment it just produced \
             (bundled witnesses: {witnesses:?}). The seal blinding {blinding} collided with a rival \
             transition over the same parent output(s) — rgb-lib merged them into one BundleId and \
             kept the rival with the smaller internal txid, so this consignment is unvalidatable by \
             ANY receiver."
        ));
    }
    Ok(())
}

/// **[CTES-R MIGRATION] THE ONE-WAY HATCH: may this route spend these carriers' `F` on the legacy
/// RGB-aware lane even though `colored_ladder` is on?**
///
/// `carriers` is [`CarrierMigrationFacts`] per input. `Ok(())` opens the hatch; `Err(why)` is the
/// sentence the gate appends to its refusal.
///
/// # Why a hatch exists at all, and why it is this one
///
/// The retirement gate's premise is that the legacy split and a coloured trigger `T` are RIVAL spends
/// of the same `F`, and `T` has no timelock, so the previous owner wins. **That premise needs a `T`
/// to be possible.** For a carrier whose ladder cannot be built, it is absent — and then the refusal
/// protects nothing while costing everything: the legacy lane is the ONLY RGB-aware spend such a coin
/// has, and with it closed the coin cannot be paid from, cannot be plainly withdrawn (that burns the
/// asset) and cannot be laddered. Retiring the lane wholesale converts a working coin into an
/// unspendable one, which is strictly worse than the hazard it avoids.
///
/// # `colourable_now` is EVIDENCE, not a proxy
///
/// The first draft of this gate keyed the hatch on the carrier's SIZE — below
/// [`UtexoWallet::colored_root_floor`] and no ladder — because that is `build_colored_ladder`'s first
/// pre-flight and a coin's funding value can never move. It is sound and it covers the largest class
/// (every pre-flip 1_500-sat piece: above the coloured CHILD floor, so a split carved it, and below
/// the ROOT floor, so its receiver can never ladder it). It is also **not enough**: sdk78 measured a
/// TOKEN_PIECE_SATS-sized piece — comfortably above the floor — that rgb-lib still refuses to colour
/// (`Invalid coloring info`), and a size-only hatch strands exactly that coin.
///
/// So the condition is the real one: **can a coloured ladder be built for this carrier right now?**
/// [`UtexoWallet::carrier_migration_facts`] answers it by re-running, read-only, every precondition
/// `build_colored_ladder_auto` would hit — the sats floor, the "exactly one booked allocation" test
/// the claim path applies, and rgb-lib's own `color_psbt` over `F`. Nothing is consumed and no
/// witness is resolved (`rgb::probe_allocation`, §3.3), so the probe cannot damage the coin it is
/// asking about.
///
/// The answer is not permanent for all time, and it does not need to be. It is evaluated under
/// `wallet_lock`, which `claim()` also takes — so while it holds, the pass that would build `T`
/// cannot run, and by the time the lock is released the carrier is terminalized and no longer
/// eligible for laddering at all. "No rival can exist" is therefore a fact about the window in which
/// the spend happens, which is the only window in which it matters.
///
/// # Why it cannot be widened by accident
///
/// Three conditions, all necessary:
///
///  * **the list is non-empty** — an empty list is not "no objection", it is no evidence. A caller
///    that reaches the gate without naming what it is about to spend gets a refusal;
///  * **no** carrier is colourable now — one colourable input is one coin a `claim()` pass will
///    ladder, i.e. exactly the case the gate was written for, and it closes the hatch for the whole
///    route rather than for itself alone (a combine spends every input's `F` in one transaction, so
///    one rival is enough);
///  * **no** carrier holds a ladder of any kind. Not "no COLOURED ladder" — no ladder. A plain ladder
///    over a carrier would be a defect, but its `T` would still spend `F`, so the hatch is keyed on
///    the rival's existence rather than on its colour.
///
/// Free-standing (not a method) so the decision itself is unit-testable without a wallet, database or
/// network: `migration_hatch_is_narrow` below drives every branch.
pub(crate) fn migration_hatch_verdict(
    floor: u64,
    carriers: &[CarrierMigrationFacts],
) -> std::result::Result<(), String> {
    if carriers.is_empty() {
        return Err(
            "No carrier was named at this gate, so nothing can be proved about what it is about \
             to spend. Refusing rather than paying over the retired lane."
                .to_string(),
        );
    }
    let laddered: Vec<&str> = carriers
        .iter()
        .filter(|c| c.has_ladder)
        .map(|c| c.statechain_id.as_str())
        .collect();
    if !laddered.is_empty() {
        return Err(format!(
            "Carrier(s) {} already hold a TES-R ladder, whose trigger spends `F` with no timelock; \
             this spend would be a rival its previous owner could out-race instantly. Move them \
             along their ladder instead.",
            laddered.join(", ")
        ));
    }
    let waiting: Vec<String> = carriers
        .iter()
        .filter(|c| c.colourable_now)
        .map(|c| format!("{} ({} sat)", c.statechain_id, c.sats))
        .collect();
    if !waiting.is_empty() {
        return Err(format!(
            "Carrier(s) {} CAN be coloured right now — they clear the coloured root floor of \
             {floor} sat and their allocation is still spendable out of `F` — so they have simply \
             not been laddered YET, and a later `claim()` pass will do it. Refusing rather than \
             paying over the retired lane. (The migration hatch opens only for carriers for which \
             no coloured ladder can be built at all, and which therefore can never carry a trigger \
             to rival this spend.)",
            waiting.join(", ")
        ));
    }
    Ok(())
}

/// What [`migration_hatch_verdict`] decides on, gathered by
/// [`UtexoWallet::carrier_migration_facts`]. Split out so the decision can be driven directly by
/// unit tests and so each field has one, named, documented meaning.
#[derive(Clone, Debug)]
pub(crate) struct CarrierMigrationFacts {
    pub statechain_id: String,
    /// The carrier's funding value, for the refusal message.
    pub sats: u64,
    /// Does this coin hold a TES-R ladder of ANY kind (coloured or plain)? If it does, a trigger
    /// already spends `F` and the hatch must stay shut.
    pub has_ladder: bool,
    /// Could `build_colored_ladder_auto` succeed for this coin right now? Determined READ-ONLY; see
    /// [`migration_hatch_verdict`] for why "now" is the right tense.
    pub colourable_now: bool,
}

/// How a colored transfer's piece is handed over.
pub(crate) enum ColoredLatch {
    /// Plain transfer (no latch).
    None,
    /// Batch-locked to an external payment hash (Lightning PAY: receiver claims on the preimage).
    ExternalHash(String),
    /// Batch-locked to an SE-generated preimage (Lightning RECEIVE: the SE reveals the preimage only
    /// after the coin is released).
    SePreimage,
}

/// Output of a colored transfer, with any latch artifacts.
pub(crate) struct ColoredTransferOut {
    pub result: TransferResult,
    pub piece_id: String,
    pub batch_id: Option<String>,
    pub se_hash: Option<String>,
}

/// Envelope stored in `BackupTx.rgb_consignment` so a token transfer is self-describing.
#[derive(Serialize, Deserialize)]
pub(crate) struct ConsignmentEnvelope {
    /// Consignment, base64.
    pub c: String,
    /// Advisory amount hint. NOT trusted: the receiver re-derives the booked amount from the
    /// consignment (`accept_offchain_amount`) and rejects the transfer if this disagrees.
    pub a: u64,
    /// Sats on the sub-coin.
    pub s: u64,
}

/// Read one backup-row key, separating a GENUINE absence from an INABILITY TO TELL.
///
/// This distinction is the whole point. `sqlite_manager::get_backup_txs` reports both outcomes
/// through `Err`: sqlx's `RowNotFound` (and the equivalent empty-row guard) means the key really has
/// no row — a true, informative "there is nothing here" — while any other error means the read
/// failed and the caller learned NOTHING. Every `.unwrap_or_default()` / `Err(_) => …` on this
/// function collapsed the two, so a locked, corrupted or closed database produced a confident empty
/// answer: no branch witnesses, no consignment, no carrier. Callers then acted on that emptiness.
///
/// `Ok(None)` = the row genuinely does not exist. `Err(_)` = the database could not be read, and the
/// caller must fail rather than substitute a default.
pub(crate) async fn read_backup_rows(
    pool: &sqlx::Pool<sqlx::Sqlite>,
    wallet_name: &str,
    key: &str,
) -> Result<Option<Vec<mercurylib::wallet::BackupTx>>> {
    match mercuryrustlib::sqlite_manager::get_backup_txs(pool, wallet_name, key).await {
        Ok(rows) => Ok(Some(rows)),
        Err(e) => {
            let missing = matches!(
                e.downcast_ref::<sqlx::Error>(),
                Some(sqlx::Error::RowNotFound)
            ) || e.to_string().contains("Statechain id not found");
            if missing {
                Ok(None)
            } else {
                Err(anyhow!("backup rows for '{key}' could not be read: {e}"))
            }
        }
    }
}

/// The sentinel `book_incoming_token` matches to decide a rejection is PERMANENT (and therefore that
/// the coin may be un-quarantined). Declared once, next to the only code allowed to emit it.
pub(crate) const PERMANENT_INVALID_SENTINEL: &str = "PERMANENT-INVALID";

/// Remove the permanent-rejection sentinel from untrusted text before it is interpolated into a
/// TRANSIENT error message.
///
/// The receiver classifies transient vs permanent by looking for [`PERMANENT_INVALID_SENTINEL`]
/// anywhere in the error string, and some of the text quoted into these errors originates in a
/// consignment an attacker wrote. Without this, a griefer could embed the sentinel in a payload that
/// surfaces in a resolver-failure detail and thereby trigger the irreversible un-quarantine from a
/// transient path. Scrubbing keeps the classification honest no matter what the input says.
pub(crate) fn scrub_permanent_sentinel(detail: &str) -> String {
    detail.replace(PERMANENT_INVALID_SENTINEL, "<redacted-marker>")
}

/// **[CTES-R] Classify the `tesr-` backup rows into `(PROVEN coloured, UNREADABLE)`.**
///
/// Deliberately PURE — no pool, no wallet — because the whole point is the third bucket, and a
/// bucket that only exists inside an async DB read is a bucket nobody tests. A row that parses
/// answers the question (coloured / plain); a row that does NOT parse answers nothing, and the two
/// callers of the census need opposite defaults for that case:
///
///   * `unilateral_exit` must refuse it — an unreadable bundle is not evidence that a coloured walk
///     exists, and broadcasting a plain tier over a carrier destroys the allocation;
///   * the plain-BTC QUARANTINE must admit it — dropping it there says "not a carrier", and a
///     coloured carrier that is not yet booked has no other arm covering it.
///
/// The old single `filter_map` served the first and betrayed the second with the same line.
fn classify_tesr_rows(
    rows: impl IntoIterator<Item = (String, String)>,
) -> (
    std::collections::HashSet<String>,
    std::collections::HashSet<String>,
) {
    let mut colored = std::collections::HashSet::new();
    let mut unreadable = std::collections::HashSet::new();
    for (key, json) in rows {
        let Some(sid) = key.strip_prefix("tesr-").map(|s| s.to_string()) else {
            continue;
        };
        match serde_json::from_str::<mercuryrustlib::tesr::TesrBundle>(&json) {
            Ok(bundle) if bundle.is_colored() => {
                colored.insert(sid);
            }
            // Parsed and says PLAIN: a real, negative answer. Not a carrier.
            Ok(_) => {}
            // No verdict at all. Reported, never silently discarded.
            Err(_parse) => {
                unreadable.insert(sid);
            }
        }
    }
    (colored, unreadable)
}

/// Turn a non-`Valid` RGB verdict into the receiver's error, choosing whether it may carry the
/// permanent-rejection sentinel. `None` means the consignment validated.
///
/// C2 lives here. This is deliberately PURE — no RGB engine, no network, no wallet — so the one
/// decision that can irreversibly strip a carrier's protection is unit-testable in isolation, which
/// is how `a_transient_resolver_failure_keeps_a_genuine_carrier_quarantined` pins it.
pub(crate) fn verdict_rejection(
    verdict: ValidationVerdict,
    detail: Option<&str>,
) -> Option<anyhow::Error> {
    match verdict {
        ValidationVerdict::Valid => None,
        // The validator reached a real verdict of INVALID. Only this may un-quarantine.
        ValidationVerdict::PermanentlyInvalid => Some(anyhow!(
            "{PERMANENT_INVALID_SENTINEL}: token consignment INVALID: {}",
            detail.unwrap_or_default()
        )),
        // TRANSIENT: no verdict was reached. No sentinel => the coin stays quarantined and the
        // claim is retried. `detail` is SCRUBBED because it is attacker-influenced text (a resolver
        // failure quotes material from a consignment a griefer may have authored) and the receiver
        // classifies by SUBSTRING: an un-scrubbed detail carrying the sentinel would let a griefer
        // trigger from this very branch the permanent un-quarantine it exists to prevent.
        ValidationVerdict::Unresolved => Some(anyhow!(
            "token consignment could not be validated (RGB resolver/indexer unreachable) — carrier \
             stays QUARANTINED, will retry: {}",
            scrub_permanent_sentinel(detail.unwrap_or("no detail"))
        )),
    }
}

/// Derive the branch witness txids a consignment chain resolves against, from the raw (hex) branch
/// transactions. Shared so the pre-payment gate and the claim path resolve the SAME witness set.
pub(crate) fn branch_witness_txids(branch_txs: &[String]) -> Result<Vec<String>> {
    let mut txids = Vec::with_capacity(branch_txs.len());
    for raw in branch_txs {
        let tx: bitcoin::Transaction =
            bitcoin::consensus::encode::deserialize(&hex::decode(raw)?)?;
        txids.push(tx.txid().to_string());
    }
    Ok(txids)
}

/// THE token-acceptance predicate — the SINGLE definition of "this consignment genuinely assigns
/// `env.a` of some contract to `witness_txid:witness_vout`". Returns `(contract_id, amount)` where
/// `amount` is the CONSIGNMENT-derived assignment (never the envelope hint).
///
/// Both the irreversible-payment gate (`validate_pending_token`, called by the SSP BEFORE it pays a
/// Lightning invoice) and the claim path (`accept_incoming_tokens`) call exactly this. Keeping them
/// as one function is a security requirement, not tidiness: while the SSP's pre-check was the weaker
/// of the two (it omitted the `booked == env.a` equality below), a payer could latch a coin whose
/// consignment validated but whose envelope amount was mutated — it passed the gate, the SSP paid an
/// irreversible invoice, and the SAME coin then failed PERMANENT-INVALID at claim. Two predicates
/// that must be kept in sync by hand is the bug; one predicate cannot disagree with itself.
///
/// Fails CLOSED. The `PERMANENT-INVALID:` prefix is emitted ONLY for a verdict the RGB validator
/// actually reached — see the C2 note below — because `claim()` matches that prefix to
/// UN-QUARANTINE the coin, which irreversibly discards its RGB protection.
///
/// C2 — WHY THIS FUNCTION BRANCHES ON A VERDICT AND NOT ON A BOOLEAN
/// ----------------------------------------------------------------
/// `validate_offchain_chain_info` used to return a bare `valid: bool`, and every falsy answer was
/// labelled `PERMANENT-INVALID:`. But rgb-lib returns `valid == false` for TWO different things:
/// a consignment that genuinely fails validation, and a validation that never reached a verdict
/// because a witness could not be resolved (indexer/electrum down, timing out, mid-reorg). The
/// second is transient and routine. Under the old code a five-second resolver outage during
/// `claim()` was laundered into a PERMANENT rejection: `book_incoming_token` wrote the durable
/// `token-rejected-<id>` marker, `consignment_bearing_outpoints` stopped quarantining the coin, and
/// a GENUINE carrier holding a real allocation became ordinary spendable BTC — after which any
/// plain-BTC path (transfer, withdraw, auto-refresh re-anchor) would spend it and destroy the
/// allocation. Nothing ever re-quarantines it: the marker is durable and the un-quarantine is
/// one-way. A transient failure must therefore leave the carrier QUARANTINED and merely surface
/// blindness, which is what [`ValidationVerdict::Unresolved`] does here.
///
/// The permanent/transient split is rgb-lib's own (`ValidateConsignmentResult::error` is `"invalid"`
/// vs `"resolver"`, straight off rgbstd's `ValidationError` arms) and is mirrored, not invented; see
/// [`ValidationVerdict::from_rgb_lib`], which also fails closed on an unrecognised tag.
pub(crate) fn verify_consignment_assignment(
    w: &mut RgbWallet,
    env: &ConsignmentEnvelope,
    branch_txids: &[String],
    witness_txid: &str,
    witness_vout: u32,
) -> Result<(String, u64)> {
    let (verdict, detail, contract_id) =
        tokio::task::block_in_place(|| w.validate_offchain_chain_info(&env.c, branch_txids))?;
    if let Some(e) = verdict_rejection(verdict, detail.as_deref()) {
        return Err(e);
    }
    let contract_id = contract_id.ok_or_else(|| {
        // Un-prefixed on purpose: "valid but no contract id" is a shape this bridge does not
        // understand, so it is treated as transient and the carrier keeps its quarantine.
        anyhow!("validated consignment without contract id — carrier stays QUARANTINED")
    })?;
    // The amount the CONSIGNMENT assigns to our own witness outpoint — the cryptographic source of
    // truth. The envelope amount (env.a) is only a hint we cross-check; a lying sender cannot
    // inflate the derived amount because the consignment governs it.
    let booked = tokio::task::block_in_place(|| {
        w.accept_offchain_amount(&env.c, branch_txids, witness_txid, witness_vout)
    })?;
    if booked != env.a {
        // Permanent by construction: both numbers are already in hand and no retry can change them.
        return Err(anyhow!(
            "{PERMANENT_INVALID_SENTINEL}: token consignment assigns {booked} to this coin but the envelope claimed {} — rejecting",
            env.a
        ));
    }
    Ok((contract_id, booked))
}

// ============================================================================================
// F7 — durable prepare/commit journal for STRUCTURAL colored spends (split + combine).
// ============================================================================================
//
// A colored split/combine makes the parent carrier(s) TERMINAL at the SE and then produces exactly
// one co-signature over the un-broadcast child transaction. Before this journal, everything between
// those two facts lived only in process memory: the signed tx, the consignment, the blinding, the
// child vouts. A crash anywhere after the co-signature and before `register_split_subcoins`
// persisted the children destroyed the cooperative off-chain path for good — the SE would never
// co-sign the carrier again, and the one transaction that spent it was gone. (The BTC stayed
// recoverable by unilateral exit of the carrier's own backup; the token/piece hand-over did not.)
//
// WHY THE TERMINALIZATION IS *NOT* MOVED AFTER THE SIGNATURE
// ---------------------------------------------------------
// The obvious "persist the signed child material BEFORE terminalizing" ordering is unsafe here and
// is deliberately NOT what this journal does. `set_spend_budget(carrier, 1)` is an ABSOLUTE budget
// (`finalized_count + remaining`, monotonically tightening — see `database::deposit::set_sig_budget`)
// and it is the *only* thing that stops the SE from issuing a SECOND co-signature over the same
// carrier. Signing first and calling `set_spend_budget(carrier, 0)` afterwards would still leave the
// carrier reading `terminal == true` to a receiver, but a malicious sender could co-sign branch A
// and branch B in the gap and hand them to two different receivers — both would see a terminal
// ancestor and both would accept. That is exactly the INV-19 fork the terminal check exists to
// prevent, and no client-side guard can bind a sender who simply skips it. So the budget is still
// pinned BEFORE the signature, and durability is bought with a write-ahead journal instead.
//
// WHAT THE JOURNAL BUYS
// ---------------------
//  * `Prepared` is committed to sqlite BEFORE `set_spend_budget`, so a crash in the pre-signature
//    window is *classifiable* rather than silent: the recovery reader asks the SE whether each
//    carrier is terminal and reports `Retryable` (budget pinned but never consumed — the whole
//    operation can simply be run again) or `CooperativePathLost` (the co-signature WAS finalized and
//    its transaction is gone — the holder must unilaterally exit that carrier). Fail-closed: a
//    carrier with a lost journal entry is never selected for a new colored spend.
//  * `Signed` is committed to sqlite IMMEDIATELY after the co-signature returns and BEFORE any other
//    work, so every later step (sub-coin registration, RGB re-registration, envelope attachment,
//    hand-over) is replayable after a restart. That is the bulk of the old window — several DB
//    writes, an RGB stash mutation and two network calls — and it is now fully recoverable.
//  * The one window that remains is the single HTTP round-trip between the SE finalizing the
//    signature and this process persisting it. It is irreducible on the client: closing it needs an
//    SE-side idempotent re-serve of a finalized partial signature.
//
/// Stage of a journalled structural spend. Ordered: every stage implies all earlier ones are done,
/// so recovery resumes at the first step AFTER the recorded stage.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum StructuralStage {
    /// Plan is durable; the parent carrier(s) have NOT been terminalized yet.
    Prepared,
    /// The co-signed child transaction + consignment are durable.
    Signed,
    /// The child sub-coins are registered as wallet coins (branch + backups persisted).
    Registered,
    /// The RGB change allocation is re-registered (or the carriers marked spent).
    Colored,
    /// The consignment envelope is attached to the piece's backup row.
    Enveloped,
    /// Done: the piece was handed over (or, after a replay, restored to this wallet). Terminal.
    Committed,
    /// Terminal: the operation never consumed its co-signature and was safely rolled forward to
    /// "never happened". The carrier keeps its pinned budget and can be re-spent once.
    Abandoned,
    /// Terminal: the co-signature was consumed but its transaction was lost. The carrier's
    /// cooperative path is GONE — unilateral exit only.
    Stranded,
}

impl StructuralStage {
    fn as_str(self) -> &'static str {
        match self {
            StructuralStage::Prepared => "prepared",
            StructuralStage::Signed => "signed",
            StructuralStage::Registered => "registered",
            StructuralStage::Colored => "colored",
            StructuralStage::Enveloped => "enveloped",
            StructuralStage::Committed => "committed",
            StructuralStage::Abandoned => "abandoned",
            StructuralStage::Stranded => "stranded",
        }
    }
    /// A stage that still needs the recovery reader's attention.
    fn is_open(self) -> bool {
        !matches!(
            self,
            StructuralStage::Committed | StructuralStage::Abandoned | StructuralStage::Stranded
        )
    }
}

/// The durable plan + signed material of one structural colored spend.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct StructuralSpendRecord {
    pub op_id: String,
    /// `"colored_split"` (one carrier) or `"colored_combine"` (N carriers).
    pub lane: String,
    pub stage: StructuralStage,
    pub asset_id: String,
    pub receiver_address: String,
    pub token_amount: u64,
    pub token_change: u64,
    /// Statechain ids of every carrier this spend consumes (1 for a split, N for a combine).
    pub carrier_ids: Vec<String>,
    /// `txid:vout` of every consumed carrier — the RGB sources of the change allocation.
    pub carrier_ops: Vec<String>,
    /// Derived-slot deposit tokens minted for the piece and change (diagnostics).
    pub slot_tokens: Vec<String>,
    pub piece_addr: String,
    pub change_addr: String,
    pub piece_sats: u64,
    pub change_sats: u64,
    /// True when the piece was to be handed over under a Lightning latch. A latched hand-over is
    /// NEVER replayed by recovery: the caller never learned the batch id / SE hash, so completing it
    /// would strand the piece in a lock nobody can open.
    pub latched: bool,
    // ---- present from `Signed` onward ----
    pub signed_tx: Option<String>,
    pub txid: Option<String>,
    pub piece_vout: Option<u32>,
    pub change_vout: Option<u32>,
    pub consignment: Option<String>,
    pub blinding: Option<u64>,
    // ---- present from `Registered` onward ----
    pub piece_id: Option<String>,
    pub change_id: Option<String>,
    /// BATCH lane only (`lane == "colored_batch_split"`): one entry per recipient piece, in the
    /// caller's `transfers` order. The single/combine lanes leave this empty and keep using
    /// `piece_*` / `change_*`.
    ///
    /// `#[serde(default)]` so journal rows written before this field existed still deserialize —
    /// `journal_open_entries` treats an unparseable row as a hard error (correctly), which would
    /// otherwise turn an upgrade into a wallet that cannot open.
    #[serde(default)]
    pub batch_pieces: Vec<BatchPiece>,
}

/// One recipient piece of a journalled BATCH colored split.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct BatchPiece {
    /// Statechain address this piece is handed to.
    pub recipient: String,
    /// Deposit address of the piece's derived slot (how the sub-coin is recognised at registration).
    pub addr: String,
    pub sats: u64,
    /// Token amount assigned to THIS piece (the envelope's `a`).
    pub token_amount: u64,
    // ---- present from `Signed` onward ----
    pub vout: Option<u32>,
    // ---- present from `Registered` onward ----
    pub piece_id: Option<String>,
    /// Set once this piece's key handover has actually completed. Journalled per piece so a crash
    /// part-way through an N-recipient hand-over never re-sends a piece that already left.
    #[serde(default)]
    pub handed_over: bool,
}

/// Journal lane tag of the N-recipient colored split (`batch_transfer_tokens`).
pub(crate) const LANE_BATCH_SPLIT: &str = "colored_batch_split";

/// What the recovery reader did with one journalled spend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StructuralSpendOutcome {
    /// The signed child material survived the crash and the local state was rebuilt from it. The
    /// piece exists in this wallet again; `handed_over` says whether the transfer to
    /// `receiver_address` was completed (never true for a replay — re-send it explicitly).
    Replayed { piece_id: String, handed_over: bool },
    /// The crash landed BEFORE the co-signature was issued. Nothing was lost: the carriers keep
    /// their pinned budget and the operation can simply be run again.
    Retryable,
    /// The crash landed in the irreducible window: the SE finalized the co-signature but this
    /// process never persisted the transaction. The listed carriers can never be co-signed again —
    /// their value is recoverable ONLY by unilateral exit of their own backup.
    CooperativePathLost,
}

/// One entry of the recovery reader's report.
#[derive(Clone, Debug)]
pub struct StructuralSpendRecovery {
    pub op_id: String,
    pub lane: String,
    pub carrier_ids: Vec<String>,
    pub receiver_address: String,
    pub outcome: StructuralSpendOutcome,
}

/// Classify a crash that landed at `Prepared` from the carriers' SE terminal states.
///
/// PURE so the decision is unit-testable without an SE. Terminal means the pinned budget was
/// consumed — i.e. the co-signature this operation asked for WAS issued and its transaction is the
/// thing we lost. Non-terminal on every carrier means the signature never happened.
fn classify_prepared(carriers_terminal: &[bool]) -> StructuralSpendOutcome {
    if carriers_terminal.iter().any(|t| *t) {
        StructuralSpendOutcome::CooperativePathLost
    } else {
        StructuralSpendOutcome::Retryable
    }
}

/// Create the journal table if it does not exist.
///
/// Deliberately created here rather than in `clients/libs/rust/migrations`: the journal is owned by
/// this module, and a lazily-created table keeps the SDK's storage contract in one file.
pub(crate) async fn journal_ensure_table(pool: &sqlx::Pool<sqlx::Sqlite>) -> Result<()> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS structural_spend_journal (
             op_id        TEXT PRIMARY KEY,
             wallet_name  TEXT NOT NULL,
             stage        TEXT NOT NULL,
             payload      TEXT NOT NULL,
             updated_at   INTEGER NOT NULL
         )",
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Commit a record at its current stage. Durable on return: sqlite's default `synchronous = FULL`
/// rollback journal fsyncs the commit, which is the whole point of the write-ahead ordering.
pub(crate) async fn journal_upsert(
    pool: &sqlx::Pool<sqlx::Sqlite>,
    wallet_name: &str,
    rec: &StructuralSpendRecord,
) -> Result<()> {
    journal_ensure_table(pool).await?;
    let payload = serde_json::to_string(rec)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    sqlx::query(
        "INSERT INTO structural_spend_journal (op_id, wallet_name, stage, payload, updated_at)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT(op_id) DO UPDATE SET stage = $3, payload = $4, updated_at = $5",
    )
    .bind(&rec.op_id)
    .bind(wallet_name)
    .bind(rec.stage.as_str())
    .bind(&payload)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

/// Every journal entry of this wallet that is not in a terminal stage, oldest first. THE recovery
/// reader's input.
pub(crate) async fn journal_open_entries(
    pool: &sqlx::Pool<sqlx::Sqlite>,
    wallet_name: &str,
) -> Result<Vec<StructuralSpendRecord>> {
    journal_ensure_table(pool).await?;
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT payload FROM structural_spend_journal
         WHERE wallet_name = $1 AND stage NOT IN ('committed', 'abandoned', 'stranded')
         ORDER BY updated_at ASC",
    )
    .bind(wallet_name)
    .fetch_all(pool)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for (payload,) in rows {
        // A journal row we cannot parse is NOT skipped silently — that would be the same
        // "an unreadable state looks like an empty one" failure this audit is about.
        let rec: StructuralSpendRecord = serde_json::from_str(&payload)
            .map_err(|e| anyhow!("unreadable structural-spend journal row: {e}"))?;
        if !rec.stage.is_open() {
            return Err(anyhow!(
                "structural-spend journal row {} is at terminal stage {:?} but was indexed as open",
                rec.op_id,
                rec.stage
            ));
        }
        out.push(rec);
    }
    Ok(out)
}

/// FAULT INJECTION for the crash-recovery tests, debug builds ONLY.
///
/// A durability claim can only be proved by really killing the process at the instant in question —
/// an early `return` proves nothing about what reached the disk. This gives the E2E a way to abort
/// (SIGABRT: no unwinding, no `Drop`, no flush) at a named point inside the structural spend.
/// `#[cfg(debug_assertions)]` compiles it out of release builds entirely, and even in a debug build
/// it fires only when the operator sets `UTEXO_CRASH_POINT` to the exact point name.
#[cfg(debug_assertions)]
fn crash_point(name: &str) {
    if std::env::var("UTEXO_CRASH_POINT").as_deref() == std::result::Result::Ok(name) {
        eprintln!("UTEXO_CRASH_POINT={name}: aborting to exercise structural-spend recovery");
        std::process::abort();
    }
}
#[cfg(not(debug_assertions))]
fn crash_point(_name: &str) {}

/// Carriers whose cooperative path is known to be gone (`Stranded`). They must never be selected
/// for a new colored spend: the SE will refuse to co-sign them and the attempt would only burn
/// derived slots and produce a confusing SE-side error instead of a clear local one.
pub(crate) async fn journal_stranded_carriers(
    pool: &sqlx::Pool<sqlx::Sqlite>,
    wallet_name: &str,
) -> Result<Vec<String>> {
    journal_ensure_table(pool).await?;
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT payload FROM structural_spend_journal WHERE wallet_name = $1 AND stage = 'stranded'",
    )
    .bind(wallet_name)
    .fetch_all(pool)
    .await?;
    let mut out: Vec<String> = Vec::new();
    for (payload,) in rows {
        let rec: StructuralSpendRecord = serde_json::from_str(&payload)
            .map_err(|e| anyhow!("unreadable structural-spend journal row: {e}"))?;
        out.extend(rec.carrier_ids);
    }
    Ok(out)
}

impl UtexoWallet {
    /// Open (lazily) this wallet's RGB engine. Token support requires `rgb_proxy_url` and
    /// `rgb_data_dir` in the config.
    pub(crate) async fn rgb(&self) -> Result<tokio::sync::MutexGuard<'_, Option<RgbWallet>>> {
        let mut guard = self.inner.rgb.lock().await;
        if guard.is_none() {
            let dir = self
                .inner
                .config
                .rgb_data_dir
                .clone()
                .ok_or(SdkError::TokensNotConfigured)?;
            let proxy = self
                .inner
                .config
                .rgb_proxy_url
                .clone()
                .ok_or(SdkError::TokensNotConfigured)?;
            std::fs::create_dir_all(&dir)?;
            // The RGB engine has its own BIP39 seed, persisted alongside its data.
            let mnemonic_path = std::path::Path::new(&dir).join("rgb.mnemonic");
            let mnemonic = if mnemonic_path.exists() {
                std::fs::read_to_string(&mnemonic_path)?.trim().to_string()
            } else {
                let m = RgbWallet::generate_mnemonic(&self.inner.config.network.to_string())?;
                std::fs::write(&mnemonic_path, &m)?;
                m
            };
            let wallet = tokio::task::block_in_place(|| {
                RgbWallet::open(
                    &dir,
                    &mnemonic,
                    &self.inner.config.network.to_string(),
                    &self.inner.config.electrum_url,
                    &proxy,
                )
            })?;
            *guard = Some(wallet);
        }
        Ok(guard)
    }

    /// Bitcoin address of the RGB engine's internal wallet. Issuance needs a little on-chain
    /// funding here (colorable UTXO + witness fees) — send some sats to it before `issue_token`.
    pub async fn get_token_funding_address(&self) -> Result<String> {
        let mut rgb = self.rgb().await?;
        let w = rgb.as_mut().unwrap();
        tokio::task::block_in_place(|| w.get_address())
    }

    /// Issue a new token (RGB NIA: fixed supply at issuance) and deposit the full supply onto a
    /// fresh statechain coin of this wallet. Returns the asset id (the token identifier).
    ///
    /// Prerequisite: the RGB funding address holds enough sats (see
    /// [`Self::get_token_funding_address`]); the statechain slot consumes one deposit token.
    pub async fn issue_token(
        &self,
        ticker: &str,
        name: &str,
        precision: u8,
        supply: u64,
    ) -> Result<String> {
        self.issue_token_sized(ticker, name, precision, supply, TOKEN_CARRIER_SATS).await
    }

    /// [`Self::issue_token`] with the carrier's sats chosen by the caller instead of taken from
    /// [`TOKEN_CARRIER_SATS`].
    ///
    /// **This is how a LEGACY-sized carrier is created, and that is what it is for.** A carrier
    /// funded below [`Self::colored_root_floor`] can never be laddered (`build_colored_ladder`
    /// refuses it pre-flight) — the class every pre-flip 1_500-sat token piece belongs to. The
    /// migration hatch in [`migration_hatch_verdict`] exists for exactly those coins, and a hatch
    /// with no way to construct the coins it serves cannot be tested against real SE co-signs, only
    /// asserted about. Callers who just want to issue a token want [`Self::issue_token`]; this is
    /// the knob for reproducing, and migrating, what is already in circulation.
    ///
    /// The RGB engine's own colorable UTXOs are still sized from [`TOKEN_CARRIER_SATS`]: they pay
    /// the issuance and witness fees and have nothing to do with how big the statechain carrier is.
    pub async fn issue_token_sized(
        &self,
        ticker: &str,
        name: &str,
        precision: u8,
        supply: u64,
        carrier_sats: u64,
    ) -> Result<String> {
        let deposit_sats: u64 = carrier_sats;
        // 1. Colorable UTXO + issuance in the RGB engine.
        let (asset_id, sources) = {
            let mut rgb = self.rgb().await?;
            let w = rgb.as_mut().unwrap();
            tokio::task::block_in_place(|| -> Result<(String, Vec<String>)> {
                w.create_utxos(1, (TOKEN_CARRIER_SATS * 4) as u32, 2)?;
                let asset_id = w.issue_nia(ticker, name, precision, vec![supply])?;
                let sources = w
                    .list_allocations(&asset_id)?
                    .into_iter()
                    .map(|(op, _, _)| op)
                    .collect();
                Ok((asset_id, sources))
            })?
        };

        // 2. A fresh statechain slot and the colored deposit onto it (one on-chain tx).
        let token = self.take_token().await?;
        let sc_address = mercuryrustlib::deposit::get_deposit_bitcoin_address(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            &token,
            u32::try_from(deposit_sats)?,
        )
        .await?;
        // The coin has no statechain_id yet (the deposit has not confirmed), so the funding seal is
        // keyed on the deposit address — freshly derived per slot, therefore unique per funding
        // transition. Nothing on the receiving side ever re-derives a funding blinding.
        let funding_blinding =
            TierSeal::new(sc_address.clone(), TierRole::Funding, 0, 0).blinding();
        let (txid, vout) = {
            let mut rgb = self.rgb().await?;
            let w = rgb.as_mut().unwrap();
            let (txid, vout, _consignment, signed_tx) = tokio::task::block_in_place(|| {
                w.fund_statechain(&sc_address, deposit_sats, &asset_id, supply, 2, funding_blinding)
            })?;
            use electrum_client::ElectrumApi;
            let raw = hex::decode(&signed_tx)?;
            let _ = self.inner.cc.electrum_client.transaction_broadcast_raw(&raw)?;
            (txid, vout)
        };

        // 3. Register the statechain UTXO as the asset's carrier (consumes the funding sources).
        {
            let rgb = self.rgb().await?;
            let w = rgb.as_ref().unwrap();
            tokio::task::block_in_place(|| {
                w.register_statechain(&txid, vout, deposit_sats, &asset_id, supply, &sources)
            })?;
        }
        Ok(asset_id)
    }

    /// Issue an IFA (inflatable) token. `supply` is minted now and bound to a fresh statechain
    /// coin exactly like [`Self::issue_token`]; `inflation_amounts` reserve inflation-right the
    /// issuer can later realize with [`Self::mint_tokens`]. `list_allocations` returns only the
    /// fungible allocation, so binding the supply never consumes the (InflationRight) reserve.
    /// Returns the asset id.
    pub async fn issue_inflatable_token(
        &self,
        ticker: &str,
        name: &str,
        precision: u8,
        supply: u64,
        inflation_amounts: Vec<u64>,
    ) -> Result<String> {
        self.issue_inflatable_token_sized(
            ticker,
            name,
            precision,
            supply,
            inflation_amounts,
            TOKEN_CARRIER_SATS,
        )
        .await
    }

    /// [`Self::issue_inflatable_token`] with the carrier's sats chosen by the caller — the IFA
    /// sibling of [`Self::issue_token_sized`], and see that method for why the knob exists.
    pub async fn issue_inflatable_token_sized(
        &self,
        ticker: &str,
        name: &str,
        precision: u8,
        supply: u64,
        inflation_amounts: Vec<u64>,
        carrier_sats: u64,
    ) -> Result<String> {
        let deposit_sats: u64 = carrier_sats;
        let (asset_id, sources) = {
            let mut rgb = self.rgb().await?;
            let w = rgb.as_mut().unwrap();
            let inflation = inflation_amounts.clone();
            tokio::task::block_in_place(move || -> Result<(String, Vec<String>)> {
                // One colorable UTXO per allocation (the fungible supply + each inflation-right)
                // plus a spare for the fund/witness txs; max_allocations_per_utxo is 1.
                let utxos = (inflation.len() as u8).saturating_add(2);
                w.create_utxos(utxos, (TOKEN_CARRIER_SATS * 4) as u32, 2)?;
                let asset_id = w.issue_ifa(ticker, name, precision, vec![supply], inflation)?;
                let sources = w
                    .list_allocations(&asset_id)?
                    .into_iter()
                    .map(|(op, _, _)| op)
                    .collect();
                Ok((asset_id, sources))
            })?
        };
        self.bind_engine_supply(&asset_id, supply, deposit_sats, &sources).await?;
        Ok(asset_id)
    }

    /// Realize `inflation_amounts` of an IFA's inflation-right as new supply and bind it to a fresh
    /// statechain coin. **This broadcasts an on-chain tx in the RGB engine** (inflation is a
    /// contract state transition — there is no off-chain variant); the newly-minted allocation is
    /// then bound like issuance. Returns `(inflate_txid, minted_total)`.
    ///
    /// Requires the inflate tx to confirm before the minted allocation is spendable — on regtest
    /// the caller must be mining (e.g. a background miner); in production real blocks provide it.
    pub async fn mint_tokens(
        &self,
        asset_id: &str,
        inflation_amounts: Vec<u64>,
    ) -> Result<(String, u64)> {
        self.mint_tokens_sized(asset_id, inflation_amounts, TOKEN_CARRIER_SATS).await
    }

    /// [`Self::mint_tokens`] with the carrier's sats chosen by the caller — the mint sibling of
    /// [`Self::issue_token_sized`], and see that method for why the knob exists.
    pub async fn mint_tokens_sized(
        &self,
        asset_id: &str,
        inflation_amounts: Vec<u64>,
        carrier_sats: u64,
    ) -> Result<(String, u64)> {
        let deposit_sats: u64 = carrier_sats;
        // Snapshot the allocations that already exist (incl. registered statechain coins, which
        // list_allocations reports as colorable UTXOs) so we can isolate ONLY the freshly-minted
        // one afterwards — otherwise binding would wrongly consume already-bound supply.
        let before: std::collections::HashSet<String> = {
            let mut rgb = self.rgb().await?;
            let w = rgb.as_mut().unwrap();
            tokio::task::block_in_place(|| w.list_allocations(asset_id))?
                .into_iter()
                .map(|(op, _, _)| op)
                .collect()
        };

        // 1. Inflate in the engine (on-chain broadcast). Ensure a colorable UTXO exists first.
        let (inflate_txid, minted) = {
            let mut rgb = self.rgb().await?;
            let w = rgb.as_mut().unwrap();
            let inflation = inflation_amounts.clone();
            tokio::task::block_in_place(move || -> Result<(String, u64)> {
                let _ = w.create_utxos(2, (TOKEN_CARRIER_SATS * 4) as u32, 2);
                w.inflate(asset_id, inflation, 2)
            })?
        };

        // 2. Wait for the inflate to confirm and the NEW (post-snapshot) fungible allocation to
        //    settle; use only that as the bind source.
        let mut sources: Vec<String> = Vec::new();
        for _ in 0..90 {
            let allocs = {
                let mut rgb = self.rgb().await?;
                let w = rgb.as_mut().unwrap();
                tokio::task::block_in_place(|| -> Result<Vec<(String, u64, bool)>> {
                    let _ = w.refresh(Some(asset_id.to_string()));
                    w.list_allocations(asset_id)
                })?
            };
            let fresh: Vec<(String, u64)> = allocs
                .into_iter()
                .filter(|(op, _, s)| *s && !before.contains(op))
                .map(|(op, a, _)| (op, a))
                .collect();
            let settled: u64 = fresh.iter().map(|(_, a)| *a).sum();
            if settled >= minted {
                sources = fresh.into_iter().map(|(op, _)| op).collect();
                break;
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
        if sources.is_empty() {
            return Err(anyhow!("minted allocation for {asset_id} did not settle (is the chain advancing?)"));
        }

        // 3. Bind the minted supply to a fresh statechain coin.
        self.bind_engine_supply(asset_id, minted, deposit_sats, &sources).await?;
        Ok((inflate_txid, minted))
    }

    /// Burn `amount` of an asset's FREE (engine-held) balance. **On-chain** in the RGB engine.
    /// Statechain-bound supply must be exited back into the engine first (documented limitation).
    /// Returns the burn txid.
    pub async fn burn_tokens(&self, asset_id: &str, amount: u64) -> Result<String> {
        let mut rgb = self.rgb().await?;
        let w = rgb.as_mut().unwrap();
        tokio::task::block_in_place(|| w.burn(asset_id, amount, 2))
    }

    /// Bind an engine-held fungible allocation of `amount` to a fresh statechain coin: fund the
    /// coin colored (one on-chain tx) and register it as the carrier. Shared by issuance and mint.
    async fn bind_engine_supply(
        &self,
        asset_id: &str,
        amount: u64,
        deposit_sats: u64,
        sources: &[String],
    ) -> Result<(String, u32)> {
        let token = self.take_token().await?;
        let sc_address = mercuryrustlib::deposit::get_deposit_bitcoin_address(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            &token,
            u32::try_from(deposit_sats)?,
        )
        .await?;
        // Same as `issue_token`: no statechain_id yet, so the fresh deposit address is the seal id.
        let funding_blinding =
            TierSeal::new(sc_address.clone(), TierRole::Funding, 0, 0).blinding();
        let (txid, vout) = {
            let mut rgb = self.rgb().await?;
            let w = rgb.as_mut().unwrap();
            let (txid, vout, _c, signed_tx) = tokio::task::block_in_place(|| {
                w.fund_statechain(&sc_address, deposit_sats, asset_id, amount, 2, funding_blinding)
            })?;
            use electrum_client::ElectrumApi;
            let raw = hex::decode(&signed_tx)?;
            let _ = self.inner.cc.electrum_client.transaction_broadcast_raw(&raw)?;
            (txid, vout)
        };
        {
            let rgb = self.rgb().await?;
            let w = rgb.as_ref().unwrap();
            tokio::task::block_in_place(|| {
                w.register_statechain(&txid, vout, deposit_sats, asset_id, amount, sources)
            })?;
        }
        Ok((txid, vout))
    }

    /// L1 (Bitcoin) address for token operations — where to send sats to fund issuance/mint.
    /// Alias of [`Self::get_token_funding_address`]; mirrors Spark's `getTokenL1Address`.
    pub async fn get_token_l1_address(&self) -> Result<String> {
        self.get_token_funding_address().await
    }

    /// Transaction history for a token contract (Spark's `queryTokenTransactions`):
    /// `(kind, status, amount, txid)` per transfer known to the RGB engine.
    pub async fn query_token_transactions(&self, asset_id: &str) -> Result<Vec<crate::types::TokenTx>> {
        if self.inner.config.rgb_data_dir.is_none() || self.inner.config.rgb_proxy_url.is_none() {
            return Err(SdkError::TokensNotConfigured.into());
        }
        let rgb = self.rgb().await?;
        let w = rgb.as_ref().unwrap();
        let rows = tokio::task::block_in_place(|| w.transfers(asset_id))?;
        Ok(rows
            .into_iter()
            .map(|(kind, status, amount, txid)| crate::types::TokenTx { kind, status, amount, txid })
            .collect())
    }

    /// Token balances across this wallet's registered coins.
    pub async fn get_token_balances(&self) -> Result<Vec<TokenBalance>> {
        if self.inner.config.rgb_data_dir.is_none() || self.inner.config.rgb_proxy_url.is_none() {
            return Ok(vec![]);
        }
        let mut rgb = self.rgb().await?;
        let w = rgb.as_mut().unwrap();
        tokio::task::block_in_place(|| -> Result<Vec<TokenBalance>> {
            let mut out = Vec::new();
            for (asset_id, ticker, name, precision) in w.list_assets()? {
                let (settled, future, _spendable) = w.balance(&asset_id)?;
                out.push(TokenBalance {
                    asset_id,
                    ticker: Some(ticker),
                    name: Some(name),
                    precision,
                    balance: settled,
                    total: future,
                });
            }
            Ok(out)
        })
    }

    /// Outpoints (`"txid:vout"`) of every coin that currently carries an RGB token allocation.
    /// BTC coin-selection and the spendable-BTC balance MUST exclude these: spending a token
    /// carrier as ordinary sats destroys its RGB allocation with no warning (review H2). Empty when
    /// token support is not configured (the common pure-BTC wallet — no RGB engine is opened).
    pub(crate) async fn token_carrier_outpoints(
        &self,
    ) -> Result<std::collections::HashSet<String>> {
        if self.inner.config.rgb_data_dir.is_none() || self.inner.config.rgb_proxy_url.is_none() {
            return Ok(std::collections::HashSet::new());
        }
        let mut rgb = self.rgb().await?;
        let w = rgb.as_mut().unwrap();
        tokio::task::block_in_place(|| -> Result<std::collections::HashSet<String>> {
            let mut out = std::collections::HashSet::new();
            for (asset_id, _ticker, _name, _precision) in w.list_assets()? {
                for (outpoint, _amt, _settled) in w.list_allocations(&asset_id)? {
                    out.insert(outpoint);
                }
            }
            Ok(out)
        })
    }

    /// Where an asset's allocations actually sit: `(outpoint, amount)` per carrier UTXO.
    ///
    /// Distinct from [`Self::get_token_balances`] on purpose. A balance is an aggregate computed
    /// from rgb-lib's sqlite tables and stays confidently wrong when the RGB stock has been
    /// invalidated underneath it (E7); this lists the actual per-outpoint bindings, which is what a
    /// caller needs to answer "is the allocation still on the coin I think it is?".
    pub async fn list_token_allocations(&self, asset_id: &str) -> Result<Vec<(String, u64)>> {
        if self.inner.config.rgb_data_dir.is_none() || self.inner.config.rgb_proxy_url.is_none() {
            return Ok(vec![]);
        }
        let mut rgb = self.rgb().await?;
        let w = rgb.as_mut().unwrap();
        tokio::task::block_in_place(|| -> Result<Vec<(String, u64)>> {
            Ok(w.list_allocations(asset_id)?
                .into_iter()
                .map(|(op, amt, _settled)| (op, amt))
                .collect())
        })
    }

    /// [CTES-R] Every **booked** RGB allocation this wallet holds, keyed by carrier outpoint:
    /// `"txid:vout" -> (contract_id, amount)`.
    ///
    /// This is [`Self::token_carrier_outpoints`] with the two facts it throws away kept — the same
    /// `list_assets() × list_allocations()` walk, nothing more expensive. Those two facts are the
    /// entire "how do I know what to colour" problem for a coloured ladder: a CTES-R tier needs a
    /// contract id and an amount, and both are already in hand.
    ///
    /// **Multi-allocation carriers are reported, not hidden.** An outpoint holding allocations of
    /// two different contracts (or two fungible entries of one) maps to `None`, so the caller can
    /// tell "one allocation, colourable" from "several, no single-transition tier shape yet" and
    /// fail CLOSED on the latter rather than silently colouring one asset and destroying the other.
    pub(crate) async fn token_carrier_allocations(
        &self,
    ) -> Result<std::collections::HashMap<String, Option<(String, u64)>>> {
        let mut out: std::collections::HashMap<String, Option<(String, u64)>> =
            std::collections::HashMap::new();
        if self.inner.config.rgb_data_dir.is_none() || self.inner.config.rgb_proxy_url.is_none() {
            return Ok(out);
        }
        let mut rgb = self.rgb().await?;
        let w = rgb.as_mut().unwrap();
        tokio::task::block_in_place(|| -> Result<()> {
            for (asset_id, _ticker, _name, _precision) in w.list_assets()? {
                for (outpoint, amt, _settled) in w.list_allocations(&asset_id)? {
                    // Second allocation on the same outpoint ⟹ ambiguous ⟹ `None` (fail closed).
                    out.entry(outpoint)
                        .and_modify(|e| *e = None)
                        .or_insert(Some((asset_id.clone(), amt)));
                }
            }
            Ok(())
        })?;
        Ok(out)
    }

    /// Outpoints of CONFIRMED coins that carry an incoming RGB **consignment** in their backup rows
    /// but whose allocation is NOT yet booked in the engine — a *pending* token carrier (external
    /// review finding 5). A transient RGB-proxy/indexer error during `accept_incoming_tokens` leaves
    /// such a coin CONFIRMED yet absent from [`Self::token_carrier_outpoints`] (which lists only
    /// booked allocations), so until the retry loop books it, plain-BTC selection would happily
    /// spend it and DESTROY the allocation — including `auto_refresh_due`, which would re-anchor it.
    /// These outpoints must be quarantined from every plain-BTC path exactly like booked carriers.
    /// **[CTES-R] Every statechain id in this wallet whose exit ladder is COLOURED.**
    ///
    /// One read of the `tesr-` rows, shared by the two callers that must agree about it: the
    /// carrier quarantine below (a coloured carrier must never reach plain-BTC selection) and
    /// `unilateral_exit` (a coloured carrier is the ONLY carrier that may be exited, because it is
    /// the only one whose tiers carry RGB transitions).
    ///
    /// **Fails CLOSED, and the direction is the whole point.** An unreadable ladder table yields an
    /// `Err`, never an empty set. For the quarantine an empty set means "no carriers" and would let
    /// a carrier be spent as sats; for the exit an empty set means "no coloured ladder" and merely
    /// refuses an exit. Both are the safe direction only if neither is silently defaulted, so the
    /// error is propagated and each caller decides.
    ///
    /// A `tesr-` row that will not DESERIALIZE has **no verdict either way**, and the two callers
    /// need OPPOSITE defaults for that case — which is why the census below reports it separately
    /// instead of dropping it. `unilateral_exit` must refuse (an unreadable bundle is not evidence
    /// that a coloured walk exists), but the quarantine must ADMIT it: dropping an unparseable row
    /// there means "not a carrier", and a coloured carrier that is not yet booked has no other arm
    /// covering it, so plain-BTC selection would spend it and DESTROY the allocation. Silently
    /// dropping served the exit and betrayed the quarantine; see [`Self::maybe_colored_ladder_sids`].
    pub(crate) async fn colored_ladder_sids(&self) -> Result<std::collections::HashSet<String>> {
        Ok(self.tesr_colour_census().await?.0)
    }

    /// The quarantine's half of [`Self::tesr_colour_census`]: every sid this wallet cannot PROVE is
    /// un-coloured — the coloured ones plus the ones whose `tesr-` row would not deserialize.
    ///
    /// The direction is the point. This set only ever removes coins from plain-BTC selection, so an
    /// unparseable row costs a refused sat-spend (recoverable, and the coin's ladder is unusable
    /// anyway — `mercuryrustlib::tesr::load` parses the same row) instead of a destroyed RGB
    /// allocation (irreversible). "I could not tell whether this coin is a carrier" must never be
    /// spelled the same way as "it is not".
    pub(crate) async fn maybe_colored_ladder_sids(
        &self,
    ) -> Result<std::collections::HashSet<String>> {
        let (mut colored, unreadable) = self.tesr_colour_census().await?;
        colored.extend(unreadable);
        Ok(colored)
    }

    /// One read of the `tesr-` rows, classified into `(PROVEN coloured, UNREADABLE)`.
    ///
    /// An unreadable ladder TABLE is still an `Err` for both callers — a carrier set assembled from
    /// failures is a confident answer about which coins are safe to spend, built out of not knowing.
    /// What this splits out is the narrower case of a table that read fine and a single ROW that did
    /// not parse, which is a real answer about the table and no answer at all about that coin.
    async fn tesr_colour_census(
        &self,
    ) -> Result<(
        std::collections::HashSet<String>,
        std::collections::HashSet<String>,
    )> {
        let rows = mercuryrustlib::sqlite_manager::get_all_backup_txs(
            &self.inner.cc.pool,
            &self.inner.config.wallet_name,
        )
        .await
        .map_err(|e| {
            anyhow!(
                "cannot enumerate token carriers: the exit-ladder rows could not be read ({e}) \
                 — refusing to report a carrier set built from an unreadable database"
            )
        })?;
        Ok(classify_tesr_rows(rows))
    }

    /// **[CTES-R] The same set for adopted SPLIT CHILDREN — sids whose `ctesr-` bundle is COLOURED.**
    ///
    /// A coloured child is a carrier (its allocation sits on `SP.out[j]`, which
    /// `token_carrier_outpoints` reports), and it has a five-tier pre-signed walk that MOVES that
    /// allocation to the owner's own key — but it has no `tesr-` row, so
    /// [`Self::colored_ladder_sids`] cannot see it. Without this set `unilateral_exit` refuses every
    /// coloured child with "its ladder is not COLOURED", i.e. the one carrier class CTES-R exists to
    /// make exitable would be the one class that cannot exit.
    ///
    /// Same fail-closed reading as its sibling: an unreadable table is an `Err`, and a `ctesr-` row
    /// that will not deserialize counts as NOT coloured, so that child stays refused.
    ///
    /// **[CATS/V4] It also covers COLOURED SPINE TIPS (`spinetip-` rows).** A tip is a carrier by
    /// exactly the same argument — its allocation sits on the un-broadcast `SP.out[K]`, which
    /// `token_carrier_outpoints` reports — and it has a pre-signed walk that MOVES that allocation
    /// to the owner's own key, one tier shorter than a child's. This set is what `unilateral_exit`
    /// consults to decide which carriers may walk at all, so a missed prefix here is not a missed
    /// optimisation: the sender's own coloured change becomes the one carrier class that can never
    /// exit, refused by the very guard CTES-R opened.
    pub(crate) async fn colored_child_sids(&self) -> Result<std::collections::HashSet<String>> {
        Ok(mercuryrustlib::sqlite_manager::get_all_backup_txs(
            &self.inner.cc.pool,
            &self.inner.config.wallet_name,
        )
        .await
        .map_err(|e| {
            anyhow!(
                "cannot enumerate coloured split children: the child-bundle rows could not be read \
                 ({e}) — refusing to report a carrier set built from an unreadable database"
            )
        })?
        .into_iter()
        .filter_map(|(key, json)| {
            if let Some(sid) = key.strip_prefix("ctesr-") {
                let cb: mercuryrustlib::tesr::ChildTesrBundle = serde_json::from_str(&json).ok()?;
                return cb.is_colored().then_some(sid.to_string());
            }
            let sid = key.strip_prefix(mercuryrustlib::tesr::SPINE_TIP_KEY_PREFIX)?.to_string();
            let tip: mercuryrustlib::tesr::SpineTipBundle = serde_json::from_str(&json).ok()?;
            tip.is_colored().then_some(sid)
        })
        .collect())
    }

    pub(crate) async fn consignment_bearing_outpoints(
        &self,
    ) -> Result<std::collections::HashSet<String>> {
        let mut out = std::collections::HashSet::new();
        if self.inner.config.rgb_data_dir.is_none() || self.inner.config.rgb_proxy_url.is_none() {
            return Ok(out);
        }
        // [CTES-R] A conveyed COLOURED ladder carries its RGB half in the `tesr-` bundle, NOT in a
        // backup-row consignment envelope — so between the claim and the booking it would be invisible
        // to the loop below, and plain-BTC selection would happily spend the carrier and destroy the
        // allocation. Read the ladder rows ONCE (not per coin) and quarantine every coloured one.
        //
        // NOT best-effort: an unreadable ladder table must stop the caller exactly as an unreadable
        // backup row does, for the same reason — a carrier set assembled from failures is a confident
        // answer about which coins are safe to spend, built out of not knowing.
        //
        // MAYBE-coloured, not PROVEN-coloured: this is the quarantine, so a `tesr-` row that will
        // not deserialize must land IN the set. Dropping it (what the old `.ok()?` did) spelled
        // "I could not read this coin's bundle" exactly like "this coin is not a carrier", and the
        // coin then flowed into plain-BTC selection where spending it destroys the allocation.
        // `unilateral_exit` keeps the opposite default via `colored_ladder_sids`.
        let colored_sids = self.maybe_colored_ladder_sids().await?;
        let record = self.record().await?;
        for coin in record
            .coins
            .iter()
            .filter(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0)
        {
            if let (Some(id), Some(outpoint)) =
                (coin.statechain_id.as_deref(), crate::wallet::coin_outpoint(coin))
            {
                if colored_sids.contains(id) {
                    out.insert(outpoint);
                }
            }
            // A coin with no statechain id or no outpoint cannot be referenced by BTC coin
            // selection at all (selection keys on the outpoint), so skipping it removes no
            // protection — unlike the DB reads below, whose absence is a real answer about a real
            // coin.
            let Some(id) = coin.statechain_id.as_deref() else { continue };
            let Some(outpoint) = crate::wallet::coin_outpoint(coin) else { continue };
            // A coin whose consignment was PERMANENTLY rejected (griefer's garbage, marked by
            // claim()) is NOT quarantined — its sats are ordinary BTC the owner may spend.
            //
            // This read is NOT allowed to fail silently either, even though its swallow leaned the
            // safe way (a DB error read as "not rejected" over-quarantines). The enumeration is the
            // input to every carrier check downstream, so an unreadable DB must stop the caller, not
            // hand it a confident-looking answer assembled from failures.
            let rejected = read_backup_rows(
                &self.inner.cc.pool,
                &self.inner.config.wallet_name,
                &format!("token-rejected-{id}"),
            )
            .await
            .map_err(|e| {
                anyhow!(
                    "cannot enumerate token carriers: the rejection marker of {id} could not be \
                     read ({e}) — refusing to report a carrier set built from an unreadable database"
                )
            })?
            .is_some_and(|v| !v.is_empty());
            if rejected {
                continue;
            }
            // THE load-bearing read. Its old `.unwrap_or_default()` turned a DB read failure into an
            // EMPTY backup list, i.e. into "this coin bears no consignment", i.e. into "not a
            // carrier" — and the coin then flowed into plain-BTC selection, where spending it
            // DESTROYS the RGB allocation with no warning. A database that cannot be read is not
            // evidence that a coin is safe to spend.
            let Some(backups) = read_backup_rows(
                &self.inner.cc.pool,
                &self.inner.config.wallet_name,
                id,
            )
            .await
            .map_err(|e| {
                anyhow!(
                    "cannot enumerate token carriers: the backup rows of {id} could not be read \
                     ({e}) — refusing to report a carrier set built from an unreadable database"
                )
            })?
            else {
                // Genuinely no backup row for this coin (a real answer, not a failed read): it
                // carries no consignment, so it is not a pending carrier.
                continue;
            };
            if backups.iter().any(|b| b.rgb_consignment.is_some()) {
                out.insert(outpoint);
            }
        }
        Ok(out)
    }

    /// The full set of outpoints that must NEVER be spent as plain BTC: booked token carriers UNION
    /// pending (consignment-bearing, not-yet-booked) carriers. Every plain-BTC selection/spend path
    /// (transfer, split, combine parent-pick, withdraw, unilateral exit, auto-refresh) uses this so
    /// a token coin — booked or still settling — is never swept as sats (review H2 + finding 5).
    pub(crate) async fn unspendable_as_btc_outpoints(
        &self,
    ) -> Result<std::collections::HashSet<String>> {
        let mut set = self.token_carrier_outpoints().await?;
        set.extend(self.consignment_bearing_outpoints().await?);
        Ok(set)
    }

    // ---------------------------------------------------------------------------------------
    // F7 — journal plumbing + the recovery reader.
    // ---------------------------------------------------------------------------------------

    /// A carrier's spend generation: the number of backup rows it has accumulated.
    ///
    /// Load-bearing twice over. It is the `tier_index` of the RGB seal (`create_colored_split_tx`
    /// gets `generation + 1`) and it is an input to the seal blinding, so it must be the REAL count.
    /// The three call sites used to read it as `.map(len).unwrap_or(0)`: an unreadable database
    /// silently produced generation 0, which re-derives a seal this carrier may already have used —
    /// and two RGB transitions over the same parent with the same blinding collapse to one BundleId,
    /// after which the loser's consignment embeds the rival's witness and the allocation is simply
    /// unclaimable off-chain (the collision this module's `TierSeal` exists to prevent). Zero is a
    /// real generation, never a stand-in for "could not read".
    /// **[CTES-R] Is a coin's coloured ladder still alive?** The health check `CTESR-GATE.md` §3.3
    /// mandates, and the ONLY kind that works.
    ///
    /// Never assert carrier liveness on `get_asset_balance` or `list_unspents`: E7 measured both
    /// reporting `settled/future/spendable = 1000` with the RGB **stock** at zero, because rgb-lib's
    /// balance is computed from its sqlite tables and a stock invalidation touches neither. A
    /// monitoring alarm built on the balance would never fire.
    ///
    /// So this probes the stock, through the fork's `OffchainResolver` with the ladder's OWN txid
    /// list — never the plain blockchain resolver, which would report every deliberately-un-broadcast
    /// tier as `Unresolved`, archive it, and recursively invalidate the ladder with no error and no
    /// repair path (E7).
    ///
    /// Returns `(contract_id, amount_assigned_to_the_final_state, tier_txids, detail)`. `Err` for a
    /// coin with no ladder or a plain one; `Err` too when the consignment does not validate — a
    /// coloured ladder that cannot be validated off-chain is not "probably fine".
    pub async fn colored_ladder_health(
        &self,
        statechain_id: &str,
    ) -> Result<(String, u64, Vec<String>, Option<String>)> {
        let bundle =
            mercuryrustlib::tesr::load(&self.inner.cc, &self.inner.config.wallet_name, statechain_id)
                .await?
                .ok_or_else(|| anyhow!("statechain id {statechain_id} has no ladder"))?;
        let rgb_half = bundle
            .rgb
            .clone()
            .ok_or_else(|| anyhow!("statechain id {statechain_id} has a PLAIN ladder"))?;
        let tiers = bundle.exit_tiers();
        if rgb_half.consignments.len() != tiers.len() {
            return Err(anyhow!(
                "coloured ladder of {statechain_id} carries {} consignments for {} tiers",
                rgb_half.consignments.len(),
                tiers.len()
            ));
        }
        let txids: Vec<String> = tiers.iter().map(|t| t.txid.clone()).collect();
        let leaf = rgb_half.consignments.last().cloned().unwrap();
        let final_state = bundle.current().state.clone();
        let mut rgb = self.rgb().await?;
        let w = rgb.as_mut().ok_or_else(|| anyhow!("no RGB engine configured"))?;
        tokio::task::block_in_place(|| -> Result<(String, u64, Vec<String>, Option<String>)> {
            let (verdict, detail, contract) = w.validate_offchain_chain_info(&leaf, &txids)?;
            if verdict != ValidationVerdict::Valid {
                return Err(anyhow!(
                    "the coloured ladder of {statechain_id} does not validate off-chain \
                     ({verdict:?}): {}",
                    detail.unwrap_or_default()
                ));
            }
            let assigned = w.accept_offchain_amount(
                &leaf,
                &txids,
                &final_state.txid,
                final_state.payload_vout,
            )?;
            Ok((
                contract.unwrap_or_else(|| rgb_half.contract_id.clone()),
                assigned,
                txids,
                detail,
            ))
        })
    }

    /// **[CTES-R] The on-chain survival proof of a unilateral coloured exit.**
    ///
    /// Validates the ladder's LEAF consignment with an **EMPTY** off-chain witness set. That empty
    /// set is the whole assertion, and it is the one thing stash state cannot fake: with no ids in
    /// `offchain_witness_ids` the fork's `OffchainResolver` falls through to the plain indexer for
    /// EVERY witness, so `valid = true` is achievable only if every tier that ever carried the
    /// allocation is genuinely MINED. Before the exit walk this same call fails; after it succeeds.
    ///
    /// Contrast [`Self::colored_ladder_health`], which passes the ladder's own txids and therefore
    /// answers the *off-chain* question ("is the un-broadcast chain still coherent?"). Neither is a
    /// substitute for the other, and neither is `get_asset_balance`, which is blind to both (E7).
    ///
    /// Returns `(contract_id, amount_assigned_to_the_final_state, detail)`.
    pub async fn colored_exit_proof(&self, statechain_id: &str) -> Result<(String, u64, Option<String>)> {
        let bundle =
            mercuryrustlib::tesr::load(&self.inner.cc, &self.inner.config.wallet_name, statechain_id)
                .await?
                .ok_or_else(|| anyhow!("statechain id {statechain_id} has no ladder"))?;
        let rgb_half = bundle
            .rgb
            .clone()
            .ok_or_else(|| anyhow!("statechain id {statechain_id} has a PLAIN ladder"))?;
        let leaf = rgb_half
            .consignments
            .last()
            .cloned()
            .ok_or_else(|| anyhow!("the coloured ladder of {statechain_id} carries no consignment"))?;
        let final_state = bundle.current().state.clone();
        let mut rgb = self.rgb().await?;
        let w = rgb.as_mut().ok_or_else(|| anyhow!("no RGB engine configured"))?;
        tokio::task::block_in_place(|| -> Result<(String, u64, Option<String>)> {
            // THE EMPTY SET. Not a stylistic choice — see the doc comment.
            let (verdict, detail, contract) = w.validate_offchain_chain_info(&leaf, &[])?;
            if verdict != ValidationVerdict::Valid {
                return Err(anyhow!(
                    "the coloured ladder of {statechain_id} does NOT validate against the chain \
                     alone ({verdict:?}): {} — at least one tier is still un-mined, so the exit \
                     walk did not land",
                    detail.unwrap_or_default()
                ));
            }
            let assigned =
                w.accept_offchain_amount(&leaf, &[], &final_state.txid, final_state.payload_vout)?;
            Ok((contract.unwrap_or_else(|| rgb_half.contract_id.clone()), assigned, detail))
        })
    }

    /// **[CTES-R] The §3.3 read-only STOCK probe at a coloured ladder's exit tip.**
    ///
    /// Asks the stash — not the balance — whether `amount` of the ladder's contract can still be
    /// spent out of the final state's payload output, the outpoint that pays the owner's own exit
    /// key. `Ok(())` means the allocation is alive there; `Err` carries rgb-lib's reason, which for
    /// a dead stash is `InvalidColoringInfo { … greater than available (0) }`.
    ///
    /// Nothing is consumed and no witness is resolved: it runs rgb-lib's `color_psbt`, not
    /// `color_psbt_and_consume`. `get_asset_balance` and `list_unspents` are BLIND to the failure
    /// this detects — E7 measured both reporting a full settled spendable balance over a stock at
    /// zero — so this is the probe every CTES-R invariant test and ops alarm must use.
    pub async fn probe_colored_tip(&self, statechain_id: &str, amount: u64) -> Result<()> {
        let bundle =
            mercuryrustlib::tesr::load(&self.inner.cc, &self.inner.config.wallet_name, statechain_id)
                .await?
                .ok_or_else(|| anyhow!("statechain id {statechain_id} has no ladder"))?;
        let rgb_half = bundle
            .rgb
            .clone()
            .ok_or_else(|| anyhow!("statechain id {statechain_id} has a PLAIN ladder"))?;
        let state = bundle.current().state.clone();
        let tx: bitcoin::Transaction =
            bitcoin::consensus::encode::deserialize(&hex::decode(&state.signed_tx)?)?;
        let out = tx
            .output
            .get(state.payload_vout as usize)
            .ok_or_else(|| anyhow!("the final state has no output at its declared payload vout"))?;
        let spk_hex = hex::encode(out.script_pubkey.as_bytes());
        let payee = bundle.owner_exit_address.clone();
        let network = bundle.network.clone();
        let mut rgb = self.rgb().await?;
        let w = rgb.as_mut().ok_or_else(|| anyhow!("no RGB engine configured"))?;
        tokio::task::block_in_place(|| {
            mercuryrustlib::rgb::probe_allocation(
                w,
                &rgb_half.contract_id,
                &state.txid,
                state.payload_vout,
                out.value,
                &spk_hex,
                &payee,
                &network,
                amount,
            )
        })
    }

    /// **The §3.3 read-only STOCK probe at a FLAT carrier's funding output `F`.**
    ///
    /// [`Self::probe_colored_tip`]'s sibling for a carrier that has no ladder to have a tip — which
    /// includes every carrier below [`Self::colored_root_floor`], i.e. every coin the migration
    /// hatch exists for. Same question, same mechanism: ask the STASH (rgb-lib's `color_psbt`, not
    /// `color_psbt_and_consume`, and never `get_asset_balance`, which E7 measured reporting a full
    /// settled balance over a dead stock) whether `amount` of `asset_id` can still be spent out of
    /// this coin's confirmed funding outpoint. `Ok(())` means the allocation is alive there.
    ///
    /// Nothing is broadcast, nothing is consumed and no witness is resolved.
    pub async fn probe_carrier_funding(
        &self,
        statechain_id: &str,
        asset_id: &str,
        amount: u64,
    ) -> Result<()> {
        use electrum_client::ElectrumApi;
        let coin = self.confirmed_coin(statechain_id).await?;
        let txid = coin
            .utxo_txid
            .clone()
            .ok_or_else(|| anyhow!("carrier {statechain_id} has no funding txid"))?;
        let vout = coin
            .utxo_vout
            .ok_or_else(|| anyhow!("carrier {statechain_id} has no funding vout"))?;
        // The prevout is read FROM THE CHAIN, not from the coin record: the probe has to colour a
        // real output, and the record's `amount` is the wallet's view of it rather than the chain's.
        let tx = self
            .inner
            .cc
            .electrum_client
            .transaction_get(&txid.parse::<bitcoin::Txid>()?)
            .map_err(|e| {
                anyhow!(
                    "carrier {statechain_id}'s funding {txid} is not on chain ({e}) — there is no \
                     confirmed outpoint to probe"
                )
            })?;
        let out = tx
            .output
            .get(vout as usize)
            .ok_or_else(|| anyhow!("carrier {statechain_id}'s funding tx has no output {vout}"))?;
        let spk_hex = hex::encode(out.script_pubkey.as_bytes());
        let payee = coin.backup_address.clone();
        let network = self.inner.config.network.to_string();
        let mut rgb = self.rgb().await?;
        let w = rgb.as_mut().ok_or_else(|| anyhow!("no RGB engine configured"))?;
        tokio::task::block_in_place(|| {
            mercuryrustlib::rgb::probe_allocation(
                w, asset_id, &txid, vout, out.value, &spk_hex, &payee, &network, amount,
            )
        })
    }

    /// **[CTES-R] Renew a coloured ladder off-chain.** The coloured sibling of `tesr::renew_auto`.
    ///
    /// The new extension `X_{m+1}` is a RIVAL of `X_m` over the trigger's payload output — the exact
    /// case a shared blinding collapses (CTESR-GATE §2.2). What separates them is the seal rung,
    /// which folds in the renewal counter AND the (strictly lower) CSV; the receiver re-derives both
    /// from the bundle it is handed. Returns the new renewal counter `m`.
    ///
    /// Two phases, and the split is load-bearing: colouring holds the RGB engine (whose resolver is
    /// `!Sync`) and co-signing `await`s the SE, so the two may never overlap.
    pub async fn renew_colored_ladder(&self, statechain_id: &str) -> Result<u32> {
        self.renew_colored_ladder_ex(statechain_id, None).await
    }

    /// [`Self::renew_colored_ladder`] with hand-picked CSVs instead of the network's canonical
    /// cadence. The new extension CSV must still be strictly lower than the one it replaces — that
    /// is the maturity race, and it is also what separates the two rival transitions over the
    /// trigger's payload output.
    pub async fn renew_colored_ladder_with(
        &self,
        statechain_id: &str,
        csv_e: u16,
        csv_d: u16,
    ) -> Result<u32> {
        self.renew_colored_ladder_ex(statechain_id, Some((csv_e, csv_d))).await
    }

    async fn renew_colored_ladder_ex(
        &self,
        statechain_id: &str,
        csvs: Option<(u16, u16)>,
    ) -> Result<u32> {
        let mut bundle = mercuryrustlib::tesr::load(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            statechain_id,
        )
        .await?
        .ok_or_else(|| anyhow!("statechain id {statechain_id} has no ladder"))?;
        if !bundle.is_colored() {
            return Err(anyhow!(
                "statechain id {statechain_id} has a PLAIN ladder — use the plain renewal path"
            ));
        }
        let mut coin = self.confirmed_coin(statechain_id).await?;

        let draft = {
            let mut rgb = self.rgb().await?;
            let w = rgb.as_mut().ok_or_else(|| anyhow!("no RGB engine configured"))?;
            tokio::task::block_in_place(|| match csvs {
                Some((csv_e, csv_d)) => {
                    mercuryrustlib::tesr::build_colored_renewal(w, &bundle, csv_e, csv_d)
                }
                None => mercuryrustlib::tesr::build_colored_renewal_auto(w, &bundle),
            })?
        };
        mercuryrustlib::tesr::cosign_colored_renewal(&self.inner.cc, &mut coin, &mut bundle, draft)
            .await?;
        mercuryrustlib::tesr::persist(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            &bundle,
        )
        .await?;
        Ok(bundle.m)
    }

    /// **[CTES-R] Convey a whole COLOURED carrier — sats and allocation together.**
    ///
    /// The coloured transfer is the same Model-A handover as a plain coin, with one difference: the
    /// receiver-paying state `S'` carries a valid RGB state transition, so it MOVES the allocation
    /// instead of destroying it. `S'` rivals the sender's own state over the extension's payload
    /// output; the seal rung's CSV term is what keeps the two transitions apart.
    ///
    /// The consignment is validated against the ladder BEFORE any SE co-sign, so a seal collision or
    /// a stash that cannot produce a resolvable chain is a refusal here rather than an unvalidatable
    /// consignment at the receiver. After the handover the carrier is marked spent in the engine, so
    /// the sender's balance drops to what it actually still controls.
    pub async fn transfer_colored_carrier(
        &self,
        statechain_id: &str,
        receiver_address: &str,
    ) -> Result<()> {
        let bundle = mercuryrustlib::tesr::load(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            statechain_id,
        )
        .await?
        .ok_or_else(|| anyhow!("statechain id {statechain_id} has no ladder"))?;
        if !bundle.is_colored() {
            return Err(anyhow!(
                "statechain id {statechain_id} has a PLAIN ladder — use the plain transfer path"
            ));
        }
        let rgb_half = bundle.rgb.clone().expect("is_colored");
        let carrier_op = format!("{}:{}", bundle.f_txid, bundle.f_vout);

        // PHASE 1 — engine only: colour S', then PROVE the resulting ladder resolves off-chain.
        let draft = {
            let mut rgb = self.rgb().await?;
            let w = rgb.as_mut().ok_or_else(|| anyhow!("no RGB engine configured"))?;
            tokio::task::block_in_place(|| -> Result<_> {
                let draft = mercuryrustlib::tesr::build_colored_receiver_state(
                    w,
                    &bundle,
                    receiver_address,
                )?;
                // The ladder the RECEIVER will be handed: trigger, current extension, new S'.
                let txids = vec![
                    bundle.trigger.txid.clone(),
                    bundle.current().extension.txid.clone(),
                    draft.tier.txid.clone(),
                ];
                let (verdict, detail, contract) =
                    w.validate_offchain_chain_info(&draft.tier.consignment, &txids)?;
                if verdict != ValidationVerdict::Valid {
                    return Err(anyhow!(
                        "refusing to convey {statechain_id}: the coloured S' consignment does not \
                         validate against the ladder ({verdict:?}): {}",
                        detail.unwrap_or_default()
                    ));
                }
                if contract.as_deref() != Some(rgb_half.contract_id.as_str()) {
                    return Err(anyhow!(
                        "refusing to convey {statechain_id}: the coloured S' consignment is for \
                         contract {contract:?}, not the ladder's {}",
                        rgb_half.contract_id
                    ));
                }
                let assigned = w.accept_offchain_amount(
                    &draft.tier.consignment,
                    &txids,
                    &draft.tier.txid,
                    draft.tier.payload_vout,
                )?;
                if assigned != rgb_half.amount {
                    return Err(anyhow!(
                        "refusing to convey {statechain_id}: the coloured S' assigns {assigned} to \
                         the receiver's exit output but the ladder carries {}",
                        rgb_half.amount
                    ));
                }
                Ok(draft)
            })?
        };

        // PHASE 2 — network only: one blind SE co-sign of S', then the ordinary handover.
        mercuryrustlib::transfer_sender::execute_colored(
            &self.inner.cc,
            receiver_address,
            &self.inner.config.wallet_name,
            statechain_id,
            None,
            false,
            None,
            draft,
        )
        .await?;

        // The carrier is gone. Marking it spent is DB accounting only (the allocation itself moved
        // with the ladder), but without it `get_token_balances` keeps advertising an asset this
        // wallet no longer controls, and the coin keeps being quarantined from plain-BTC selection.
        {
            let mut rgb = self.rgb().await?;
            let w = rgb.as_mut().ok_or_else(|| anyhow!("no RGB engine configured"))?;
            tokio::task::block_in_place(|| w.mark_spent(&[carrier_op]))?;
        }
        Ok(())
    }

    /// The CONFIRMED, non-duplicate coin of `statechain_id`.
    async fn confirmed_coin(&self, statechain_id: &str) -> Result<mercurylib::wallet::Coin> {
        let rec = self.record().await?;
        rec.coins
            .into_iter()
            .find(|c| {
                c.statechain_id.as_deref() == Some(statechain_id)
                    && c.duplicate_index == 0
                    && c.status == CoinStatus::CONFIRMED
            })
            .ok_or_else(|| anyhow!("no CONFIRMED coin for statechain id {statechain_id}"))
    }

    /// **[CTES-R] Receive-side booking of a conveyed COLOURED ladder.**
    ///
    /// `Ok(None)` means "not a coloured ladder, try the legacy lane"; `Ok(Some(..))` books the
    /// allocation. Every refusal that is a property of the material rather than of the network
    /// carries the `PERMANENT-INVALID:` prefix `claim()` matches, so a griefer cannot lock a
    /// victim's sats forever behind a consignment that can never book.
    ///
    /// Three things happen, in this order, and the order matters:
    ///  1. the LEAF consignment is validated against the ladder's OWN un-broadcast txids through the
    ///     fork's `OffchainResolver` — never the plain blockchain resolver, which would report every
    ///     deliberately-un-broadcast tier `Unresolved`, archive it, and recursively invalidate the
    ///     chain with no error and no repair (CTESR-GATE §2.3);
    ///  2. the amount is taken from the CONSIGNMENT (`accept_offchain_amount` at the final state's
    ///     payload output), never from the sender's `ColoredLadder::amount` field, and the two must
    ///     agree;
    ///  3. every tier seal is opened — not just the leaf. The receiver must be able to spend the
    ///     EXTENSION's payload output, because that is the outpoint its own next-hop state tier
    ///     spends; without that seal the coin books fine and is then exit-only.
    async fn accept_colored_ladder(&self, statechain_id: &str) -> Result<Option<(String, u64)>> {
        let Some(bundle) = mercuryrustlib::tesr::load(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            statechain_id,
        )
        .await?
        else {
            return Ok(None);
        };
        if !bundle.is_colored() {
            return Ok(None);
        }
        let rgb_half = bundle.rgb.clone().expect("is_colored");
        let carrier_op = format!("{}:{}", bundle.f_txid, bundle.f_vout);
        // Idempotency: claim() may run this again for the same coin. An allocation already booked on
        // `F` means this ladder was accepted; re-running `register_statechain` would double-book it.
        if self.token_carrier_outpoints().await?.contains(&carrier_op) {
            return Ok(None);
        }
        let txids = bundle.ladder_txids();
        let leaf = bundle
            .leaf_consignment()
            .cloned()
            .ok_or_else(|| anyhow!("PERMANENT-INVALID: coloured ladder carries no consignment"))?;
        let seals = bundle
            .colored_tier_seals()
            .map_err(|e| anyhow!("PERMANENT-INVALID: coloured ladder seals are not derivable: {e}"))?;
        let final_state = bundle.current().state.clone();
        let f_value = bundle.f_value;

        let mut rgb = self.rgb().await?;
        let w = rgb.as_mut().ok_or_else(|| anyhow!("RGB engine not configured"))?;
        let booked = tokio::task::block_in_place(|| -> Result<u64> {
            let (verdict, detail, contract) = w.validate_offchain_chain_info(&leaf, &txids)?;
            match verdict {
                ValidationVerdict::Valid => {}
                ValidationVerdict::PermanentlyInvalid => {
                    return Err(anyhow!(
                        "PERMANENT-INVALID: the conveyed coloured ladder does not validate against \
                         its own tiers: {}",
                        detail.unwrap_or_default()
                    ))
                }
                other => {
                    return Err(anyhow!(
                        "coloured ladder validation is inconclusive ({other:?}): {}",
                        detail.unwrap_or_default()
                    ))
                }
            }
            let contract = contract.ok_or_else(|| {
                anyhow!("PERMANENT-INVALID: valid coloured consignment with no contract id")
            })?;
            if contract != rgb_half.contract_id {
                return Err(anyhow!(
                    "PERMANENT-INVALID: the conveyed ladder claims contract {} but its consignment \
                     verifies under {contract}",
                    rgb_half.contract_id
                ));
            }
            // The CONSIGNMENT-derived amount at the receiver's own exit output. The sender's
            // declared `amount` is attacker-supplied and is only ever checked against this.
            let assigned = w.accept_offchain_amount(
                &leaf,
                &txids,
                &final_state.txid,
                final_state.payload_vout,
            )?;
            if assigned != rgb_half.amount {
                return Err(anyhow!(
                    "PERMANENT-INVALID: the conveyed ladder declares {} but its consignment assigns \
                     {assigned} to the receiver's exit output",
                    rgb_half.amount
                ));
            }
            if assigned == 0 {
                return Err(anyhow!(
                    "PERMANENT-INVALID: the conveyed coloured ladder assigns nothing to the receiver"
                ));
            }
            // First sight of this contract: import its genesis + history, validated against the same
            // un-broadcast ladder.
            w.import_asset_offchain(&leaf, &txids)?;
            // Open EVERY tier seal (see the doc comment, point 3).
            let received = w.accept_ladder(&leaf, &txids, &seals)?;
            if received != assigned {
                return Err(anyhow!(
                    "PERMANENT-INVALID: accepting the coloured ladder booked {received}, but its \
                     consignment assigns {assigned} to the receiver's exit output"
                ));
            }
            w.register_statechain(
                &bundle.f_txid,
                bundle.f_vout,
                f_value,
                &contract,
                assigned,
                &[],
            )?;
            Ok(assigned)
        })?;
        Ok(Some((rgb_half.contract_id, booked)))
    }

    /// **[CTES-R] Receive-side booking of a conveyed COLOURED SPLIT CHILD.**
    ///
    /// The child-lane sibling of [`Self::accept_colored_ladder`], and it exists because a child's
    /// RGB half rides in the `ctesr-` bundle (`ChildTesrBundle::rgb`), not in a `tesr-` ladder and
    /// not in a backup-row consignment envelope — so neither of the other two lanes can see it.
    /// `Ok(None)` means "this coin is not a coloured child, try the next lane".
    ///
    /// The four steps are the ladder's four steps over the child's own five-tier chain, and each one
    /// is here for the same reason it is there:
    ///
    ///  1. the LEAF consignment (`state_child`'s) is validated against the child's OWN un-broadcast
    ///     witness list `[T, X_m, SP, ext_child, state_child]` through the fork's `OffchainResolver`
    ///     — never the plain blockchain resolver, which reports every deliberately-un-broadcast tier
    ///     `Unresolved`, archives it, and recursively invalidates the chain with no error and no
    ///     repair (`CTESR-GATE.md` §2.3);
    ///  2. the amount comes from the CONSIGNMENT, read at `state_child`'s payload output, never from
    ///     the sender's `ColoredChild::amount` field, and the two must agree;
    ///  3. EVERY seal is opened, `SP`'s and `ext_child`'s included. Missing `ext_child`'s payload
    ///     seal is the silent one: the coin books, the balance looks right, and the receiver can
    ///     never colour a spend of that outpoint — i.e. it is exit-only, and
    ///     [`Self::transfer_colored_child`] fails at the last possible moment. `SP`'s seal is opened
    ///     at THIS child's `sp_vout`, which is what keeps its siblings concealed;
    ///  4. the child is REGISTERED as a colorable UTXO at `SP.out[sp_vout]` — the outpoint the claim
    ///     path already booked as the coin's `utxo_txid:utxo_vout`. Without this the allocation is in
    ///     the stash but no carrier-selection walk can find it, so the piece is not a spendable coin.
    ///
    /// Every refusal that is a property of the material rather than of the network carries the
    /// `PERMANENT-INVALID:` prefix `claim()` matches, so a griefer cannot lock a victim's sats
    /// forever behind a child bundle that can never book.
    async fn accept_colored_child_bundle(
        &self,
        statechain_id: &str,
    ) -> Result<Option<(String, u64)>> {
        let Some(cb) = mercuryrustlib::tesr::load_child(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            statechain_id,
        )
        .await?
        else {
            return Ok(None);
        };
        if !cb.is_colored() {
            return Ok(None);
        }
        let rgb_half = cb.rgb.clone().expect("is_colored");
        // The child's funding outpoint: SP.out[sp_vout]. `colored_child_txids` below refuses a
        // multi-level child, so the parent segment's current state IS `SP`.
        let sp_txid = crate::transfer::signed_tier_txid(&cb.parent.current().state.signed_tx)
            .map_err(|e| anyhow!("PERMANENT-INVALID: the child's SP does not parse: {e}"))?;
        let sp_tx: bitcoin::Transaction = bitcoin::consensus::encode::deserialize(
            &hex::decode(&cb.parent.current().state.signed_tx)
                .map_err(|e| anyhow!("PERMANENT-INVALID: SP hex does not decode: {e}"))?,
        )
        .map_err(|e| anyhow!("PERMANENT-INVALID: SP is not a transaction: {e}"))?;
        let sp_out = sp_tx
            .output
            .get(cb.sp_vout as usize)
            .ok_or_else(|| {
                anyhow!("PERMANENT-INVALID: SP has no output {} to fund this child", cb.sp_vout)
            })?
            .clone();
        let child_op = format!("{sp_txid}:{}", cb.sp_vout);
        // Idempotency: claim() re-runs this for the same coin. An allocation already booked at the
        // child's funding outpoint means this bundle was accepted; `register_statechain` is NOT
        // idempotent and a second call would double-book it.
        if self.token_carrier_outpoints().await?.contains(&child_op) {
            return Ok(None);
        }
        let txids = cb.colored_child_txids().map_err(|e| {
            anyhow!("PERMANENT-INVALID: the coloured child has no derivable witness chain: {e}")
        })?;
        let seals = cb.colored_child_seals().map_err(|e| {
            anyhow!("PERMANENT-INVALID: the coloured child's seals are not derivable: {e}")
        })?;
        let leaf = cb
            .leaf_consignment()
            .cloned()
            .ok_or_else(|| anyhow!("PERMANENT-INVALID: coloured child carries no consignment"))?;
        let child_state = cb.child_state.clone();

        let mut rgb = self.rgb().await?;
        let w = rgb.as_mut().ok_or_else(|| anyhow!("RGB engine not configured"))?;
        let booked = tokio::task::block_in_place(|| -> Result<u64> {
            let (verdict, detail, contract) = w.validate_offchain_chain_info(&leaf, &txids)?;
            match verdict {
                ValidationVerdict::Valid => {}
                ValidationVerdict::PermanentlyInvalid => {
                    return Err(anyhow!(
                        "PERMANENT-INVALID: the conveyed coloured child does not validate against \
                         its own ancestor + child tiers: {}",
                        detail.unwrap_or_default()
                    ))
                }
                other => {
                    return Err(anyhow!(
                        "coloured child validation is inconclusive ({other:?}): {}",
                        detail.unwrap_or_default()
                    ))
                }
            }
            let contract = contract.ok_or_else(|| {
                anyhow!("PERMANENT-INVALID: valid coloured child consignment with no contract id")
            })?;
            if contract != rgb_half.contract_id {
                return Err(anyhow!(
                    "PERMANENT-INVALID: the conveyed child claims contract {} but its consignment \
                     verifies under {contract}",
                    rgb_half.contract_id
                ));
            }
            // The CONSIGNMENT-derived amount at the receiver's own exit output. The sender's
            // declared `amount` is attacker-supplied and is only ever checked against this.
            let assigned = w.accept_offchain_amount(
                &leaf,
                &txids,
                &child_state.txid,
                child_state.payload_vout,
            )?;
            if assigned != rgb_half.amount {
                return Err(anyhow!(
                    "PERMANENT-INVALID: the conveyed child declares {} but its consignment assigns \
                     {assigned} to the receiver's exit output",
                    rgb_half.amount
                ));
            }
            if assigned == 0 {
                return Err(anyhow!(
                    "PERMANENT-INVALID: the conveyed coloured child assigns nothing to the receiver"
                ));
            }
            // First sight of this contract: import its genesis + history, validated against the
            // same un-broadcast chain.
            w.import_asset_offchain(&leaf, &txids)?;
            // Open EVERY seal (see the doc comment, point 3).
            let received = w.accept_ladder(&leaf, &txids, &seals)?;
            if received != assigned {
                return Err(anyhow!(
                    "PERMANENT-INVALID: accepting the coloured child booked {received}, but its \
                     consignment assigns {assigned} to the receiver's exit output"
                ));
            }
            w.register_statechain(
                &sp_txid,
                cb.sp_vout,
                sp_out.value,
                &contract,
                assigned,
                &[],
            )?;
            Ok(assigned)
        })?;
        Ok(Some((rgb_half.contract_id, booked)))
    }

    /// **[CTES-R] Is an adopted COLOURED CHILD's allocation still coherent off-chain?** The
    /// child-lane sibling of [`Self::colored_ladder_health`], with the same rule: probe through the
    /// child's OWN un-broadcast witness list, never the plain blockchain resolver (E7).
    ///
    /// Returns `(contract_id, amount_assigned_to_state_child, witness_txids, detail)`.
    pub async fn colored_child_health(
        &self,
        child_statechain_id: &str,
    ) -> Result<(String, u64, Vec<String>, Option<String>)> {
        let cb = mercuryrustlib::tesr::load_child(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            child_statechain_id,
        )
        .await?
        .ok_or_else(|| anyhow!("statechain id {child_statechain_id} is not an adopted child"))?;
        let rgb_half = cb
            .rgb
            .clone()
            .ok_or_else(|| anyhow!("child {child_statechain_id} is PLAIN"))?;
        let txids = cb.colored_child_txids()?;
        let leaf = cb
            .leaf_consignment()
            .cloned()
            .ok_or_else(|| anyhow!("coloured child {child_statechain_id} carries no consignment"))?;
        let child_state = cb.child_state.clone();
        let mut rgb = self.rgb().await?;
        let w = rgb.as_mut().ok_or_else(|| anyhow!("no RGB engine configured"))?;
        tokio::task::block_in_place(|| -> Result<(String, u64, Vec<String>, Option<String>)> {
            let (verdict, detail, contract) = w.validate_offchain_chain_info(&leaf, &txids)?;
            if verdict != ValidationVerdict::Valid {
                return Err(anyhow!(
                    "the coloured child {child_statechain_id} does not validate off-chain \
                     ({verdict:?}): {}",
                    detail.unwrap_or_default()
                ));
            }
            let assigned = w.accept_offchain_amount(
                &leaf,
                &txids,
                &child_state.txid,
                child_state.payload_vout,
            )?;
            Ok((
                contract.unwrap_or_else(|| rgb_half.contract_id.clone()),
                assigned,
                txids,
                detail,
            ))
        })
    }

    /// **[S3] `colored_child_health` for the sender's own CHANGE TIP.**
    ///
    /// A tip is not a child and is not stored as one — `spinetip-`, one cap, no payee — so the child
    /// call cannot reach it. Same three questions though: does the leaf consignment validate against
    /// this tip's OWN un-broadcast witness chain, what amount does it assign, and over which
    /// witnesses. The witness list is `colored_tip_txids`, which contributes ONE txid for the cap
    /// where a child contributes an extension and a state.
    pub async fn colored_tip_health(
        &self,
        statechain_id: &str,
    ) -> Result<(String, u64, Vec<String>, Option<String>)> {
        let tip = mercuryrustlib::tesr::load_spine_tip(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            statechain_id,
        )
        .await?
        .ok_or_else(|| anyhow!("statechain id {statechain_id} is not a persisted spine tip"))?;
        let rgb_half = tip
            .rgb
            .clone()
            .ok_or_else(|| anyhow!("spine tip {statechain_id} is PLAIN"))?;
        let txids = tip.colored_tip_txids()?;
        let cap = tip.cap.clone();
        let mut rgb = self.rgb().await?;
        let w = rgb.as_mut().ok_or_else(|| anyhow!("no RGB engine configured"))?;
        tokio::task::block_in_place(|| -> Result<(String, u64, Vec<String>, Option<String>)> {
            let (verdict, detail, contract) =
                w.validate_offchain_chain_info(&rgb_half.consignment, &txids)?;
            if verdict != ValidationVerdict::Valid {
                return Err(anyhow!(
                    "the coloured spine tip {statechain_id} does not validate off-chain \
                     ({verdict:?}): {}",
                    detail.unwrap_or_default()
                ));
            }
            let assigned = w.accept_offchain_amount(
                &rgb_half.consignment,
                &txids,
                &cap.txid,
                cap.payload_vout,
            )?;
            Ok((contract.unwrap_or_else(|| rgb_half.contract_id.clone()), assigned, txids, detail))
        })
    }

    /// **[CTES-R] The on-chain survival proof of a COLOURED CHILD's unilateral exit.** The
    /// child-lane sibling of [`Self::colored_exit_proof`] — the EMPTY off-chain witness set is the
    /// whole assertion, so `Valid` is reachable only once every tier that ever carried the
    /// allocation (`T, X_m, SP, ext_child, state_child`) is genuinely MINED.
    pub async fn colored_child_exit_proof(
        &self,
        child_statechain_id: &str,
    ) -> Result<(String, u64, Option<String>)> {
        let cb = mercuryrustlib::tesr::load_child(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            child_statechain_id,
        )
        .await?
        .ok_or_else(|| anyhow!("statechain id {child_statechain_id} is not an adopted child"))?;
        let rgb_half = cb
            .rgb
            .clone()
            .ok_or_else(|| anyhow!("child {child_statechain_id} is PLAIN"))?;
        let leaf = cb
            .leaf_consignment()
            .cloned()
            .ok_or_else(|| anyhow!("coloured child {child_statechain_id} carries no consignment"))?;
        let child_state = cb.child_state.clone();
        let mut rgb = self.rgb().await?;
        let w = rgb.as_mut().ok_or_else(|| anyhow!("no RGB engine configured"))?;
        tokio::task::block_in_place(|| -> Result<(String, u64, Option<String>)> {
            // THE EMPTY SET. Not a stylistic choice — see the doc comment.
            let (verdict, detail, contract) = w.validate_offchain_chain_info(&leaf, &[])?;
            if verdict != ValidationVerdict::Valid {
                return Err(anyhow!(
                    "the coloured child {child_statechain_id} does NOT validate against the chain \
                     alone ({verdict:?}): {} — at least one tier is still un-mined, so the exit \
                     walk did not land",
                    detail.unwrap_or_default()
                ));
            }
            let assigned =
                w.accept_offchain_amount(&leaf, &[], &child_state.txid, child_state.payload_vout)?;
            Ok((contract.unwrap_or_else(|| rgb_half.contract_id.clone()), assigned, detail))
        })
    }

    /// **[CTES-R] The §3.3 read-only STOCK probe at a COLOURED CHILD's exit tip.** The child-lane
    /// sibling of [`Self::probe_colored_tip`], and the only assertion that discriminates: E7
    /// measured `get_asset_balance` and `list_unspents` both reporting a full settled spendable
    /// balance over a stock at zero.
    pub async fn probe_colored_child_tip(
        &self,
        child_statechain_id: &str,
        amount: u64,
    ) -> Result<()> {
        let cb = mercuryrustlib::tesr::load_child(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            child_statechain_id,
        )
        .await?
        .ok_or_else(|| anyhow!("statechain id {child_statechain_id} is not an adopted child"))?;
        let rgb_half = cb
            .rgb
            .clone()
            .ok_or_else(|| anyhow!("child {child_statechain_id} is PLAIN"))?;
        let state = cb.child_state.clone();
        let tx: bitcoin::Transaction =
            bitcoin::consensus::encode::deserialize(&hex::decode(&state.signed_tx)?)?;
        let out = tx
            .output
            .get(state.payload_vout as usize)
            .ok_or_else(|| anyhow!("the child state has no output at its declared payload vout"))?;
        let spk_hex = hex::encode(out.script_pubkey.as_bytes());
        let payee = cb.child_owner_exit_address.clone();
        let network = cb.parent.network.clone();
        let mut rgb = self.rgb().await?;
        let w = rgb.as_mut().ok_or_else(|| anyhow!("no RGB engine configured"))?;
        tokio::task::block_in_place(|| {
            mercuryrustlib::rgb::probe_allocation(
                w,
                &rgb_half.contract_id,
                &state.txid,
                state.payload_vout,
                out.value,
                &spk_hex,
                &payee,
                &network,
                amount,
            )
        })
    }

    /// **[CTES-R] Register the EXIT TIP with the RGB engine once a coloured walk has completed.**
    ///
    /// After a unilateral exit the allocation physically lives on the last tier's payload output —
    /// `S_0.out[payload]` for a root ladder, `state_child.out[payload]` for a coloured child,
    /// `cap.out[payload]` for a coloured SPINE TIP — and the outpoint the engine still has
    /// registered (`F`, or `SP.out[j]`, or `SP.out[K]`) has been SPENT on
    /// chain. Until this runs, every UTXO-driven rgb-lib view (`get_asset_balance`,
    /// `list_unspents`, `list_allocations`, `blind_receive`) is not merely incomplete but STALE: it
    /// reports the asset at an outpoint that no longer exists. That was measured and recorded as a
    /// known gap by sdk75, and this closes it.
    ///
    /// The engine cannot discover the tip on its own: it pays a MERCURY seed-derived key that is
    /// not in the engine's BDK descriptor, so no wallet sync will ever surface it.
    /// `register_statechain_utxo` is exactly the path for an outpoint whose existence is asserted
    /// by something other than the engine's own descriptor, and passing the pre-exit outpoint as
    /// `spend_outpoints` is what stops the allocation being double-counted at both ends.
    ///
    /// **Idempotent** — the exit pass is called once per block and reports `complete` on every call
    /// after the last tier lands, so this must survive being invoked repeatedly. It reads the
    /// engine's own allocation list first and returns `Ok(false)` if the tip is already there;
    /// `register_statechain_utxo` inserts rows unconditionally, so without that check a coin's
    /// balance would multiply once per background pass.
    ///
    /// **Never fatal to the exit.** The exit itself is finished and on chain by the time this runs;
    /// a bookkeeping failure must not turn a completed exit into an `Err` that a caller reads as
    /// "the exit failed". The outcome is returned so callers can surface it, and the caller logs it
    /// rather than propagating.
    pub(crate) async fn register_colored_exit_tip(
        &self,
        statechain_id: &str,
    ) -> Result<Option<String>> {
        if self.inner.config.rgb_data_dir.is_none() || self.inner.config.rgb_proxy_url.is_none() {
            return Ok(None);
        }
        // WHICH RECORD backs this coin — and then ONE resolver over all of them.
        //
        // [CATS/V4] This used to be an `if let … else if let … else { None }` chain over the two
        // record shapes that existed when it was written, and that shape is the defect: a coloured
        // SPINE TIP has neither a `tesr-` nor a `ctesr-` row, so it reached the trailing `else` and
        // came back `Ok(None)` — the same answer a plain coin gives. `register_exit_tip_best_effort`
        // maps `Ok(None)` to nothing at all: no event, no fault, no error. The tip's cap would land
        // on chain, spend `SP.out[K]`, and the engine would go on reporting the allocation at that
        // spent outpoint forever — the exact staleness this function's own doc comment describes,
        // reintroduced through a door it did not know about.
        //
        // So the shapes go through `mercuryrustlib::tesr::colored_exit_move`, whose `match` is
        // EXHAUSTIVE: the next record shape added is a compile error here rather than a fourth
        // silent `None`. What remains below is the one honest negative — this coin has no off-chain
        // exit material of any kind.
        let root = mercuryrustlib::tesr::load(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            statechain_id,
        )
        .await?;
        let child = match &root {
            Some(_) => None,
            None => {
                mercuryrustlib::tesr::load_child(
                    &self.inner.cc,
                    &self.inner.config.wallet_name,
                    statechain_id,
                )
                .await?
            }
        };
        let tip = match (&root, &child) {
            (None, None) => {
                mercuryrustlib::tesr::load_spine_tip(
                    &self.inner.cc,
                    &self.inner.config.wallet_name,
                    statechain_id,
                )
                .await?
            }
            _ => None,
        };
        let record = match (&root, &child, &tip) {
            (Some(b), _, _) => Some(mercuryrustlib::tesr::LadderRecord::Root(b)),
            (_, Some(cb), _) => Some(mercuryrustlib::tesr::LadderRecord::Child(cb)),
            (_, _, Some(t)) => Some(mercuryrustlib::tesr::LadderRecord::Tip(t)),
            _ => None,
        };
        let Some(mv) = record.and_then(mercuryrustlib::tesr::colored_exit_move) else {
            return Ok(None);
        };
        let (txid, vout, value, contract, amount, spent) = (
            mv.tip_txid,
            mv.tip_vout,
            mv.tip_value,
            mv.contract_id,
            mv.amount,
            mv.spent_outpoint,
        );
        let tip_outpoint = format!("{txid}:{vout}");
        let mut rgb = self.rgb().await?;
        let w = rgb.as_mut().ok_or_else(|| anyhow!("no RGB engine configured"))?;
        tokio::task::block_in_place(|| -> Result<Option<String>> {
            if w.list_allocations(&contract)?
                .iter()
                .any(|(op, _, _)| *op == tip_outpoint)
            {
                return Ok(None); // already registered by an earlier pass
            }
            w.register_statechain(&txid, vout, value, &contract, amount, &[spent.clone()])?;
            Ok(Some(tip_outpoint))
        })
    }

    /// **[CTES-R] The lane interlock — one carrier, one spend of `F`.**
    ///
    /// A coloured ladder's trigger `T` spends the carrier's funding output `F` with NO timelock. The
    /// legacy coloured split spends the SAME `F` as an absolute-locktime backup maturing ~`initlock`
    /// blocks out. A carrier holding both would let its previous owner broadcast `T` the instant
    /// after conveying a split, taking back the sats AND the asset against a receiver who cannot
    /// race it. The census does not catch this — the piece is a fresh statechain node with its own
    /// `num_sigs` — and neither does the terminal-parent check, because `T` is already co-signed.
    ///
    /// So the two lanes are mutually exclusive per coin and this is where that is enforced, BEFORE
    /// `set_spend_budget` and before any co-sign. It is a refusal, not a preference: there is no
    /// safe ordering, only one lane per carrier.
    ///
    /// Unreachable while `SdkConfig::colored_ladder` is off — no carrier has a coloured ladder then,
    /// which is exactly why that flag defaults to false.
    async fn refuse_if_colored_ladder(&self, carrier_id: &str) -> Result<()> {
        // Fail CLOSED on an unreadable row: "I could not tell whether this carrier has a coloured
        // ladder" must not be read as "it does not".
        let bundle = mercuryrustlib::tesr::load(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            carrier_id,
        )
        .await
        .map_err(|e| {
            anyhow!(
                "cannot tell whether carrier {carrier_id} holds a coloured ladder ({e}) — refusing \
                 the colored split rather than risking two rival spends of its funding output"
            )
        })?;
        if bundle.is_some_and(|b| b.is_colored()) {
            return Err(anyhow!(
                "carrier {carrier_id} holds a COLOURED (CTES-R) ladder, whose trigger already spends \
                 its funding output with no timelock. A colored split would be a RIVAL spend of the \
                 same output that the previous owner could out-race instantly, so it is refused. \
                 Move this coin along its ladder instead."
            ));
        }
        Ok(())
    }

    async fn carrier_spend_generation(&self, carrier_id: &str) -> Result<u32> {
        let rows = read_backup_rows(
            &self.inner.cc.pool,
            &self.inner.config.wallet_name,
            carrier_id,
        )
        .await
        .map_err(|e| {
            anyhow!(
                "refusing to colour a split over carrier {carrier_id}: its spend generation could \
                 not be read ({e}) — guessing it would risk an RGB seal collision"
            )
        })?;
        // A genuinely absent row means no backups yet: generation 0, a true answer.
        Ok(rows.map_or(0, |v| v.len() as u32))
    }

    /// Commit a structural-spend record at its current stage. Durable on return.
    async fn journal_write(&self, rec: &StructuralSpendRecord) -> Result<()> {
        journal_upsert(&self.inner.cc.pool, &self.inner.config.wallet_name, rec).await
    }

    /// Commit a record at a new stage.
    async fn journal_stage(
        &self,
        rec: &mut StructuralSpendRecord,
        stage: StructuralStage,
    ) -> Result<()> {
        rec.stage = stage;
        self.journal_write(rec).await
    }

    /// Replay every interrupted structural colored spend (split/combine) found in the journal.
    ///
    /// Call after a restart — `transfer_tokens` and friends call it themselves before selecting a
    /// carrier, so an interrupted spend is always healed before a new one can touch the same coin.
    /// Errors are propagated, never swallowed: an entry this pass could not resolve stays open and
    /// its carriers stay excluded from selection (fail closed).
    pub async fn recover_structural_spends(&self) -> Result<Vec<StructuralSpendRecovery>> {
        let _guard = self.inner.wallet_lock.lock().await;
        self.recover_structural_spends_locked().await
    }

    /// [`Self::recover_structural_spends`] without taking `wallet_lock` (the caller holds it).
    pub(crate) async fn recover_structural_spends_locked(
        &self,
    ) -> Result<Vec<StructuralSpendRecovery>> {
        let open = journal_open_entries(&self.inner.cc.pool, &self.inner.config.wallet_name).await?;
        let mut report = Vec::with_capacity(open.len());
        for mut rec in open {
            let outcome = match rec.stage {
                StructuralStage::Prepared => {
                    // Ask the SE, per carrier, whether the pinned budget was consumed. A query
                    // failure aborts the pass with the entry still OPEN — guessing here would be
                    // exactly the "success-like default" defect this audit is about.
                    let mut terminal = Vec::with_capacity(rec.carrier_ids.len());
                    for id in &rec.carrier_ids {
                        let (_, _, t) = mercuryrustlib::lightning_latch::get_spend_budget(
                            &self.inner.cc,
                            id,
                        )
                        .await
                        .map_err(|e| {
                            anyhow!(
                                "structural-spend recovery: could not read carrier {id}'s terminal \
                                 state at the SE ({e}) — leaving journal entry {} open (fail-closed)",
                                rec.op_id
                            )
                        })?;
                        terminal.push(t);
                    }
                    let outcome = classify_prepared(&terminal);
                    let stage = match outcome {
                        StructuralSpendOutcome::Retryable => StructuralStage::Abandoned,
                        _ => StructuralStage::Stranded,
                    };
                    self.journal_stage(&mut rec, stage).await?;
                    outcome
                }
                // The signed child material survived: rebuild everything downstream of it. The tail
                // is lane-specific — the batch lane's transaction has N+1 outputs and N envelopes,
                // which `finish_structural_spend`'s two-output shape cannot describe.
                _ if rec.lane == LANE_BATCH_SPLIT => {
                    self.finish_structural_batch_spend(&mut rec, false).await?;
                    StructuralSpendOutcome::Replayed {
                        piece_id: rec.piece_id.clone().unwrap_or_default(),
                        handed_over: false,
                    }
                }
                _ => {
                    self.finish_structural_spend(&mut rec, None).await?;
                    StructuralSpendOutcome::Replayed {
                        piece_id: rec.piece_id.clone().unwrap_or_default(),
                        handed_over: false,
                    }
                }
            };
            report.push(StructuralSpendRecovery {
                op_id: rec.op_id.clone(),
                lane: rec.lane.clone(),
                carrier_ids: rec.carrier_ids.clone(),
                receiver_address: rec.receiver_address.clone(),
                outcome,
            });
        }
        Ok(report)
    }

    /// THE tail of every structural colored spend, shared by the live path and the recovery reader
    /// so the two can never drift: register the sub-coins, re-register the RGB change (or mark the
    /// carriers spent), attach the consignment envelope, and — only when `latch` is `Some` (the live
    /// path) — latch and hand the piece over. Each completed step is journalled before the next one
    /// starts, so a second crash resumes exactly where this one stopped.
    ///
    /// `latch == None` means "replay": the local state is rebuilt and the piece is left in THIS
    /// wallet. A hand-over is a payment the caller must re-authorise, and for a latched piece the
    /// batch id / SE hash were lost with the crash, so completing it would lock the piece forever.
    async fn finish_structural_spend(
        &self,
        rec: &mut StructuralSpendRecord,
        latch: Option<&ColoredLatch>,
    ) -> Result<(Option<String>, Option<String>)> {
        let signed_tx = rec
            .signed_tx
            .clone()
            .ok_or_else(|| anyhow!("journal entry {} has no signed tx", rec.op_id))?;
        let txid = rec
            .txid
            .clone()
            .ok_or_else(|| anyhow!("journal entry {} has no txid", rec.op_id))?;
        let piece_vout = rec
            .piece_vout
            .ok_or_else(|| anyhow!("journal entry {} has no piece vout", rec.op_id))?;
        let change_vout = rec
            .change_vout
            .ok_or_else(|| anyhow!("journal entry {} has no change vout", rec.op_id))?;
        let outputs = [
            (rec.piece_addr.clone(), piece_vout, rec.piece_sats),
            (rec.change_addr.clone(), change_vout, rec.change_sats),
        ];

        if rec.stage == StructuralStage::Signed {
            let ids = if rec.lane == "colored_combine" {
                self.register_combine_subcoins(&rec.carrier_ids, &signed_tx, &txid, &outputs)
                    .await?
            } else {
                let (p, c) = self
                    .register_split_subcoins(&rec.carrier_ids[0], &signed_tx, &txid, &outputs)
                    .await?;
                vec![p, c]
            };
            rec.piece_id = Some(ids[0].clone());
            rec.change_id = Some(ids[1].clone());
            self.journal_stage(rec, StructuralStage::Registered).await?;
        }

        if rec.stage == StructuralStage::Registered {
            // NOT idempotent (a second `register_statechain` would double-book the change), which is
            // exactly why the stage is journalled around it.
            let rgb = self.rgb().await?;
            let w = rgb.as_ref().unwrap();
            let asset_id = rec.asset_id.clone();
            let token_change = rec.token_change;
            let change_sats = rec.change_sats;
            let carrier_ops = rec.carrier_ops.clone();
            tokio::task::block_in_place(|| -> Result<()> {
                if token_change > 0 {
                    w.register_statechain(
                        &txid,
                        change_vout,
                        change_sats,
                        &asset_id,
                        token_change,
                        &carrier_ops,
                    )?;
                } else {
                    w.mark_spent(&carrier_ops)?;
                }
                Ok(())
            })?;
            drop(rgb);
            self.journal_stage(rec, StructuralStage::Colored).await?;
        }

        if rec.stage == StructuralStage::Colored {
            let piece_id = rec
                .piece_id
                .clone()
                .ok_or_else(|| anyhow!("journal entry {} has no piece id", rec.op_id))?;
            let envelope = serde_json::to_string(&ConsignmentEnvelope {
                c: rec
                    .consignment
                    .clone()
                    .ok_or_else(|| anyhow!("journal entry {} has no consignment", rec.op_id))?,
                a: rec.token_amount,
                s: rec.piece_sats,
            })?;
            let mut piece_backups = mercuryrustlib::sqlite_manager::get_backup_txs(
                &self.inner.cc.pool,
                &self.inner.config.wallet_name,
                &piece_id,
            )
            .await?;
            if let Some(first) = piece_backups.first_mut() {
                first.rgb_consignment = Some(envelope);
                first.rgb_blinding = rec.blinding;
            }
            mercuryrustlib::sqlite_manager::update_backup_txs(
                &self.inner.cc.pool,
                &self.inner.config.wallet_name,
                &piece_id,
                &piece_backups,
            )
            .await?;
            self.journal_stage(rec, StructuralStage::Enveloped).await?;
        }

        let latch = match latch {
            Some(l) => l,
            // Replay: the local state is whole again; the piece stays here. The entry is CLOSED so
            // the carriers are released from the selection ban — they are spent, and the piece is a
            // normal coin of this wallet that the caller can re-send.
            None => {
                self.journal_stage(rec, StructuralStage::Committed).await?;
                return Ok((None, None));
            }
        };

        let piece_id = rec
            .piece_id
            .clone()
            .ok_or_else(|| anyhow!("journal entry {} has no piece id", rec.op_id))?;
        let (batch_id, se_hash) = match latch {
            ColoredLatch::None => (None, None),
            ColoredLatch::ExternalHash(hash) => (
                Some(
                    mercuryrustlib::lightning_latch::create_external_hash_latch(
                        &self.inner.cc,
                        &self.inner.config.wallet_name,
                        &piece_id,
                        hash,
                    )
                    .await?,
                ),
                None,
            ),
            ColoredLatch::SePreimage => {
                let pre = mercuryrustlib::lightning_latch::create_pre_image(
                    &self.inner.cc,
                    &self.inner.config.wallet_name,
                    &piece_id,
                )
                .await?;
                (Some(pre.batch_id), Some(pre.hash))
            }
        };
        mercuryrustlib::transfer_sender::execute(
            &self.inner.cc,
            &rec.receiver_address,
            &self.inner.config.wallet_name,
            &piece_id,
            None,
            false,
            batch_id.clone(),
        )
        .await?;
        self.journal_stage(rec, StructuralStage::Committed).await?;
        Ok((batch_id, se_hash))
    }

    /// THE tail of the N-recipient colored split (`batch_transfer_tokens`), shared by the live path
    /// and the recovery reader exactly as [`Self::finish_structural_spend`] is for the other lanes,
    /// so the replay can never drift from what really ran.
    ///
    /// F7 for the batch lane. This lane previously had NO journal at all: it pinned the carrier's
    /// spend budget, took the one irreplaceable co-signature, and then did N+1 sub-coin
    /// registrations, an RGB stash mutation, N envelope writes and N hand-overs entirely in process
    /// memory. A crash anywhere in that stretch destroyed the cooperative path for the carrier AND
    /// every piece derived from it, with nothing on disk to say it had happened — the same
    /// terminalize-before-persist window F7 closed for the single and combine lanes, left open here
    /// only because the record's two-output shape did not fit N recipients.
    ///
    /// `hand_over == false` means "replay": local state is rebuilt and every piece that had not yet
    /// left stays in THIS wallet, because a hand-over is a payment the caller must re-authorise.
    /// Pieces that DID leave are recorded per piece and are never re-sent.
    async fn finish_structural_batch_spend(
        &self,
        rec: &mut StructuralSpendRecord,
        hand_over: bool,
    ) -> Result<Vec<TransferResult>> {
        let signed_tx = rec
            .signed_tx
            .clone()
            .ok_or_else(|| anyhow!("batch journal entry {} has no signed tx", rec.op_id))?;
        let txid = rec
            .txid
            .clone()
            .ok_or_else(|| anyhow!("batch journal entry {} has no txid", rec.op_id))?;
        let change_vout = rec
            .change_vout
            .ok_or_else(|| anyhow!("batch journal entry {} has no change vout", rec.op_id))?;
        if rec.batch_pieces.is_empty() {
            return Err(anyhow!(
                "batch journal entry {} carries no pieces — refusing to replay a batch whose \
                 recipient set is unknown",
                rec.op_id
            ));
        }

        if rec.stage == StructuralStage::Signed {
            let mut outputs: Vec<(String, u32, u64)> = Vec::with_capacity(rec.batch_pieces.len() + 1);
            for (i, p) in rec.batch_pieces.iter().enumerate() {
                let vout = p.vout.ok_or_else(|| {
                    anyhow!("batch journal entry {} has no vout for piece {i}", rec.op_id)
                })?;
                outputs.push((p.addr.clone(), vout, p.sats));
            }
            outputs.push((rec.change_addr.clone(), change_vout, rec.change_sats));
            let ids = self
                .register_split_subcoins_n(&rec.carrier_ids[0], &signed_tx, &txid, &outputs)
                .await?;
            for (i, p) in rec.batch_pieces.iter_mut().enumerate() {
                p.piece_id = Some(ids[i].clone());
            }
            rec.piece_id = rec.batch_pieces[0].piece_id.clone();
            rec.change_id = ids.last().cloned();
            self.journal_stage(rec, StructuralStage::Registered).await?;
        }

        if rec.stage == StructuralStage::Registered {
            // NOT idempotent (a second `register_statechain` double-books the change) — hence the
            // stage journalled around it.
            let rgb = self.rgb().await?;
            let w = rgb.as_ref().unwrap();
            let asset_id = rec.asset_id.clone();
            let token_change = rec.token_change;
            let change_sats = rec.change_sats;
            let carrier_ops = rec.carrier_ops.clone();
            tokio::task::block_in_place(|| -> Result<()> {
                if token_change > 0 {
                    w.register_statechain(
                        &txid,
                        change_vout,
                        change_sats,
                        &asset_id,
                        token_change,
                        &carrier_ops,
                    )?;
                } else {
                    w.mark_spent(&carrier_ops)?;
                }
                Ok(())
            })?;
            drop(rgb);
            self.journal_stage(rec, StructuralStage::Colored).await?;
        }

        if rec.stage == StructuralStage::Colored {
            let consignment = rec
                .consignment
                .clone()
                .ok_or_else(|| anyhow!("batch journal entry {} has no consignment", rec.op_id))?;
            let blinding = rec.blinding;
            for (i, p) in rec.batch_pieces.clone().iter().enumerate() {
                let piece_id = p.piece_id.clone().ok_or_else(|| {
                    anyhow!("batch journal entry {} has no piece id for piece {i}", rec.op_id)
                })?;
                // Each piece gets the SAME consignment but its OWN amount: the receiver re-derives
                // the assignment from the consignment and rejects any envelope that disagrees.
                let envelope = serde_json::to_string(&ConsignmentEnvelope {
                    c: consignment.clone(),
                    a: p.token_amount,
                    s: p.sats,
                })?;
                let mut backups = mercuryrustlib::sqlite_manager::get_backup_txs(
                    &self.inner.cc.pool,
                    &self.inner.config.wallet_name,
                    &piece_id,
                )
                .await?;
                if let Some(first) = backups.first_mut() {
                    first.rgb_consignment = Some(envelope);
                    first.rgb_blinding = blinding;
                }
                mercuryrustlib::sqlite_manager::update_backup_txs(
                    &self.inner.cc.pool,
                    &self.inner.config.wallet_name,
                    &piece_id,
                    &backups,
                )
                .await?;
            }
            self.journal_stage(rec, StructuralStage::Enveloped).await?;
        }

        if !hand_over {
            // Replay: local state is whole again and the un-sent pieces stay here. Closing the entry
            // releases the carrier from the selection ban — it is spent, and the pieces are ordinary
            // coins of this wallet the caller can re-send.
            self.journal_stage(rec, StructuralStage::Committed).await?;
            return Ok(Vec::new());
        }

        let mut results = Vec::with_capacity(rec.batch_pieces.len());
        for i in 0..rec.batch_pieces.len() {
            let p = rec.batch_pieces[i].clone();
            let piece_id = p.piece_id.clone().ok_or_else(|| {
                anyhow!("batch journal entry {} has no piece id for piece {i}", rec.op_id)
            })?;
            if !p.handed_over {
                mercuryrustlib::transfer_sender::execute(
                    &self.inner.cc,
                    &p.recipient,
                    &self.inner.config.wallet_name,
                    &piece_id,
                    None,
                    false,
                    None,
                )
                .await?;
                // Journalled per piece, BEFORE the next hand-over starts: a crash here must not
                // leave a piece that already left the wallet looking un-sent.
                rec.batch_pieces[i].handed_over = true;
                self.journal_write(rec).await?;
            }
            results.push(TransferResult {
                receiver_address: p.recipient.clone(),
                total_sats: p.sats,
                coins: vec![TransferredCoin {
                    statechain_id: piece_id,
                    amount_sats: p.sats,
                }],
                used_split: true,
            });
        }
        self.journal_stage(rec, StructuralStage::Committed).await?;
        Ok(results)
    }

    /// Send `token_amount` of `asset_id` to a statechain address, entirely off-chain: colored
    /// split (exact token piece + change back to this wallet) then branch-carrying key handover
    /// of the piece coin. The receiver's SDK auto-claims, validates the consignment off-chain and
    /// books the balance.
    /// Send `token_amount` of `asset_id` to `receiver_address`, entirely off-chain (colored split).
    pub async fn transfer_tokens(
        &self,
        asset_id: &str,
        receiver_address: &str,
        token_amount: u64,
    ) -> Result<TransferResult> {
        Ok(self
            .colored_transfer(asset_id, receiver_address, token_amount, ColoredLatch::None)
            .await?
            .result)
    }

    /// Colored transfer LATCHED to an EXTERNAL payment hash (the RGB half of a Lightning PAY: the
    /// user hands a colored coin to the SSP, claimable only once the invoice preimage is revealed).
    /// Returns `(batch_id, piece_statechain_id)`.
    pub async fn latch_tokens(
        &self,
        asset_id: &str,
        receiver_address: &str,
        token_amount: u64,
        payment_hash: &str,
    ) -> Result<(String, String)> {
        let out = self
            .colored_transfer(asset_id, receiver_address, token_amount, ColoredLatch::ExternalHash(payment_hash.to_string()))
            .await?;
        let batch = out.batch_id.ok_or_else(|| anyhow!("colored latch did not produce a batch id"))?;
        Ok((batch, out.piece_id))
    }

    /// Colored transfer LATCHED to an SE-HELD preimage (the RGB half of a Lightning RECEIVE: the SSP
    /// hands a colored coin to the user; the SE reveals the preimage only once the coin is released,
    /// so the SSP can't take the HTLC without releasing). Returns `(batch_id, piece_statechain_id,
    /// payment_hash)`.
    pub async fn latch_tokens_se_preimage(
        &self,
        asset_id: &str,
        receiver_address: &str,
        token_amount: u64,
    ) -> Result<(String, String, String)> {
        let out = self
            .colored_transfer(asset_id, receiver_address, token_amount, ColoredLatch::SePreimage)
            .await?;
        let batch = out.batch_id.ok_or_else(|| anyhow!("colored SE-preimage latch produced no batch id"))?;
        let hash = out.se_hash.ok_or_else(|| anyhow!("colored SE-preimage latch produced no payment hash"))?;
        Ok((batch, out.piece_id, hash))
    }

    /// Core colored transfer with an optional latch mode. Returns the pieces + any latch outputs.
    async fn colored_transfer(
        &self,
        asset_id: &str,
        receiver_address: &str,
        token_amount: u64,
        latch: ColoredLatch,
    ) -> Result<ColoredTransferOut> {
        let _guard = self.inner.wallet_lock.lock().await;
        // F7: heal any structural spend an earlier run left half-done BEFORE picking a carrier, so a
        // new spend can never race the replay of an old one over the same coin.
        self.recover_structural_spends_locked().await?;
        let banned = journal_stranded_carriers(&self.inner.cc.pool, &self.inner.config.wallet_name)
            .await?;
        mercuryrustlib::coin_status::update_coins(&self.inner.cc, &self.inner.config.wallet_name)
            .await?;
        let record = self.record().await?;

        // Locate the carrier coin: the confirmed coin whose outpoint holds the allocation.
        let allocations = {
            let mut rgb = self.rgb().await?;
            let w = rgb.as_mut().unwrap();
            tokio::task::block_in_place(|| w.list_allocations(asset_id))?
        };
        let mut carrier: Option<(mercurylib::wallet::Coin, u64)> = None;
        for coin in record.coins.iter() {
            if coin.status != CoinStatus::CONFIRMED || coin.duplicate_index != 0 {
                continue;
            }
            // A carrier whose co-signature was consumed by a lost structural spend can never be
            // co-signed again — exit-only. Never select it (fail closed).
            if coin
                .statechain_id
                .as_deref()
                .map_or(false, |sid| banned.iter().any(|b| b == sid))
            {
                continue;
            }
            let op = format!(
                "{}:{}",
                coin.utxo_txid.clone().unwrap_or_default(),
                coin.utxo_vout.unwrap_or_default()
            );
            if let Some((_, amt, _)) = allocations.iter().find(|(o, _, settled)| *o == op && *settled)
            {
                if *amt >= token_amount {
                    carrier = Some((coin.clone(), *amt));
                    break;
                }
            }
        }
        let (mut carrier, carrier_amount) = match carrier {
            Some(c) => c,
            None => {
                // No single carrier covers the amount. [CTES-R] THE LANE FORK, multi-carrier half:
                // if this wallet's carriers of the asset are COLOURED, pay across them on the
                // coloured lane (one in-ladder split per carrier). Only a wallet with no coloured
                // carrier at all falls through to the legacy combine, which spends every input
                // carrier's `F` directly.
                if let Some(out) = self
                    .colored_multi_carrier_transfer(
                        asset_id,
                        receiver_address,
                        token_amount,
                        &latch,
                        &record,
                        &allocations,
                        &banned,
                    )
                    .await?
                {
                    return Ok(out);
                }
                // [CTES-R MIGRATION] The gate needs to know WHICH coins' `F` is about to be spent,
                // and the combine picks them itself further down. Hand it the SUPERSET — every
                // settled, un-banned carrier of this asset in the wallet — which is strictly more
                // conservative than the set the combine will select from it: one above-floor carrier
                // anywhere in that superset closes the hatch. The exact set is re-gated inside
                // `colored_combine_transfer` once selection has run.
                let hatch_candidates: Vec<mercurylib::wallet::Coin> = record
                    .coins
                    .iter()
                    .filter(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0)
                    .filter(|c| {
                        c.statechain_id
                            .as_deref()
                            .is_some_and(|sid| !banned.iter().any(|b| b == sid))
                    })
                    .filter(|c| {
                        let op = format!(
                            "{}:{}",
                            c.utxo_txid.clone().unwrap_or_default(),
                            c.utxo_vout.unwrap_or_default()
                        );
                        allocations.iter().any(|(o, _, settled)| *o == op && *settled)
                    })
                    .cloned()
                    .collect();
                self.refuse_legacy_colored_split_lane(
                    "combining several carriers",
                    &hatch_candidates,
                )
                .await?;
                return self
                    .colored_combine_transfer(
                        asset_id,
                        receiver_address,
                        token_amount,
                        latch,
                        record,
                        allocations,
                        &banned,
                    )
                    .await;
            }
        };
        let carrier_id = carrier
            .statechain_id
            .clone()
            .ok_or_else(|| anyhow!("carrier coin without statechain id"))?;
        // [CTES-R] THE LANE FORK. A carrier with a coloured ladder cannot take the legacy split at
        // all — that split spends `F`, which the coloured trigger `T` already spends with no
        // timelock, so the two are rival spends of one outpoint and the previous owner wins
        // ([`Self::refuse_if_colored_ladder`]). It does not need to: the coloured IN-LADDER split
        // carves the same value out of a DESCENDANT of `T` instead. So route, do not refuse.
        //
        // Unreachable while `SdkConfig::colored_ladder` is off — no carrier has a coloured ladder
        // then, which is exactly why that flag still defaults to false: the legacy lane below is
        // still present, and one carrier must never be able to reach both.
        if mercuryrustlib::tesr::load(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            &carrier_id,
        )
        .await
        .map_err(|e| {
            anyhow!(
                "cannot tell whether carrier {carrier_id} holds a coloured ladder ({e}) — refusing \
                 the token transfer rather than guessing which lane is safe"
            )
        })?
        .is_some_and(|b| b.is_colored())
        {
            return self
                .colored_in_ladder_transfer(
                    asset_id,
                    receiver_address,
                    token_amount,
                    carrier,
                    carrier_amount,
                    latch,
                )
                .await;
        }
        // [CTES-R] …and the carrier may be a coloured CHILD rather than a laddered root: the change
        // of every coloured payment lands as one, and so does every received piece. A child is a
        // real carrier (its allocation sits on `SP.out[j]`, which carrier selection above finds),
        // but it has no `tesr-` row, so the root fork misses it and it would fall into the retired
        // lane — which would spend the child's funding output with an RGB-unaware transaction.
        if let Some(cb) = mercuryrustlib::tesr::load_child(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            &carrier_id,
        )
        .await
        .map_err(|e| {
            anyhow!(
                "cannot tell whether carrier {carrier_id} is a coloured split child ({e}) — \
                 refusing the token transfer rather than guessing which lane is safe"
            )
        })?
        .filter(|cb| cb.is_colored())
        {
            let held = cb.rgb.as_ref().map(|r| r.amount).unwrap_or_default();
            if token_amount != held {
                // STATED, not silently downgraded. A coloured CHILD-level split does not exist:
                // `SP'` over a child's `ext_child` payload output would make the child a depth-2
                // ancestor segment, and `ChildTesrBundle::colored_child_seals` refuses a multi-level
                // coloured child outright because there is no derivable seal schedule for one. The
                // whole-amount forward below is what does exist. Falling through to the retired lane
                // instead would spend the child's own funding output and destroy the allocation.
                return Err(anyhow!(
                    "cannot pay {token_amount} of {asset_id} out of coloured child {carrier_id}: it \
                     holds {held}, and a coloured CHILD-level split is not implemented (a coloured \
                     child has no derivable depth-2 seal schedule). It can be forwarded WHOLE — pay \
                     exactly {held} — or split at a root carrier instead."
                ));
            }
            if !matches!(latch, ColoredLatch::None) {
                return Err(anyhow!(
                    "a Lightning-latched colored transfer is not yet wired to the CTES-R child lane"
                ));
            }
            self.transfer_colored_child(&carrier_id, receiver_address).await?;
            let sats = carrier.amount.unwrap_or_default() as u64;
            return Ok(ColoredTransferOut {
                result: TransferResult {
                    receiver_address: receiver_address.to_string(),
                    total_sats: sats,
                    coins: vec![TransferredCoin {
                        statechain_id: carrier_id.clone(),
                        amount_sats: sats,
                    }],
                    used_split: false,
                },
                piece_id: carrier_id,
                batch_id: None,
                se_hash: None,
            });
        }
        // [S4b] …and it may be a coloured SPINE TIP: the shape a carrier takes from its SECOND
        // payment onward, because the change of a coloured split is a one-cap tip (S3). The comment
        // on the CHILD arm directly above states this hazard exactly, and the tip is the third shape
        // it applies to — no `tesr-` row, so the root fork misses it and it falls into the RETIRED
        // lane below, which spends `SP.out[K]` with an RGB-unaware transaction and destroys the
        // allocation booked there. Measured: without this arm the second payment out of a coloured
        // carrier was refused as "not laddered YET" by the retirement gate, because a tip reads as
        // un-laddered to every check that asks only for a `tesr-` row.
        if mercuryrustlib::tesr::load_spine_tip(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            &carrier_id,
        )
        .await
        .map_err(|e| {
            anyhow!(
                "cannot tell whether carrier {carrier_id} is a coloured spine tip ({e}) — refusing \
                 the token transfer rather than guessing which lane is safe"
            )
        })?
        .is_some_and(|t| t.is_colored())
        {
            return self
                .colored_in_ladder_transfer(
                    asset_id,
                    receiver_address,
                    token_amount,
                    carrier,
                    carrier_amount,
                    latch,
                )
                .await;
        }
        // Everything below is the RETIRED lane: one `create_colored_split_tx` over the carrier's
        // funding output `F`. Gated as a whole first, then still interlocked per coin — the
        // interlock is the fail-closed backstop if the read above ever disagrees with the load below.
        self.refuse_legacy_colored_split_lane(
            "a single-carrier token transfer",
            std::slice::from_ref(&carrier),
        )
        .await?;
        self.refuse_if_colored_ladder(&carrier_id).await?;
        let carrier_sats = carrier.amount.unwrap_or_default() as u64;
        let fee_reserve = (carrier_sats / 100).clamp(300, 2_000);
        if TOKEN_PIECE_SATS + fee_reserve >= carrier_sats {
            return Err(anyhow!(
                "carrier coin too small ({carrier_sats} sats) for a token split"
            ));
        }
        let change_sats = carrier_sats - TOKEN_PIECE_SATS - fee_reserve;
        let token_change = carrier_amount - token_amount;

        // Backup-fee floor: the 1_500-sat piece and the change must each fund their own backup at
        // the live feerate, else create_tx1 rejects the backup as FeeTooLow AFTER the carrier is
        // made terminal — stranding it. Refuse up-front (carrier untouched). At feerates above
        // ~10 sat/vB the fixed 1_500-sat packaging itself falls below the floor, so a token
        // transfer is correctly refused rather than stranding the carrier.
        let min_output =
            crate::transfer::min_split_output(crate::transfer::backup_fee_rate(&self.inner.cc).await?);
        if TOKEN_PIECE_SATS < min_output || change_sats < min_output {
            return Err(anyhow!(
                "token split output below the minimum viable size {min_output} at the current feerate (piece {TOKEN_PIECE_SATS} sats, change {change_sats} sats) — a sub-coin could not fund its own backup"
            ));
        }

        // Fresh slots for piece and change — DERIVED from the carrier (free SE vouchers; a
        // colored split re-houses the carrier's value, no new on-chain onboarding).
        let mut slot_tokens = self.take_derived_tokens(&carrier_id, 2).await?;
        let token_a = slot_tokens.remove(0);
        let piece_addr = mercuryrustlib::deposit::get_deposit_bitcoin_address(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            &token_a,
            u32::try_from(TOKEN_PIECE_SATS)?,
        )
        .await?;
        let token_b = slot_tokens.remove(0);
        let change_addr = mercuryrustlib::deposit::get_deposit_bitcoin_address(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            &token_b,
            u32::try_from(change_sats)?,
        )
        .await?;

        // Colored split: piece carries the exact token amount; change keeps the rest (or is a
        // plain sats output when the transfer consumes the full allocation).
        let parent_backups = self.carrier_spend_generation(&carrier_id).await?;
        let server_info = mercuryrustlib::utils::info_config(&self.inner.cc).await?;

        // F7 WRITE-AHEAD: the plan becomes durable BEFORE the carrier is terminalized, so a crash in
        // the pre-signature window is classifiable by the recovery reader instead of silent.
        let carrier_op = format!(
            "{}:{}",
            carrier.utxo_txid.clone().unwrap_or_default(),
            carrier.utxo_vout.unwrap_or_default()
        );
        let mut journal = StructuralSpendRecord {
            op_id: uuid::Uuid::new_v4().to_string(),
            lane: "colored_split".to_string(),
            stage: StructuralStage::Prepared,
            asset_id: asset_id.to_string(),
            receiver_address: receiver_address.to_string(),
            token_amount,
            token_change,
            carrier_ids: vec![carrier_id.clone()],
            carrier_ops: vec![carrier_op],
            slot_tokens: vec![token_a.clone(), token_b.clone()],
            piece_addr: piece_addr.clone(),
            change_addr: change_addr.clone(),
            piece_sats: TOKEN_PIECE_SATS,
            change_sats,
            latched: !matches!(latch, ColoredLatch::None),
            signed_tx: None,
            txid: None,
            piece_vout: None,
            change_vout: None,
            consignment: None,
            blinding: None,
            piece_id: None,
            change_id: None,
            batch_pieces: Vec::new(),
        };
        self.journal_write(&journal).await?;

        // Terminal-spend guard on the carrier: one more co-signature (the colored split), then
        // the SE refuses everything — the token branch cannot be double-spent. This MUST stay ahead
        // of the co-signature (see the journal's module note): pinning the budget afterwards would
        // let a malicious sender co-sign two rival branches and show both receivers a terminal
        // ancestor.
        mercuryrustlib::lightning_latch::set_spend_budget(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            &carrier_id,
            1,
        )
        .await?;
        // The pre-signature window: the budget is pinned but no co-signature exists yet.
        crash_point("after_structural_terminalize");
        let splits = vec![
            (piece_addr.clone(), TOKEN_PIECE_SATS, token_amount),
            (change_addr.clone(), change_sats, token_change),
        ];
        // Per-transfer seal: the carrier being spent, the split role, the carrier's spend
        // generation, and the transition's arity — always 2 here. What separates this lane from the
        // batch lane below (whose split over the same carrier would otherwise share
        // `(role, tier_index)`, and at `n == 1` the same arity too) is `BATCH_SPLIT_RUNG_FLAG`,
        // which that lane ORs into its rung; this one never sets it.
        let split_blinding =
            single_split_seal(&carrier_id, parent_backups, splits.len() as u32).blinding();
        let split = {
            let rgb = self.rgb().await?;
            let w = rgb.as_ref().unwrap();
            mercuryrustlib::rgb::create_colored_split_tx(
                &self.inner.cc,
                w,
                &mut carrier,
                asset_id,
                &splits,
                parent_backups + 1,
                false,
                None,
                &self.inner.config.network.to_string(),
                server_info.initlock,
                server_info.interval,
                split_blinding,
            )
            .await?
        };
        // F7 COMMIT POINT: the co-signature is spent and irreplaceable, so the signed child material
        // is made durable HERE — before the sub-coin registration, the RGB stash mutation, the
        // envelope write and the hand-over. Everything after this line is replayable from the
        // journal by `recover_structural_spends`.
        journal.signed_tx = Some(split.signed_tx.clone());
        journal.txid = Some(split.txid.clone());
        journal.piece_vout = Some(split.output_vouts[0]);
        journal.change_vout = Some(split.output_vouts[1]);
        journal.consignment = Some(split.consignment.clone());
        journal.blinding = Some(split.blinding);
        self.journal_stage(&mut journal, StructuralStage::Signed)
            .await?;
        // The exact instant F7 is about: the carrier is terminal, the co-signature is spent, and
        // NOTHING downstream has run yet. sdk73 kills the process here and proves the split is
        // rebuilt from the journal alone.
        crash_point("after_structural_sign");

        // Registration → RGB change → envelope → latch → hand-over, each stage journalled. THE SAME
        // code the recovery reader runs, so a replay cannot drift from the live path.
        let (batch_id, se_hash) = self.finish_structural_spend(&mut journal, Some(&latch)).await?;
        let piece_id = journal
            .piece_id
            .clone()
            .ok_or_else(|| anyhow!("colored split produced no piece id"))?;

        Ok(ColoredTransferOut {
            result: TransferResult {
                receiver_address: receiver_address.to_string(),
                total_sats: TOKEN_PIECE_SATS,
                coins: vec![TransferredCoin {
                    statechain_id: piece_id.clone(),
                    amount_sats: TOKEN_PIECE_SATS,
                }],
                used_split: true,
            },
            piece_id,
            batch_id,
            se_hash,
        })
    }

    /// **[CTES-R] Pay a PARTIAL token amount out of a COLOURED carrier — the in-ladder coloured
    /// split, and the replacement for [`Self::colored_transfer`]'s `create_colored_split_tx` route.**
    ///
    /// The legacy route spends the carrier's funding output `F` directly. On a coloured carrier `F`
    /// is already spent by the trigger `T` — with NO timelock — so the two are rival spends of one
    /// outpoint carrying conflicting RGB transitions, and the party holding `T` wins instantly. That
    /// is the hazard `colored_ladder` still defaults OFF for.
    ///
    /// This route carves the same value out of a DESCENDANT of `T`: a split state `SP` over `X_m`'s
    /// payload output assigning the recipient's piece and this wallet's change, and per child a
    /// headless COLOURED ladder (`ext_child`, `state_child`) rooted at `SP.out[j]`. `SP` is not a
    /// rival for `F`; it is a rival for the parent's own retained `S_0` one rung lower, which is the
    /// ordinary CTES-R case the per-tier seal blinding already separates.
    ///
    /// TWO PHASES, and the split is load-bearing twice over. The RGB engine's resolver is `!Sync`,
    /// so its guard must not be alive across an `await` (or `claim()`'s future stops being `Send`);
    /// and phase 1 PROVES both children's consignments resolve against the chain they will be
    /// conveyed with **before** a single SE co-signature is spent — a seal collision or an
    /// unresolvable stash is a refusal with the carrier untouched, never an unvalidatable
    /// consignment at a receiver who has already been paid for.
    ///
    /// KNOWN GAP, stated rather than hidden: this lane has no F7 structural-spend journal. Neither
    /// does the plain in-ladder lane it is modelled on (`transfer::in_ladder_pay`) — the journal
    /// exists for the `create_colored_split_tx` lanes — so a crash between `cosign_…` and the
    /// conveyance leaves the piece child in this wallet, exitable, with the parent terminalized.
    /// That is recoverable by hand and is not a loss of funds, but it is not yet automatic.
    async fn colored_in_ladder_transfer(
        &self,
        asset_id: &str,
        receiver_address: &str,
        token_amount: u64,
        carrier: mercurylib::wallet::Coin,
        carrier_amount: u64,
        latch: ColoredLatch,
    ) -> Result<ColoredTransferOut> {
        if !matches!(latch, ColoredLatch::None) {
            return Err(anyhow!(
                "a Lightning-latched colored transfer is not yet wired to the CTES-R in-ladder \
                 lane — refusing rather than silently paying over the retired split lane"
            ));
        }
        let payouts = [(receiver_address.to_string(), token_amount)];
        let pieces = self
            .colored_in_ladder_pay(asset_id, carrier, carrier_amount, &payouts)
            .await?;
        let (piece_sid, piece_sats) = pieces
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("the coloured in-ladder split produced no piece"))?;
        Ok(ColoredTransferOut {
            result: TransferResult {
                receiver_address: receiver_address.to_string(),
                total_sats: piece_sats,
                coins: vec![TransferredCoin {
                    statechain_id: piece_sid.clone(),
                    amount_sats: piece_sats,
                }],
                used_split: true,
            },
            piece_id: piece_sid,
            batch_id: None,
            se_hash: None,
        })
    }

    /// **[CTES-R] THE coloured payment engine: one `SP` over `X_m`, `N` payouts and an optional
    /// change child.** Every coloured send in the SDK goes through here — single, batch, and each
    /// leg of a multi-carrier payment — so there is exactly one place where a carrier's value is
    /// carved, and it is a descendant of `T` rather than a rival of `F`.
    ///
    /// `payouts` is `(receiver_address, rgb_amount)`. `Σ rgb_amount` must not exceed the carrier's
    /// allocation; whatever is left becomes a change child kept by this wallet, and when nothing is
    /// left NO change child is carved at all (see below).
    ///
    /// ## Sizing, and why the no-change case is a different shape rather than a zero-amount child
    ///
    /// `X_m`'s payload output affords `colored_tier_out_total(x_m, n_children, rate)` across its
    /// children — a figure that DEPENDS on the child count, because each child is a real output the
    /// committed fee has to cover. So the child count is decided first, from whether there is any
    /// allocation left over, and the budget is computed for exactly that count.
    ///
    /// When the carrier's whole allocation is being paid out, carving a change child anyway would
    /// mean carving a child with an EMPTY RGB assignment that must still clear `colored_child_floor`
    /// and still funds two coloured rungs — sats spent to hold nothing, and a shape no other part of
    /// CTES-R produces or verifies. Instead the last piece absorbs the remainder of the budget.
    ///
    /// ## Ordering
    ///
    /// Every refusal that can be raised is raised BEFORE `cosign_colored_in_ladder_split`, because
    /// that call terminalizes the parent: a split rejected afterwards leaves the carrier spent and
    /// its value in children that were never conveyed. Phase 1 is engine-only and proves each
    /// child's leaf consignment resolves against the exact witness list its receiver will use.
    async fn colored_in_ladder_pay(
        &self,
        asset_id: &str,
        mut carrier: mercurylib::wallet::Coin,
        carrier_amount: u64,
        payouts: &[(String, u64)],
    ) -> Result<Vec<(String, u64)>> {
        use mercuryrustlib::tesr::{ColoredSplitChildSpec, COLORED_LADDER_DUST};

        if payouts.is_empty() {
            return Err(anyhow!("a coloured in-ladder split needs at least one payout"));
        }
        // [K>1] THE ENGINE is where this is refused, not the entry point. Every coloured send in the
        // SDK funnels through here — `transfer_tokens`, `batch_transfer_tokens`, and each leg of a
        // multi-carrier payment — so a future route cannot reach a shared-blinding batch by taking a
        // different door. First statement after the empty check, i.e. before the ladder is read and
        // long before anything is co-signed.
        refuse_colored_multi_payee(payouts.len())?;
        let network = self.inner.config.network.to_string();
        let carrier_id = carrier
            .statechain_id
            .clone()
            .ok_or_else(|| anyhow!("carrier coin without statechain id"))?;
        let bundle = mercuryrustlib::tesr::load(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            &carrier_id,
        )
        .await?;
        // [S4b] A COLOURED TIP IS A CARRIER, AND ITS SPLIT IS A DIFFERENT CONSTRUCT.
        //
        // `carrier_is_colored` now routes a coloured spine tip here (it used to answer `false` and
        // send it down the plain lane, which would have spent `SP.out[K]` uncoloured and destroyed
        // the allocation). This engine splits a ROOT carrier: it reads a `tesr-` ladder and builds
        // `SP` over `X_m`'s payload output. A tip has neither — its `SP_{i+1}` sits over the tip's
        // own funding outpoint and out-races the tip's cap — so it needs
        // `build_colored_spine_batch` + `cosign_colored_spine_batch`, which exist.
        //
        // What does NOT exist yet is this driver's batch sibling (the child-slot minting, conveyance
        // and RGB re-booking around them). Until it does, the honest answer is a refusal that names
        // the shape and the reason, not a generic "no TES-R ladder" — which reads as data loss and
        // is the sort of message that gets someone to reach for a lane that would burn the coin.
        let bundle = match bundle {
            Some(b) => b,
            None => {
                let tip = mercuryrustlib::tesr::load_spine_tip(
                    &self.inner.cc,
                    &self.inner.config.wallet_name,
                    &carrier_id,
                )
                .await?;
                // A COLOURED SPINE TIP is the carrier's shape from its SECOND payment onward, and
                // it routes to the batch — `SP_{i+1}` over the tip's own funding outpoint, not `SP`
                // over `X_m`'s payload. Dispatching here rather than at the entry point keeps the
                // rule where every coloured send already funnels through.
                if tip.as_ref().is_some_and(|t| t.is_colored()) {
                    return self
                        .colored_spine_batch_pay(asset_id, carrier, carrier_amount, payouts)
                        .await;
                }
                return Err(anyhow!(
                    "carrier {carrier_id} has no TES-R ladder to split in-ladder"
                ));
            }
        };
        let rgb_half = bundle
            .rgb
            .clone()
            .ok_or_else(|| anyhow!("carrier {carrier_id} has a PLAIN ladder"))?;
        if rgb_half.contract_id != asset_id {
            return Err(anyhow!(
                "carrier {carrier_id}'s coloured ladder carries contract {} but this transfer is \
                 for {asset_id}",
                rgb_half.contract_id
            ));
        }
        // The ladder's declared allocation and the engine's booked allocation must agree, because
        // the split's conservation law is stated over the LADDER's amount and the caller's change
        // is computed from it. Disagreement means one of the two views is stale; refuse rather than
        // mint or burn the difference.
        if rgb_half.amount != carrier_amount {
            return Err(anyhow!(
                "carrier {carrier_id}'s ladder declares {} of {asset_id} but the engine has {} \
                 booked on its funding output — refusing to split on a disagreement",
                rgb_half.amount,
                carrier_amount
            ));
        }
        let token_out: u64 = payouts.iter().map(|(_, a)| *a).sum();
        if payouts.iter().any(|(_, a)| *a == 0) {
            return Err(anyhow!(
                "a coloured in-ladder split payout of 0 of {asset_id} would carve a child holding \
                 no allocation — refusing"
            ));
        }
        if token_out == 0 || token_out > rgb_half.amount {
            return Err(anyhow!(
                "cannot send {token_out} of {asset_id}: the carrier holds {}",
                rgb_half.amount
            ));
        }
        let token_change = rgb_half.amount - token_out;

        // ---- SATS. The budget depends on the CHILD COUNT (each child is an output the committed
        // coloured fee must cover), so the count is fixed first.
        let n_pay = payouts.len();
        let n_children = n_pay + usize::from(token_change > 0);
        let x_m = bundle.current().extension.clone();
        let total = mercuryrustlib::rgb::colored_tier_out_total(
            x_m.out_value,
            n_children,
            bundle.fee_rate,
        )
        .ok_or_else(|| {
            anyhow!(
                "X_m's payload output ({} sat) cannot carry a coloured {n_children}-child split at \
                 {} sat/vB",
                x_m.out_value,
                bundle.fee_rate
            )
        })?;
        // Piece sizing: every piece gets the standard piece size, except that when there is NO
        // change child the LAST piece absorbs the remainder — otherwise those sats would be
        // silently forfeited to the miner on exit.
        let mut piece_sats: Vec<u64> = vec![TOKEN_PIECE_SATS; n_pay];
        let change_sats = if token_change > 0 {
            let spent = TOKEN_PIECE_SATS
                .checked_mul(n_pay as u64)
                .ok_or_else(|| anyhow!("piece sizing overflowed"))?;
            if spent >= total {
                return Err(anyhow!(
                    "carrier {carrier_id} is too small for a coloured in-ladder split into \
                     {n_pay} piece(s) + change: its extension affords {total} sat and the pieces \
                     alone are {spent}"
                ));
            }
            total - spent
        } else {
            let head = TOKEN_PIECE_SATS
                .checked_mul(n_pay as u64 - 1)
                .ok_or_else(|| anyhow!("piece sizing overflowed"))?;
            if head >= total {
                return Err(anyhow!(
                    "carrier {carrier_id} is too small for a coloured in-ladder split into \
                     {n_pay} piece(s): its extension affords {total} sat"
                ));
            }
            piece_sats[n_pay - 1] = total - head;
            0
        };
        // BOTH floors apply and the larger binds, exactly as `transfer::in_ladder_pay` does it:
        //  * the coloured LADDER floor for that leg's SHAPE — a coloured PIECE funds its OWN two
        //    coloured rungs (`colored_child_floor`), the sender's CHANGE funds whatever
        //    `change_leg_role()` says the builder gives it. This is the load-bearing one: the child
        //    ladders are built AFTER the carrier's spend budget is consumed, so admitting a leg below
        //    its floor would terminalize the carrier and THEN fail, stranding it exit-only — and on
        //    this lane "exit-only" also means the allocation's only pre-signed RGB-aware walk;
        //  * `min_split_output` — the generic sub-coin viability floor at the live backup fee rate.
        //
        // [V5] PER-LEG, as on the plain lane and for the identical reason: the coloured tip floor is
        // 576 sat below the coloured piece floor, so one shared number could only carry the tip's
        // cheaper shape by applying it to every PAYEE too.
        let backup_floor = crate::transfer::min_split_output(
            crate::transfer::backup_fee_rate(&self.inner.cc).await?,
        );
        let piece_floor =
            backup_floor.max(mercuryrustlib::tesr::SplitLegRole::Piece
                .colored_min_value(bundle.fee_rate, COLORED_LADDER_DUST));
        // [CATS change 2] `SplitLane::Colored` — this lane's OWN answer, not the plain root lane's.
        // `cosign_colored_in_ladder_split` still sends every leg through the two-rung coloured child
        // builder, so its change floor is still `colored_child_floor`. Reading the root lane's
        // answer here would floor the change at 906 while the builder built 1 482 worth of rungs,
        // and `establish_child` would discover that after the carrier was already terminal.
        let change_floor =
            backup_floor.max(mercuryrustlib::tesr::change_leg_role(
                mercuryrustlib::tesr::SplitLane::Colored,
            )
            .colored_min_value(bundle.fee_rate, COLORED_LADDER_DUST));
        let too_small: Vec<u64> =
            piece_sats.iter().copied().filter(|s| *s < piece_floor).collect();
        if !too_small.is_empty() {
            return Err(anyhow!(
                "coloured in-ladder token split needs every PIECE >= {piece_floor} sat (each piece \
                 funds its own coloured extension + state tier at {} sat/vB, then must clear the \
                 dust floor); these do not: {too_small:?}",
                bundle.fee_rate
            ));
        }
        if token_change > 0 && change_sats < change_floor {
            return Err(anyhow!(
                "coloured in-ladder token split needs the CHANGE >= {change_floor} sat at {} sat/vB \
                 (it is {change_sats}); lower the payout or use a larger carrier",
                bundle.fee_rate
            ));
        }

        // Fresh SE-registered child slots (DERIVED — free vouchers against the carrier's value; an
        // in-ladder split re-houses value, it does not onboard any).
        let mut slot_tokens = self.take_derived_tokens(&carrier_id, n_children).await?;
        let mut child_coins: Vec<mercurylib::wallet::Coin> = Vec::with_capacity(n_children);
        for sats in piece_sats.iter().copied() {
            child_coins.push(self.create_child_slot(&slot_tokens.remove(0), sats).await?);
        }
        if token_change > 0 {
            child_coins.push(self.create_child_slot(&slot_tokens.remove(0), change_sats).await?);
        }
        let child_sids: Vec<String> = child_coins
            .iter()
            .map(|c| {
                c.statechain_id
                    .clone()
                    .ok_or_else(|| anyhow!("child slot has no statechain id"))
            })
            .collect::<Result<_>>()?;

        // Model A payees: each piece pays its RECIPIENT's own exit key, the change pays ours.
        let mut specs: Vec<ColoredSplitChildSpec> = Vec::with_capacity(n_children);
        for (j, (receiver_address, amount)) in payouts.iter().enumerate() {
            specs.push(ColoredSplitChildSpec {
                statechain_id: child_sids[j].clone(),
                agg_address: child_coins[j]
                    .aggregated_address
                    .clone()
                    .ok_or_else(|| anyhow!("piece child slot has no aggregate address"))?,
                owner_exit_address: mercurylib::tesr::payee_address(receiver_address, &network)?,
                sats: piece_sats[j],
                rgb_amount: *amount,
                // A PAYEE — two coloured rungs, floored at `colored_child_floor`.
                is_change_tip: false,
            });
        }
        if token_change > 0 {
            let c = &child_coins[n_pay];
            specs.push(ColoredSplitChildSpec {
                statechain_id: child_sids[n_pay].clone(),
                agg_address: c
                    .aggregated_address
                    .clone()
                    .ok_or_else(|| anyhow!("change child slot has no aggregate address"))?,
                owner_exit_address: mercurylib::transaction::get_user_backup_address(
                    c,
                    network.clone(),
                )?,
                sats: change_sats,
                rgb_amount: token_change,
                // [S3] THE SENDER'S CHANGE — a one-rung SPINE TIP. Declared here rather than
                // inferred from being last: position is a convention, and a convention that decides
                // a leg's floor and ladder shape is one that eventually gets violated silently.
                is_change_tip: true,
            });
        }

        // ---- PHASE 1 — engine only: build + colour, then PROVE each child resolves. --------------
        let draft = {
            let mut rgb = self.rgb().await?;
            let w = rgb.as_mut().ok_or_else(|| anyhow!("no RGB engine configured"))?;
            tokio::task::block_in_place(|| -> Result<_> {
                let draft =
                    mercuryrustlib::tesr::build_colored_in_ladder_split(w, &bundle, &specs)?;
                for cd in draft.children.iter() {
                    // The witness list the RECEIVER will resolve against — `ChildTesrBundle::
                    // colored_child_txids` for the bundle this draft is about to become.
                    // [S3] A CHANGE TIP's witness chain has no extension — it is one cap over
                    // `SP.out[j]`. Including a phantom txid would make the receiver's resolver look
                    // for a witness that was never built.
                    let mut txids = vec![
                        bundle.trigger.txid.clone(),
                        x_m.txid.clone(),
                        draft.sp_txid.clone(),
                    ];
                    if let Some(x) = cd.extension.as_ref() {
                        txids.push(x.txid.clone());
                    }
                    txids.push(cd.state.txid.clone());
                    let (verdict, detail, contract) =
                        w.validate_offchain_chain_info(&cd.state.consignment, &txids)?;
                    if verdict != ValidationVerdict::Valid {
                        // [S3] NAME THE LEG. A payee and the sender's change tip have different
                        // ladder shapes — two rungs versus one cap — and therefore different
                        // witness chains, so "which leg" is the first thing anyone debugging this
                        // needs and the message used to withhold it. `extension: None` IS the tip.
                        let leg = if cd.extension.is_none() {
                            "the sender's CHANGE TIP (one cap over SP.out[j], no extension)"
                        } else {
                            "a PAYEE piece (extension + state over SP.out[j])"
                        };
                        return Err(anyhow!(
                            "refusing the coloured in-ladder split of {carrier_id}: child {} — \
                             {leg} — has a leaf consignment that does not validate against its own \
                             chain ({verdict:?}) over witnesses {txids:?}: {}",
                            cd.statechain_id,
                            detail.unwrap_or_default()
                        ));
                    }
                    if contract.as_deref() != Some(rgb_half.contract_id.as_str()) {
                        return Err(anyhow!(
                            "refusing the coloured in-ladder split of {carrier_id}: child {}'s \
                             consignment is for contract {contract:?}, not the ladder's {}",
                            cd.statechain_id,
                            rgb_half.contract_id
                        ));
                    }
                    let assigned = w.accept_offchain_amount(
                        &cd.state.consignment,
                        &txids,
                        &cd.state.txid,
                        cd.state.payload_vout,
                    )?;
                    if assigned != cd.rgb_amount {
                        return Err(anyhow!(
                            "refusing the coloured in-ladder split of {carrier_id}: child {}'s \
                             consignment assigns {assigned} to its exit output but the split gives \
                             it {}",
                            cd.statechain_id,
                            cd.rgb_amount
                        ));
                    }
                }
                Ok(draft)
            })?
        };
        // Which SP output funds which child — read from the draft, never assumed (a coloured tier
        // carries its opret at index 0 and shifts every payload by one).
        let change_sp_vout = (token_change > 0).then(|| draft.children[n_pay].sp_vout);
        let sp_txid = draft.sp_txid.clone();

        // ---- PHASE 2 — network only: 1 + 2N blind SE co-signs, then the handover. ----------------
        let (bundles, change_tip) = mercuryrustlib::tesr::cosign_colored_in_ladder_split(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            &mut carrier,
            &bundle,
            draft,
            &mut child_coins,
        )
        .await?;

        // ---- [B2] THE SENDER'S OWN LADDER ROW, BROUGHT UP TO DATE — BEFORE ANY CONVEYANCE. -------
        //
        // `cosign_colored_in_ladder_split` builds a terminalized PARENT SEGMENT — `SP` installed as
        // the current state, the superseded `S_0` disclosed alongside it — and hands one copy to
        // every child bundle. Nothing wrote that segment back to OUR `tesr-<parent>` row, so the
        // sender's own row went on naming `S_0` as live. `defend_ladders` drives that row: the
        // moment anyone triggered the carrier, this wallet's watchtower would broadcast `S_0`, which
        // rivals the recipient's `SP` over `X_m`'s payload output and — because `SP` is where the
        // recipient's RGB assignment lives — would DESTROY the allocation we just paid them.
        //
        // Persisting the segment does not merely disarm that; it makes the sender's tower an ALLY:
        // `exit_tiers()` now yields `T -> X_m -> SP`, exactly the three transactions the recipient's
        // child chain needs underneath it.
        //
        // ORDER IS THE POINT. This runs after the co-sign (SP does not exist before it) and before
        // the first `convey_child_bundle`, so the window in which a recipient holds a bundle while
        // our tower is still armed against it does not exist. A failure here therefore aborts with
        // nothing conveyed, and is deliberately FATAL: continuing would hand out an allocation this
        // wallet is armed to destroy, which is a third party's loss rather than our own.
        mercuryrustlib::tesr::persist(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            &bundles[0].parent,
        )
        .await
        .map_err(|e| {
            anyhow!(
                "the coloured in-ladder split of {carrier_id} is co-signed but its terminalized \
                 parent segment could not be stored ({e}). Refusing to convey any child: this \
                 wallet's watchtower would still be driving the SUPERSEDED state S_0, which rivals \
                 the children's SP and would destroy their allocation. The carrier is terminal and \
                 its value sits in un-conveyed child slots — recover by retrying the store."
            )
        })?;

        // Convey each piece to its recipient's mailbox (auth = the piece slot we still own), then
        // keep the change as an exitable, re-transferable self-claim.
        for (j, (receiver_address, _)) in payouts.iter().enumerate() {
            mercuryrustlib::tesr::convey_child_bundle(
                &self.inner.cc,
                receiver_address,
                &child_coins[j],
                &bundles[j],
                None,
            )
            .await?;
        }
        // [S3 — CORRECTED] THE CHANGE IS A TIP, SO PERSIST IT AS ONE.
        //
        // This read `persist_child(&bundles[n_pay])`, which was correct while every leg was a
        // two-rung child. Once S3 made the change leg a one-cap SPINE TIP it stopped being a
        // `ChildTesrBundle` at all, so `bundles` holds only the payees and the index panicked —
        // out of bounds on the very call that saves the sender's own money. A `spinetip-` row is
        // also what `exit_spine_tip_pass`, the watchtower and the next batch's funding lookup all
        // read, so writing it under `ctesr-` would have been wrong in a quieter way.
        if token_change > 0 {
            let tip = change_tip.ok_or_else(|| {
                anyhow!(
                    "the coloured split kept {token_change} of the allocation as change but produced                      no spine tip to hold it — refusing to continue, because the change would have                      no persisted exit and the allocation would be stranded on an un-broadcast                      outpoint"
                )
            })?;
            mercuryrustlib::tesr::persist_spine_tip(
                &self.inner.cc,
                &self.inner.config.wallet_name,
                &tip,
            )
            .await?;
        } else if change_tip.is_some() {
            return Err(anyhow!(
                "the coloured split produced a change tip while keeping NO change — the draft's leg                  shapes and the amounts disagree"
            ));
        }

        // ---- RGB bookkeeping: the carrier's `F` is spent, the change lives at SP.out[change]. ----
        // Without this `get_token_balances` keeps advertising the whole allocation on an outpoint
        // this wallet no longer controls, and the change piece is invisible to carrier selection —
        // i.e. not a spendable coin. Not idempotent, which is why it runs exactly once here.
        let carrier_op = format!(
            "{}:{}",
            carrier.utxo_txid.clone().unwrap_or_default(),
            carrier.utxo_vout.unwrap_or_default()
        );
        {
            let rgb = self.rgb().await?;
            let w = rgb.as_ref().ok_or_else(|| anyhow!("no RGB engine configured"))?;
            let contract = rgb_half.contract_id.clone();
            let sp = sp_txid.clone();
            let ops = vec![carrier_op];
            tokio::task::block_in_place(|| -> Result<()> {
                match change_sp_vout {
                    Some(vout) => {
                        w.register_statechain(
                            &sp,
                            vout,
                            change_sats,
                            &contract,
                            token_change,
                            &ops,
                        )?;
                    }
                    None => w.mark_spent(&ops)?,
                }
                Ok(())
            })?;
        }

        let piece_sids: Vec<String> = child_sids[..n_pay].to_vec();
        self.book_inladder_split_coins_opt(
            &carrier_id,
            &sp_txid,
            &piece_sids,
            CoinStatus::WITHDRAWN,
            change_sp_vout.map(|v| (child_sids[n_pay].clone(), change_sats, v)),
        )
        .await?;

        Ok(piece_sids.into_iter().zip(piece_sats).collect())
    }

    /// **[S4b] Pay from a COLOURED SPINE TIP — the second and every later payment of a carrier.**
    ///
    /// [`Self::colored_in_ladder_pay`]'s sibling. The first payment out of a coloured carrier splits
    /// the ROOT: `SP` over `X_m`'s payload output. It leaves the change as a one-cap TIP (S3), and
    /// from then on the carrier IS that tip — so every later payment is a spine BATCH: `SP_{i+1}`
    /// over the tip's own funding outpoint, out-racing the tip's cap at `SPINE_CSV`.
    ///
    /// Everything below `SP` is the same construction, which is why the two share
    /// `build_colored_split_legs`. What differs here:
    ///
    /// * the sizing budget comes from the tip's slot (`sp_out_value`), not `X_m`'s payload;
    /// * the change leg is the NEXT tip, persisted under `spinetip-`, one level deeper;
    /// * the spent tip's coin is booked WITHDRAWN — that is what disarms this wallet's tower, which
    ///   would otherwise go on driving the tip's now-superseded cap against the children's
    ///   `SP_{i+1}` and destroy their allocation.
    async fn colored_spine_batch_pay(
        &self,
        asset_id: &str,
        mut carrier: mercurylib::wallet::Coin,
        carrier_amount: u64,
        payouts: &[(String, u64)],
    ) -> Result<Vec<(String, u64)>> {
        use mercuryrustlib::tesr::{ColoredSplitChildSpec, COLORED_LADDER_DUST};

        if payouts.is_empty() {
            return Err(anyhow!("a coloured spine batch needs at least one payout"));
        }
        // Same door, same reason as the root lane: one `seal.blinding()` covers every payload, so
        // payee j de-conceals every sibling seal in K tries. Refused at the engine, not the entry.
        refuse_colored_multi_payee(payouts.len())?;
        let network = self.inner.config.network.to_string();
        let carrier_id = carrier
            .statechain_id
            .clone()
            .ok_or_else(|| anyhow!("carrier coin without statechain id"))?;
        let tip = mercuryrustlib::tesr::load_spine_tip(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            &carrier_id,
        )
        .await?
        .ok_or_else(|| anyhow!("carrier {carrier_id} is not a persisted spine tip"))?;
        let rgb_half = tip
            .rgb
            .clone()
            .ok_or_else(|| anyhow!("spine tip {carrier_id} is PLAIN"))?;
        if rgb_half.contract_id != asset_id {
            return Err(anyhow!(
                "spine tip {carrier_id} carries contract {} but this transfer is for {asset_id}",
                rgb_half.contract_id
            ));
        }
        if rgb_half.amount != carrier_amount {
            return Err(anyhow!(
                "spine tip {carrier_id} declares {} of {asset_id} but the engine has {} booked on \
                 its funding output — refusing to split on a disagreement",
                rgb_half.amount,
                carrier_amount
            ));
        }
        let token_out: u64 = payouts.iter().map(|(_, a)| *a).sum();
        if payouts.iter().any(|(_, a)| *a == 0) {
            return Err(anyhow!("a coloured spine batch payout of 0 would carve an empty leg"));
        }
        if token_out == 0 || token_out > rgb_half.amount {
            return Err(anyhow!(
                "cannot send {token_out} of {asset_id}: the tip holds {}",
                rgb_half.amount
            ));
        }
        let token_change = rgb_half.amount - token_out;
        // A batch ALWAYS leaves a tip. Spending a tip to zero would end the carrier's payment life
        // with no funding outpoint for the next level, so the whole-allocation case is the ROOT
        // lane's `build_colored_receiver_state` (convey the tip whole), not a batch.
        if token_change == 0 {
            return Err(anyhow!(
                "a coloured spine batch must leave a change tip, but this payout is the tip's whole \
                 {token_out} of {asset_id}. To move it all, convey the tip whole rather than \
                 batching it — a batch with no change leaves no funding outpoint for the next \
                 payment"
            ));
        }

        let n_pay = payouts.len();
        let n_children = n_pay + 1;
        let fee_rate = tip.parent.fee_rate;
        let total = mercuryrustlib::rgb::colored_tier_out_total(
            tip.sp_out_value,
            n_children,
            fee_rate,
        )
        .ok_or_else(|| {
            anyhow!(
                "the tip's slot ({} sat) cannot carry a coloured {n_children}-output batch at \
                 {fee_rate} sat/vB",
                tip.sp_out_value
            )
        })?;
        let mut piece_sats: Vec<u64> = vec![TOKEN_PIECE_SATS; n_pay];
        let spent = TOKEN_PIECE_SATS
            .checked_mul(n_pay as u64)
            .ok_or_else(|| anyhow!("piece sizing overflowed"))?;
        if spent >= total {
            return Err(anyhow!(
                "spine tip {carrier_id} is too small for a coloured batch into {n_pay} piece(s) + \
                 change: its slot affords {total} sat and the pieces alone are {spent}"
            ));
        }
        let change_sats = total - spent;
        // BOTH floors, per leg, and the larger binds — identical reasoning to the root lane. The
        // change leg is a TIP here by construction (a batch's change always is), so it takes the
        // one-rung floor, which sits below the piece floor; one shared number could only carry the
        // tip's cheaper shape by applying it to every payee too.
        let backup_floor = crate::transfer::min_split_output(
            crate::transfer::backup_fee_rate(&self.inner.cc).await?,
        );
        let piece_floor = backup_floor.max(
            mercuryrustlib::tesr::SplitLegRole::Piece
                .colored_min_value(fee_rate, COLORED_LADDER_DUST),
        );
        let change_floor = backup_floor.max(
            mercuryrustlib::tesr::SplitLegRole::SpineTip
                .colored_min_value(fee_rate, COLORED_LADDER_DUST),
        );
        let too_small: Vec<u64> = piece_sats.iter().copied().filter(|s| *s < piece_floor).collect();
        if !too_small.is_empty() {
            return Err(anyhow!(
                "coloured spine batch needs every PIECE >= {piece_floor} sat at {fee_rate} sat/vB; \
                 these do not: {too_small:?}"
            ));
        }
        if change_sats < change_floor {
            return Err(anyhow!(
                "coloured spine batch needs the CHANGE TIP >= {change_floor} sat at {fee_rate} \
                 sat/vB (it is {change_sats}) — a batch that cannot leave a viable tip ends the \
                 carrier's payment life"
            ));
        }

        let mut slot_tokens = self.take_derived_tokens(&carrier_id, n_children).await?;
        let mut child_coins: Vec<mercurylib::wallet::Coin> = Vec::with_capacity(n_children);
        for sats in piece_sats.iter().copied() {
            child_coins.push(self.create_child_slot(&slot_tokens.remove(0), sats).await?);
        }
        child_coins.push(self.create_child_slot(&slot_tokens.remove(0), change_sats).await?);
        let child_sids: Vec<String> = child_coins
            .iter()
            .map(|c| c.statechain_id.clone().ok_or_else(|| anyhow!("child slot has no statechain id")))
            .collect::<Result<_>>()?;

        let mut specs: Vec<ColoredSplitChildSpec> = Vec::with_capacity(n_children);
        for (j, (receiver_address, amount)) in payouts.iter().enumerate() {
            specs.push(ColoredSplitChildSpec {
                statechain_id: child_sids[j].clone(),
                agg_address: child_coins[j]
                    .aggregated_address
                    .clone()
                    .ok_or_else(|| anyhow!("piece child slot has no aggregate address"))?,
                owner_exit_address: mercurylib::tesr::payee_address(receiver_address, &network)?,
                sats: piece_sats[j],
                rgb_amount: *amount,
                is_change_tip: false,
            });
        }
        {
            let c = &child_coins[n_pay];
            specs.push(ColoredSplitChildSpec {
                statechain_id: child_sids[n_pay].clone(),
                agg_address: c
                    .aggregated_address
                    .clone()
                    .ok_or_else(|| anyhow!("change slot has no aggregate address"))?,
                owner_exit_address: mercurylib::transaction::get_user_backup_address(
                    c,
                    network.clone(),
                )?,
                sats: change_sats,
                rgb_amount: token_change,
                is_change_tip: true,
            });
        }

        // The spine LEVEL: one deeper than every segment already walked. It is what separates this
        // batch's seals from the previous level's — two levels over one funding chain share
        // `(statechain_id, role, tier_index)`, and colliding blindings collapse to one `BundleId`.
        let spine_level = tip.ancestors.len() as u32 + 1;
        let draft = {
            let mut rgb = self.rgb().await?;
            let w = rgb.as_mut().ok_or_else(|| anyhow!("no RGB engine configured"))?;
            tokio::task::block_in_place(|| -> Result<_> {
                let draft =
                    mercuryrustlib::tesr::build_colored_spine_batch(w, &tip, &specs, spine_level)?;
                // Same pre-flight as the root lane: PROVE each leg's consignment resolves against
                // its own witness chain BEFORE the tip is terminalized. The chain is the root
                // ladder, every intermediate segment, this batch's `SP`, then the leg's own tiers.
                let mut base = tip.parent.ladder_txids();
                for seg in tip.ancestors.iter() {
                    if let Some(x) = seg.extension.as_ref() {
                        base.push(x.txid.clone());
                    }
                    base.push(seg.state.txid.clone());
                }
                base.push(draft.sp_txid.clone());
                for cd in draft.children.iter() {
                    let mut txids = base.clone();
                    if let Some(x) = cd.extension.as_ref() {
                        txids.push(x.txid.clone());
                    }
                    txids.push(cd.state.txid.clone());
                    let (verdict, detail, contract) =
                        w.validate_offchain_chain_info(&cd.state.consignment, &txids)?;
                    if verdict != ValidationVerdict::Valid {
                        let leg = if cd.extension.is_none() {
                            "the sender's CHANGE TIP (one cap over SP.out[j])"
                        } else {
                            "a PAYEE piece (extension + state over SP.out[j])"
                        };
                        return Err(anyhow!(
                            "refusing the coloured spine batch of {carrier_id}: leg {} — {leg} — \
                             has a consignment that does not validate against its own chain \
                             ({verdict:?}) over witnesses {txids:?}: {}",
                            cd.statechain_id,
                            detail.unwrap_or_default()
                        ));
                    }
                    if contract.as_deref() != Some(rgb_half.contract_id.as_str()) {
                        return Err(anyhow!(
                            "refusing the coloured spine batch of {carrier_id}: leg {}'s \
                             consignment is for contract {contract:?}, not the tip's {}",
                            cd.statechain_id,
                            rgb_half.contract_id
                        ));
                    }
                }
                Ok(draft)
            })?
        };
        let sp_txid = draft.sp_txid.clone();
        let change_sp_vout = draft.children[n_pay].sp_vout;

        let (bundles, next_tip) = mercuryrustlib::tesr::cosign_colored_spine_batch(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            &mut carrier,
            &tip,
            draft,
            &mut child_coins,
        )
        .await?;

        // The NEXT tip first, then convey — the ordering the root lane argues for and for the same
        // reason: every tier is already co-signed and the old tip is terminal, so a conveyance that
        // failed here would abort with the change record unwritten, destroying the only record of
        // this wallet's remaining allocation.
        let next_tip = next_tip.ok_or_else(|| {
            anyhow!(
                "the coloured spine batch of {carrier_id} produced no change tip — refusing to \
                 convey, because this wallet's remaining {token_change} of {asset_id} would have \
                 no persisted exit"
            )
        })?;
        let next_tip_sid = next_tip.statechain_id.clone();
        mercuryrustlib::tesr::persist_spine_tip(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            &next_tip,
        )
        .await?;

        for (j, (receiver_address, _)) in payouts.iter().enumerate() {
            mercuryrustlib::tesr::convey_child_bundle(
                &self.inner.cc,
                receiver_address,
                &child_coins[j],
                &bundles[j],
                None,
            )
            .await?;
        }

        // RGB re-booking: the tip's funding outpoint is spent by `SP_{i+1}`, and the change now
        // lives at `SP_{i+1}.out[K']`.
        let tip_op = {
            let (txid, vout) = tip.funding_outpoint();
            format!("{txid}:{vout}")
        };
        {
            let rgb = self.rgb().await?;
            let w = rgb.as_ref().ok_or_else(|| anyhow!("no RGB engine configured"))?;
            let contract = rgb_half.contract_id.clone();
            let sp = sp_txid.clone();
            let ops = vec![tip_op];
            tokio::task::block_in_place(|| -> Result<()> {
                w.register_statechain(
                    &sp,
                    change_sp_vout,
                    change_sats,
                    &contract,
                    token_change,
                    &ops,
                )?;
                Ok(())
            })?;
        }

        // Booking the spent tip WITHDRAWN is what DISARMS this wallet's tower. Its child loop gates
        // on the coin's durable status, and the old tip's cap now rivals `SP_{i+1}` over the shared
        // outpoint — an armed tower would broadcast the cap and destroy the allocation it just paid
        // away.
        let piece_sids: Vec<String> = child_sids[..n_pay].to_vec();
        self.book_inladder_split_coins_opt(
            &carrier_id,
            &sp_txid,
            &piece_sids,
            CoinStatus::WITHDRAWN,
            Some((next_tip_sid, change_sats, change_sp_vout)),
        )
        .await?;

        Ok(piece_sids.into_iter().zip(piece_sats).collect())
    }

    /// Does this carrier hold a COLOURED (CTES-R) ladder? Fail CLOSED: an unreadable row is an
    /// `Err`, never `false` — "I could not tell which lane this coin is on" must not be answered
    /// with the lane that spends its funding output.
    async fn carrier_is_colored(&self, carrier_id: &str) -> Result<bool> {
        let root = mercuryrustlib::tesr::load(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            carrier_id,
        )
        .await
        .map_err(|e| {
            anyhow!(
                "cannot tell whether carrier {carrier_id} holds a coloured ladder ({e}) — refusing \
                 the token transfer rather than guessing which lane is safe"
            )
        })?;
        if let Some(b) = root {
            return Ok(b.is_colored());
        }
        // [S4b] …AND A COLOURED SPINE TIP IS A COLOURED CARRIER.
        //
        // This asked only for a `tesr-` row, so the sender's own change from a coloured split —
        // which is a `spinetip-` row (S3) — answered `false`. That is not a missing feature, it is
        // the WRONG LANE: both callers use this to fork, and `false` sends a coin whose allocation
        // sits at `SP.out[K]` down the plain path, which spends that outpoint uncoloured and
        // destroys the allocation. The coin is reachable — the change tip is registered with the RGB
        // engine, so carrier selection can and does pick it for a second payment.
        //
        // Fail CLOSED on the read for the same reason as above: "I could not tell" must never be
        // answered with the lane that spends the funding output.
        Ok(mercuryrustlib::tesr::load_spine_tip(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            carrier_id,
        )
        .await
        .map_err(|e| {
            anyhow!(
                "cannot tell whether carrier {carrier_id} is a coloured spine tip ({e}) — refusing \
                 the token transfer rather than guessing which lane is safe"
            )
        })?
        .is_some_and(|t| t.is_colored()))
    }


    /// **[CTES-R MIGRATION] The coloured ROOT floor every carrier of this wallet is measured against.**
    ///
    /// `TesrParams::committed_fee_rate` is a PROTOCOL constant per network — 2.0 on mainnet and on
    /// regtest alike, pinned by [`token_piece_sats_is_the_coloured_root_floor`] — not the live
    /// mempool rate. That is what makes "below the floor" a PERMANENT fact about a coin rather than a
    /// temporary one: a funding output's value never changes either, so a carrier under this number
    /// today is under it forever, and no later `claim()` pass can rescue it.
    pub(crate) fn colored_root_floor(&self) -> u64 {
        mercuryrustlib::tesr::colored_ladder_floor(
            mercurylib::tesr::TesrParams::for_network(&self.inner.config.network.to_string())
                .committed_fee_rate,
            mercuryrustlib::tesr::COLORED_LADDER_DUST,
        )
    }

    /// **[CTES-R MIGRATION] Facts about one carrier, gathered for [`migration_hatch_verdict`].**
    ///
    /// Every unknown resolves toward the HATCH SHUT, i.e. toward today's refusal:
    ///
    ///  * an unreadable ladder row is an `Err`, never "no ladder" — "I could not tell whether this
    ///    coin already has a trigger spending `F`" must never license a second spend of `F`;
    ///  * a coin with no recorded amount, no funding txid, or a funding output the chain will not
    ///    show us is reported `colourable_now = true`. We could not prove a coloured ladder is
    ///    impossible, so we do not act as though we had.
    ///
    /// `colourable_now` re-runs, READ-ONLY, the three preconditions `build_colored_ladder_auto`
    /// would hit, in the order it hits them:
    ///
    ///  1. the sats pre-flight (`colored_ladder_floor`), which refuses before any SE co-sign;
    ///  2. the claim path's own colouring precondition — the outpoint must resolve to EXACTLY ONE
    ///     booked allocation, since a coloured tier assigns one amount of one contract;
    ///  3. rgb-lib's own answer, via [`Self::probe_carrier_funding`]: can the stash still spend that
    ///     allocation out of `F`? This is `color_psbt`, not `color_psbt_and_consume` — nothing is
    ///     written, nothing is resolved, and the coin is untouched. It is also the ONLY one of the
    ///     three that catches the case sdk78 measured, where a piece well above the floor with a
    ///     properly booked allocation is still refused colouring (`Invalid coloring info`).
    ///
    /// Failing (2) or (3) can be transient (a consignment not booked yet). That is deliberate and
    /// safe rather than sloppy: see [`migration_hatch_verdict`] on why "now" is the right tense —
    /// this runs under `wallet_lock`, which is the lock `claim()` takes to build a ladder.
    async fn carrier_migration_facts(
        &self,
        coin: &mercurylib::wallet::Coin,
    ) -> Result<CarrierMigrationFacts> {
        let sid = coin
            .statechain_id
            .clone()
            .ok_or_else(|| anyhow!("carrier coin without statechain id at the retirement gate"))?;
        // [S4b] A COLOURED SPINE TIP IS LADDERED. `has_ladder` decides whether the retirement gate
        // treats a carrier as "waiting for its ladder" (refuse, it will get one) or "can never have
        // one" (open the migration hatch). A tip has a ladder — a cap over an un-broadcast outpoint,
        // descending from the root's — but no `tesr-` row, so this read called it un-laddered and
        // the gate refused a payment that is perfectly well-formed.
        let has_tip_ladder = mercuryrustlib::tesr::load_spine_tip(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            &sid,
        )
        .await
        .map_err(|e| {
            anyhow!("cannot read the spine-tip row of carrier {sid} ({e}) — refusing to classify it")
        })?
        .is_some();
        let has_ladder = has_tip_ladder
            || mercuryrustlib::tesr::load(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            &sid,
        )
        .await
        .map_err(|e| {
            anyhow!(
                "cannot tell whether carrier {sid} already carries a ladder ({e}) — refusing the \
                 legacy coloured lane rather than risking two rival spends of its funding output"
            )
        })?
        .is_some();
        // No amount recorded ⟹ report it AT the floor: unknown resolves to "colourable".
        let sats = match coin.amount {
            Some(a) => a as u64,
            None => self.colored_root_floor(),
        };
        let colourable_now = if sats < self.colored_root_floor() {
            false // (1) the pre-flight refuses before any co-sign, on numbers neither side can move
        } else {
            // (2) exactly one booked allocation, read the same way `claim()` reads it. An unreadable
            // allocation map is an `Err` here, not an empty map: "I could not see the RGB state" must
            // not be turned into "this coin cannot be coloured".
            let op = crate::wallet::coin_outpoint(coin);
            let booked = self.token_carrier_allocations().await.map_err(|e| {
                anyhow!(
                    "cannot tell whether carrier {sid} could be coloured ({e}) — refusing the legacy \
                     coloured lane rather than guessing that no rival trigger can exist"
                )
            })?;
            match op.and_then(|o| booked.get(&o).cloned()).flatten() {
                None => false,
                // (3) rgb-lib's own verdict, read-only.
                Some((contract_id, amount)) => {
                    self.probe_carrier_funding(&sid, &contract_id, amount).await.is_ok()
                }
            }
        };
        Ok(CarrierMigrationFacts { statechain_id: sid, sats, has_ladder, colourable_now })
    }

    /// **[CTES-R MIGRATION] Can this carrier NEVER be coloured?**
    ///
    /// THE single definition of the migration class, shared by the two subsystems that must agree
    /// about it: the legacy-lane gate below (may this coin's `F` still be spent RGB-aware?) and
    /// `UtexoWallet::unilateral_exit` (may this coin still be materialised?). They are two halves of
    /// one promise — every carrier keeps at least one safe way out — and a coin that fell into the
    /// class for one and out of it for the other would satisfy neither.
    ///
    /// Fails CLOSED through [`Self::carrier_migration_facts`]: an unreadable ladder row is an `Err`,
    /// never `true`.
    pub(crate) async fn carrier_is_permanently_flat(
        &self,
        coin: &mercurylib::wallet::Coin,
    ) -> Result<bool> {
        let facts = self.carrier_migration_facts(coin).await?;
        Ok(migration_hatch_verdict(self.colored_root_floor(), std::slice::from_ref(&facts)).is_ok())
    }

    /// **THE RETIREMENT GATE for the legacy coloured-split lane — with the migration hatch.**
    ///
    /// Everything past this point spends a carrier's FUNDING output `F` directly through
    /// `mercuryrustlib::rgb::create_colored_split_tx` / `create_colored_combine_tx`. That is the lane
    /// CTES-R replaces, and once `SdkConfig::colored_ladder` is on it must not run — with exactly one
    /// exception, argued in [`migration_hatch_verdict`]: a carrier that can NEVER be coloured.
    ///
    /// `refuse_if_colored_ladder` is the per-coin interlock and stays where it is; it answers "does
    /// THIS coin already have a rival spend of `F`?". This answers the different, stronger question:
    /// "is this wallet running the lane that made `F` spendable twice?". They are not the same check
    /// — a carrier can fail to ladder for TRANSIENT reasons (its allocation is not booked yet, its
    /// outpoint holds more than one allocation, its ladder row is unreadable) and would then slip
    /// past the per-coin interlock straight into the retired lane, on a wallet whose every OTHER
    /// carrier is coloured. Refusing here means such a carrier waits for its ladder instead of
    /// silently taking the old path — and that is still what happens to every one of them.
    ///
    /// `carriers` is the coins whose `F` this route is about to spend. It is REQUIRED and it is
    /// checked: an empty list proves nothing about any coin and is refused (see the verdict).
    async fn refuse_legacy_colored_split_lane(
        &self,
        what: &str,
        carriers: &[mercurylib::wallet::Coin],
    ) -> Result<()> {
        if self.inner.config.colored_ladder {
            let mut facts = Vec::with_capacity(carriers.len());
            for c in carriers {
                facts.push(self.carrier_migration_facts(c).await?);
            }
            let floor = self.colored_root_floor();
            if let Err(why) = migration_hatch_verdict(floor, &facts) {
                return Err(anyhow!(
                    "the legacy coloured-split lane is RETIRED on this wallet ({what}): it spends \
                     the carrier's funding output `F` directly, which is exactly what a CTES-R \
                     trigger `T` spends with no timelock. {why}"
                ));
            }
            eprintln!(
                "[CTES-R migration] {what}: no COLOURED ladder can be built for any carrier this \
                 route would spend, and none of them holds a ladder already — so no trigger `T` \
                 exists or can be built to rival this spend (carriers: {}; coloured root floor \
                 {floor} sat). Opening the RGB-aware legacy lane for them: this is the migration \
                 hatch for carriers CTES-R cannot serve, not the retired lane coming back.",
                facts
                    .iter()
                    .map(|f| format!("{} ({} sat)", f.statechain_id, f.sats))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        Ok(())
    }

    /// **[CTES-R] Pay an amount that spans SEVERAL coloured carriers.**
    ///
    /// The coloured replacement for [`Self::colored_combine_transfer`], and it is deliberately not
    /// shaped like it. The legacy combine builds ONE transaction spending every input carrier's `F`
    /// — the shape that cannot exist on the coloured lane, because each of those `F`s is already
    /// spent by that carrier's own trigger `T`, and there is no multi-parent coloured tier: `SP`
    /// spends exactly one `X_m`.
    ///
    /// So a multi-carrier payment becomes a multi-PIECE payment: one in-ladder split per carrier,
    /// each conveying a child to the same recipient, and the recipient books them as separate
    /// allocations that sum to the amount. That is not a workaround for the missing combine — it is
    /// what "pay across carriers" means when each carrier is an independent off-chain ladder. RGB
    /// value conservation holds per split, and the recipient's balance is the sum, which is the
    /// property the legacy combine delivered too.
    ///
    /// Returns `Ok(None)` when this wallet has NO coloured carrier of the asset, so the caller can
    /// fall through to the legacy lane (which is itself gated by
    /// [`Self::refuse_legacy_colored_split_lane`]).
    ///
    /// ## The failure mode, stated
    ///
    /// Legs are executed in sequence and each leg is an independent SE-co-signed split. A failure on
    /// leg `k > 0` leaves legs `0..k` already conveyed to the recipient — the recipient is short-paid
    /// rather than unpaid, and nothing is lost, but it is not atomic. The legacy combine was atomic
    /// (one transaction) and had an F7 journal to make it recoverable; this lane has neither, exactly
    /// like the single-carrier in-ladder lane it is built from. The error names every piece already
    /// conveyed so the caller can finish or refund deliberately instead of guessing.
    #[allow(clippy::too_many_arguments)]
    async fn colored_multi_carrier_transfer(
        &self,
        asset_id: &str,
        receiver_address: &str,
        token_amount: u64,
        latch: &ColoredLatch,
        record: &mercurylib::wallet::Wallet,
        allocations: &[(String, u64, bool)],
        banned: &[String],
    ) -> Result<Option<ColoredTransferOut>> {
        // 1. Every CONFIRMED, un-banned, settled carrier of this asset that holds a COLOURED
        //    ladder, largest allocation first (fewest legs, fewest SE co-signs).
        let mut coloured: Vec<(mercurylib::wallet::Coin, u64)> = Vec::new();
        let mut plain_seen = 0usize;
        // Coloured CHILDREN of this asset. They are NOT selectable legs — a coloured child-level
        // split does not exist — but they ARE balance, and leaving them out of the arithmetic makes
        // the refusal below lie about how much the wallet holds. A user who has just paid and is
        // now looking at their change deserves "you hold 10, you asked for 200", not "no carrier of
        // this asset has a coloured ladder yet".
        let mut child_held = 0u64;
        let mut child_count = 0usize;
        for coin in record.coins.iter() {
            if coin.status != CoinStatus::CONFIRMED || coin.duplicate_index != 0 {
                continue;
            }
            let Some(sid) = coin.statechain_id.as_deref() else { continue };
            if banned.iter().any(|b| b == sid) {
                continue;
            }
            let op = format!(
                "{}:{}",
                coin.utxo_txid.clone().unwrap_or_default(),
                coin.utxo_vout.unwrap_or_default()
            );
            let Some((_, amt, _)) = allocations.iter().find(|(o, _, s)| *o == op && *s) else {
                continue;
            };
            if self.carrier_is_colored(sid).await? {
                coloured.push((coin.clone(), *amt));
            } else if mercuryrustlib::tesr::load_child(
                &self.inner.cc,
                &self.inner.config.wallet_name,
                sid,
            )
            .await?
            .is_some_and(|cb| cb.is_colored())
            {
                child_held += *amt;
                child_count += 1;
            } else {
                plain_seen += 1;
            }
        }
        if coloured.is_empty() && child_count == 0 {
            return Ok(None); // nothing on this lane — the caller decides what to do next
        }
        if !matches!(latch, ColoredLatch::None) {
            return Err(anyhow!(
                "a Lightning-latched colored transfer is not yet wired to the CTES-R in-ladder \
                 lane — refusing rather than silently paying over the retired split lane"
            ));
        }
        coloured.sort_by(|a, b| b.1.cmp(&a.1));

        // 2. Plan the legs BEFORE touching any carrier, so an under-funded payment is refused with
        //    every carrier still intact rather than after the first one has been terminalized.
        let mut plan: Vec<(mercurylib::wallet::Coin, u64, u64)> = Vec::new(); // (coin, alloc, pay)
        let mut remaining = token_amount;
        for (coin, alloc) in coloured.into_iter() {
            if remaining == 0 {
                break;
            }
            let pay = remaining.min(alloc);
            remaining -= pay;
            plan.push((coin, alloc, pay));
        }
        if remaining > 0 {
            let mut extra = String::new();
            if child_count > 0 {
                extra.push_str(&format!(
                    ". A further {child_held} sits on {child_count} coloured CHILD coin(s), which \
                     can only be forwarded WHOLE (a coloured child-level split does not exist), so \
                     it cannot contribute part of a payment"
                ));
            }
            if plain_seen > 0 {
                extra.push_str(&format!(
                    ". {plain_seen} further carrier(s) of this asset have no coloured ladder yet — \
                     they cannot be paid from on the CTES-R lane and will not be silently paid over \
                     the retired split lane"
                ));
            }
            return Err(anyhow!(
                "cannot send {token_amount} of {asset_id}: this wallet's COLOURED carriers hold \
                 {} in total (short by {remaining}){extra}",
                token_amount - remaining,
            ));
        }

        // 3. Execute. One in-ladder split per carrier; each conveys its piece to the recipient.
        let mut coins: Vec<TransferredCoin> = Vec::new();
        let mut total_sats = 0u64;
        for (leg, (coin, alloc, pay)) in plan.into_iter().enumerate() {
            let payouts = [(receiver_address.to_string(), pay)];
            let pieces = self
                .colored_in_ladder_pay(asset_id, coin, alloc, &payouts)
                .await
                .map_err(|e| {
                    if leg == 0 {
                        e
                    } else {
                        anyhow!(
                            "the multi-carrier coloured payment of {token_amount} of {asset_id} \
                             FAILED on leg {leg} ({e}). Legs 0..{leg} are already conveyed to \
                             {receiver_address}: {}. The recipient is SHORT-PAID, not unpaid — \
                             nothing is lost, but this lane is not atomic and you must finish or \
                             refund the remainder deliberately.",
                            coins
                                .iter()
                                .map(|c| c.statechain_id.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    }
                })?;
            for (sid, sats) in pieces {
                total_sats += sats;
                coins.push(TransferredCoin { statechain_id: sid, amount_sats: sats });
            }
        }
        let piece_id = coins
            .first()
            .map(|c| c.statechain_id.clone())
            .ok_or_else(|| anyhow!("the multi-carrier coloured payment produced no piece"))?;
        Ok(Some(ColoredTransferOut {
            result: TransferResult {
                receiver_address: receiver_address.to_string(),
                total_sats,
                coins,
                used_split: true,
            },
            piece_id,
            batch_id: None,
            se_hash: None,
        }))
    }

    /// **[CTES-R] Re-transfer an adopted COLOURED CHILD off-chain, whole.**
    ///
    /// The coloured sibling of `transfer::child_retransfer`, and the reason it has to exist:
    /// `ext_child`'s payload output is a SEALED output, so the plain re-transfer would replace the
    /// child's state with an RGB-unaware transaction and BURN the allocation. That is exactly what
    /// `tesr::refuse_uncolored_over_colored_child` refuses, and this is the route it points at.
    ///
    /// The new state `S'_child` rivals the current one over the same outpoint at a strictly lower
    /// CSV, so it matures first; the seal rung folds in that CSV, so the two transitions cannot
    /// collapse to one `OpId`. The recipient adopts it through the ordinary child claim path and
    /// books it with [`Self::accept_colored_child_bundle`].
    pub async fn transfer_colored_child(
        &self,
        child_statechain_id: &str,
        receiver_address: &str,
    ) -> Result<()> {
        let cb = mercuryrustlib::tesr::load_child(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            child_statechain_id,
        )
        .await?
        .ok_or_else(|| anyhow!("statechain id {child_statechain_id} is not an adopted child"))?;
        if !cb.is_colored() {
            return Err(anyhow!(
                "child {child_statechain_id} is PLAIN — use the plain child re-transfer path"
            ));
        }
        let mut child_coin = self.confirmed_coin(child_statechain_id).await?;

        // PHASE 1 — engine only (the `!Sync` resolver rule).
        let draft = {
            let mut rgb = self.rgb().await?;
            let w = rgb.as_mut().ok_or_else(|| anyhow!("no RGB engine configured"))?;
            tokio::task::block_in_place(|| -> Result<_> {
                let draft = mercuryrustlib::tesr::build_colored_child_retransfer(
                    w,
                    &cb,
                    receiver_address,
                )?;
                // PROVE the chain the receiver will be handed resolves, BEFORE the co-sign.
                let txids = vec![
                    cb.parent.trigger.txid.clone(),
                    cb.parent.current().extension.txid.clone(),
                    cb.parent.current().state.txid.clone(),
                    cb.child_extension.txid.clone(),
                    draft.tier.txid.clone(),
                ];
                let (verdict, detail, contract) =
                    w.validate_offchain_chain_info(&draft.tier.consignment, &txids)?;
                if verdict != ValidationVerdict::Valid {
                    return Err(anyhow!(
                        "refusing to re-transfer child {child_statechain_id}: the coloured \
                         S'_child consignment does not validate against its chain ({verdict:?}): {}",
                        detail.unwrap_or_default()
                    ));
                }
                let want = cb.rgb.as_ref().expect("is_colored").contract_id.clone();
                if contract.as_deref() != Some(want.as_str()) {
                    return Err(anyhow!(
                        "refusing to re-transfer child {child_statechain_id}: its consignment is \
                         for contract {contract:?}, not the child's {want}"
                    ));
                }
                let assigned = w.accept_offchain_amount(
                    &draft.tier.consignment,
                    &txids,
                    &draft.tier.txid,
                    draft.tier.payload_vout,
                )?;
                if assigned != draft.rgb_amount {
                    return Err(anyhow!(
                        "refusing to re-transfer child {child_statechain_id}: S'_child assigns \
                         {assigned} to the recipient but the child carries {}",
                        draft.rgb_amount
                    ));
                }
                Ok(draft)
            })?
        };

        // PHASE 2 — network only: one blind SE co-sign, then the standard child conveyance.
        mercuryrustlib::tesr::cosign_colored_child_retransfer(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            &mut child_coin,
            &cb,
            draft,
            receiver_address,
        )
        .await?;

        // The child has left this wallet: its allocation went with `S'_child`, so the engine must
        // stop advertising it (and stop quarantining the coin from plain-BTC selection).
        let child_op = format!(
            "{}:{}",
            child_coin.utxo_txid.clone().unwrap_or_default(),
            child_coin.utxo_vout.unwrap_or_default()
        );
        {
            let rgb = self.rgb().await?;
            let w = rgb.as_ref().ok_or_else(|| anyhow!("no RGB engine configured"))?;
            tokio::task::block_in_place(|| w.mark_spent(&[child_op]))?;
        }
        self.set_coin_status(child_statechain_id, CoinStatus::WITHDRAWN).await?;
        Ok(())
    }

    /// Multi-carrier colored transfer: when no single carrier holds `token_amount`, COMBINE several
    /// carriers of `asset_id` into one payment (piece + change) in a single SE-co-signed colored
    /// combine tx (N inputs → 2 outputs). Every combined carrier is made terminal first; the receiver
    /// validates the multi-input branch and (via the per-structural-input terminal check) requires
    /// ALL N carriers to be terminal. Caller MUST hold `wallet_lock` (this runs inside
    /// `colored_transfer`'s lock and does not re-take it).
    async fn colored_combine_transfer(
        &self,
        asset_id: &str,
        receiver_address: &str,
        token_amount: u64,
        latch: ColoredLatch,
        record: mercurylib::wallet::Wallet,
        allocations: Vec<(String, u64, bool)>,
        banned: &[String],
    ) -> Result<ColoredTransferOut> {
        // The RETIRED lane: this builds one `create_colored_combine_tx` spending every input
        // carrier's funding output `F`. Its only caller gates it too, on the SUPERSET of candidates
        // — this is the copy that makes the gate a property of the ROUTE rather than of one call
        // site, and it is what `retired_split_lane_census` checks. It now runs AFTER selection
        // (step 1 below is read-only, terminalizes nothing and co-signs nothing) so it can be handed
        // the EXACT inputs whose `F` this transaction will spend, which is what the migration hatch
        // has to reason about; the caller's superset gate has already refused anything looser.
        // 1. Select a minimal set of confirmed, settled carriers of this asset, largest allocation
        //    first, until their allocations sum to >= token_amount; then top up with more carriers
        //    if the summed SATS cannot fund the piece + change + fee.
        let min_output =
            crate::transfer::min_split_output(crate::transfer::backup_fee_rate(&self.inner.cc).await?);
        let mut candidates: Vec<(mercurylib::wallet::Coin, u64)> = Vec::new();
        for coin in record.coins.iter() {
            if coin.status != CoinStatus::CONFIRMED || coin.duplicate_index != 0 {
                continue;
            }
            // F7 fail-closed: a carrier whose structural co-signature was consumed by a lost spend
            // is exit-only; combining it would produce a branch the SE can never complete.
            if coin
                .statechain_id
                .as_deref()
                .map_or(false, |sid| banned.iter().any(|b| b == sid))
            {
                continue;
            }
            let op = format!(
                "{}:{}",
                coin.utxo_txid.clone().unwrap_or_default(),
                coin.utxo_vout.unwrap_or_default()
            );
            if let Some((_, amt, _)) =
                allocations.iter().find(|(o, _, settled)| *o == op && *settled)
            {
                candidates.push((coin.clone(), *amt));
            }
        }
        let total_alloc: u64 = candidates.iter().map(|(_, a)| *a).sum();
        if total_alloc < token_amount {
            return Err(anyhow!(
                "insufficient {asset_id}: wallet holds {total_alloc} across {} carrier(s), need {token_amount}",
                candidates.len()
            ));
        }
        // Largest allocation first (fewest inputs); the combine reserve grows with total sats.
        candidates.sort_by(|a, b| b.1.cmp(&a.1));
        let mut selected: Vec<(mercurylib::wallet::Coin, u64)> = Vec::new();
        let mut sel_alloc = 0u64;
        let mut sel_sats = 0u64;
        for c in candidates.into_iter() {
            if sel_alloc >= token_amount
                && sel_sats > TOKEN_PIECE_SATS + (sel_sats / 100).clamp(300, 2_000) + min_output
            {
                break;
            }
            sel_sats += c.0.amount.unwrap_or_default() as u64;
            sel_alloc += c.1;
            selected.push(c);
        }
        if selected.len() < 2 {
            // A single carrier would have been found by the caller's scan; <2 here means the only
            // sufficient carrier is a token carrier we already rejected, so treat as unsupported.
            return Err(anyhow!(
                "no combination of carriers covers {token_amount} of {asset_id} with enough sats"
            ));
        }
        // THE GATE, on the exact inputs. Nothing above this line is irreversible.
        let selected_coins: Vec<mercurylib::wallet::Coin> =
            selected.iter().map(|(c, _)| c.clone()).collect();
        self.refuse_legacy_colored_split_lane("combining several carriers", &selected_coins)
            .await?;

        // 2. Amounts. Piece carries the exact token_amount; change keeps the rest across all inputs.
        let combined_sats: u64 = selected.iter().map(|(c, _)| c.amount.unwrap_or_default() as u64).sum();
        let combined_alloc: u64 = selected.iter().map(|(_, a)| *a).sum();
        let fee_reserve = (combined_sats / 100).clamp(300, 2_000);
        if TOKEN_PIECE_SATS + fee_reserve + min_output >= combined_sats {
            return Err(anyhow!(
                "combined carriers hold too few sats ({combined_sats}) to fund a token piece + change + fee at the current feerate"
            ));
        }
        let change_sats = combined_sats - TOKEN_PIECE_SATS - fee_reserve;
        let token_change = combined_alloc - token_amount;
        if TOKEN_PIECE_SATS < min_output || change_sats < min_output {
            return Err(anyhow!(
                "combine output below the minimum viable size {min_output} (piece {TOKEN_PIECE_SATS}, change {change_sats}) — a sub-coin could not fund its own backup"
            ));
        }

        // 3. Fresh slots for the piece + change — DERIVED slots; any consumed input carrier can
        //    vouch (they are all being re-housed by this combine), so use the first selected.
        let voucher_parent = selected[0]
            .0
            .statechain_id
            .clone()
            .ok_or_else(|| anyhow!("selected carrier without statechain id"))?;
        let mut slot_tokens = self.take_derived_tokens(&voucher_parent, 2).await?;
        let token_a = slot_tokens.remove(0);
        let piece_addr = mercuryrustlib::deposit::get_deposit_bitcoin_address(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            &token_a,
            u32::try_from(TOKEN_PIECE_SATS)?,
        )
        .await?;
        let token_b = slot_tokens.remove(0);
        let change_addr = mercuryrustlib::deposit::get_deposit_bitcoin_address(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            &token_b,
            u32::try_from(change_sats)?,
        )
        .await?;

        // 4. Make EVERY input carrier terminal at the SE before co-signing the combine — so none can
        //    be double-spent to invalidate the branch (the receiver independently verifies this).
        let carrier_ids: Vec<String> = selected
            .iter()
            .map(|(c, _)| c.statechain_id.clone().unwrap_or_default())
            .collect();
        // [CTES-R] Same interlock as the single-carrier lane, per INPUT: one coloured-laddered
        // carrier anywhere in the combine is one rival spend of a funding output, which is enough.
        for id in &carrier_ids {
            self.refuse_if_colored_ladder(id).await?;
        }
        let carrier_ops: Vec<String> = selected
            .iter()
            .map(|(c, _)| {
                format!(
                    "{}:{}",
                    c.utxo_txid.clone().unwrap_or_default(),
                    c.utxo_vout.unwrap_or_default()
                )
            })
            .collect();

        // F7 WRITE-AHEAD: the plan is durable BEFORE the first carrier is terminalized. A crash
        // anywhere in the loop below leaves an entry the recovery reader classifies per carrier —
        // and it takes only ONE terminal carrier to make the whole combine unrepeatable.
        let mut journal = StructuralSpendRecord {
            op_id: uuid::Uuid::new_v4().to_string(),
            lane: "colored_combine".to_string(),
            stage: StructuralStage::Prepared,
            asset_id: asset_id.to_string(),
            receiver_address: receiver_address.to_string(),
            token_amount,
            token_change,
            carrier_ids: carrier_ids.clone(),
            carrier_ops: carrier_ops.clone(),
            slot_tokens: vec![token_a.clone(), token_b.clone()],
            piece_addr: piece_addr.clone(),
            change_addr: change_addr.clone(),
            piece_sats: TOKEN_PIECE_SATS,
            change_sats,
            latched: !matches!(latch, ColoredLatch::None),
            signed_tx: None,
            txid: None,
            piece_vout: None,
            change_vout: None,
            consignment: None,
            blinding: None,
            piece_id: None,
            change_id: None,
            batch_pieces: Vec::new(),
        };
        self.journal_write(&journal).await?;

        for id in &carrier_ids {
            mercuryrustlib::lightning_latch::set_spend_budget(
                &self.inner.cc,
                &self.inner.config.wallet_name,
                id,
                1,
            )
            .await?;
        }

        // 5. Build + per-input blind-MuSig2 co-sign the un-broadcast colored combine (locktime 0).
        let server_info = mercuryrustlib::utils::info_config(&self.inner.cc).await?;
        let mut input_coins: Vec<mercurylib::wallet::Coin> =
            selected.iter().map(|(c, _)| c.clone()).collect();
        let splits: Vec<(String, u64, u64)> = vec![
            (piece_addr.clone(), TOKEN_PIECE_SATS, token_amount),
            (change_addr.clone(), change_sats, token_change),
        ];
        // A combine has N parent outpoints, so its seal is keyed on the whole input set. Two
        // combines over the identical set cannot both be co-signed — step 4 above set every input's
        // spend budget to 1 — so `(input set, Combine, arity)` is unique in practice. That is an
        // argument, not a check: `create_colored_combine_tx` colours through `RgbWallet::color`,
        // which has no guard of its own, so `assert_own_witness` is applied to the result below.
        let combine_blinding = TierSeal::new(
            carrier_ids.join("+"),
            TierRole::Combine,
            0,
            splits.len() as u32,
        )
        .blinding();
        let combine = {
            let rgb = self.rgb().await?;
            let w = rgb.as_ref().unwrap();
            mercuryrustlib::rgb::create_colored_combine_tx(
                &self.inner.cc,
                w,
                &mut input_coins,
                asset_id,
                &splits,
                1,
                false,
                None,
                &self.inner.config.network.to_string(),
                server_info.initlock,
                server_info.interval,
                combine_blinding,
            )
            .await?
        };
        // The collision assert the seal derivation above relies on — fail closed BEFORE the signed
        // material is journalled, so the recovery reader can never replay a consignment that carries
        // a rival's witness. The co-signatures ARE spent at this point, so the entry is closed as
        // `Stranded`: those carriers are exit-only and must never be picked again.
        if let Err(e) = assert_own_witness(
            "coloured combine",
            &combine.txid,
            &combine.consignment,
            combine.blinding,
        ) {
            self.journal_stage(&mut journal, StructuralStage::Stranded)
                .await?;
            return Err(e);
        }

        // F7 COMMIT POINT: see the single-carrier lane — the signed child material is durable before
        // any of the work that used to be able to lose it.
        journal.signed_tx = Some(combine.signed_tx.clone());
        journal.txid = Some(combine.txid.clone());
        journal.piece_vout = Some(combine.output_vouts[0]);
        journal.change_vout = Some(combine.output_vouts[1]);
        journal.consignment = Some(combine.consignment.clone());
        journal.blinding = Some(combine.blinding);
        self.journal_stage(&mut journal, StructuralStage::Signed)
            .await?;
        crash_point("after_structural_sign");

        // 6/7/8. Sub-coin registration (merged DAG branch), RGB change re-registration, envelope,
        //        optional latch, hand-over — the SAME journalled tail the split lane and the recovery
        //        reader run.
        let (batch_id, se_hash) = self.finish_structural_spend(&mut journal, Some(&latch)).await?;
        let piece_id = journal
            .piece_id
            .clone()
            .ok_or_else(|| anyhow!("colored combine produced no piece id"))?;

        Ok(ColoredTransferOut {
            result: TransferResult {
                receiver_address: receiver_address.to_string(),
                total_sats: TOKEN_PIECE_SATS,
                coins: vec![TransferredCoin {
                    statechain_id: piece_id.clone(),
                    amount_sats: TOKEN_PIECE_SATS,
                }],
                used_split: true,
            },
            piece_id,
            batch_id,
            se_hash,
        })
    }

    /// Send `asset_id` to MANY recipients in a single off-chain colored split: one SE-co-signed
    /// tx carves one piece per recipient (its exact amount) plus this wallet's change. Each piece
    /// is handed over with its own consignment envelope. Returns one `TransferResult` per recipient.
    pub async fn batch_transfer_tokens(
        &self,
        asset_id: &str,
        transfers: &[(String, u64)],
    ) -> Result<Vec<TransferResult>> {
        if transfers.is_empty() {
            return Err(anyhow!("no recipients"));
        }
        let total: u64 = transfers.iter().map(|(_, a)| *a).sum();
        let n = transfers.len();

        let _guard = self.inner.wallet_lock.lock().await;
        // F7, as in `colored_transfer`: heal any half-done structural spend BEFORE picking a carrier
        // so a new batch can never race the replay of an old one over the same coin, and never
        // select a carrier whose co-signature was already consumed by a lost spend.
        self.recover_structural_spends_locked().await?;
        let banned = journal_stranded_carriers(&self.inner.cc.pool, &self.inner.config.wallet_name)
            .await?;
        mercuryrustlib::coin_status::update_coins(&self.inner.cc, &self.inner.config.wallet_name)
            .await?;
        let record = self.record().await?;

        // Carrier: a confirmed coin holding >= total of the asset.
        let allocations = {
            let mut rgb = self.rgb().await?;
            let w = rgb.as_mut().unwrap();
            tokio::task::block_in_place(|| w.list_allocations(asset_id))?
        };
        let mut carrier: Option<(mercurylib::wallet::Coin, u64)> = None;
        for coin in record.coins.iter() {
            if coin.status != CoinStatus::CONFIRMED || coin.duplicate_index != 0 {
                continue;
            }
            // A carrier whose cooperative path is gone can never be co-signed again — fail closed.
            if coin
                .statechain_id
                .as_deref()
                .map_or(false, |sid| banned.iter().any(|b| b == sid))
            {
                continue;
            }
            let op = format!(
                "{}:{}",
                coin.utxo_txid.clone().unwrap_or_default(),
                coin.utxo_vout.unwrap_or_default()
            );
            if let Some((_, amt, _)) = allocations.iter().find(|(o, _, s)| *o == op && *s) {
                if *amt >= total {
                    carrier = Some((coin.clone(), *amt));
                    break;
                }
            }
        }
        let (mut carrier, carrier_amount) = carrier.ok_or_else(|| {
            anyhow!("no confirmed coin carries >= {total} of {asset_id} for the batch")
        })?;
        let carrier_id = carrier.statechain_id.clone().unwrap();
        // [CTES-R] THE LANE FORK, batch half. `build_colored_in_ladder_split` is already N-ary — one
        // `SP` over `X_m` with one payload output per child — so an N-recipient batch is the same
        // construct as a single payment with more children, not a different lane. Routing it here is
        // what keeps a coloured carrier payable to many recipients at once; before this, the batch
        // lane refused every coloured carrier outright, so turning the default on would have made
        // `batch_transfer_tokens` permanently unusable for tokens.
        if self.carrier_is_colored(&carrier_id).await? {
            let pieces = self
                .colored_in_ladder_pay(asset_id, carrier, carrier_amount, transfers)
                .await?;
            return Ok(pieces
                .into_iter()
                .zip(transfers.iter())
                .map(|((sid, sats), (addr, _))| TransferResult {
                    receiver_address: addr.clone(),
                    total_sats: sats,
                    coins: vec![TransferredCoin { statechain_id: sid, amount_sats: sats }],
                    used_split: true,
                })
                .collect());
        }
        // Everything below is the RETIRED lane: one `create_colored_split_tx` over the carrier's
        // funding output `F`. Gated as a whole, then still interlocked per coin.
        self.refuse_legacy_colored_split_lane(
            "an N-recipient batch",
            std::slice::from_ref(&carrier),
        )
        .await?;
        self.refuse_if_colored_ladder(&carrier_id).await?;
        let carrier_sats = carrier.amount.unwrap_or_default() as u64;
        let fee_reserve = (carrier_sats / 100).clamp(300, 2_000);
        let pieces_sats = TOKEN_PIECE_SATS * n as u64;
        if pieces_sats + fee_reserve >= carrier_sats {
            return Err(anyhow!(
                "carrier coin too small ({carrier_sats} sats) for {n} pieces + fee"
            ));
        }
        let change_sats = carrier_sats - pieces_sats - fee_reserve;
        let token_change = carrier_amount - total;

        // Backup-fee floor on every piece (each 1_500 sats) and the change — reject before the
        // carrier is made terminal so a doomed batch never strands it (see the single-transfer note).
        let min_output =
            crate::transfer::min_split_output(crate::transfer::backup_fee_rate(&self.inner.cc).await?);
        if TOKEN_PIECE_SATS < min_output || change_sats < min_output {
            return Err(anyhow!(
                "batch token split output below the minimum viable size {min_output} at the current feerate (piece {TOKEN_PIECE_SATS} sats, change {change_sats} sats) — a sub-coin could not fund its own backup"
            ));
        }

        // One fresh slot per recipient piece + one for change; build the N+1 colored split. All
        // N+1 slots are DERIVED from the carrier (one free voucher batch, one auth nonce).
        let mut slot_tokens = self.take_derived_tokens(&carrier_id, n + 1).await?;
        let mut splits: Vec<(String, u64, u64)> = Vec::with_capacity(n + 1);
        let mut piece_addrs: Vec<String> = Vec::with_capacity(n);
        for (_, amount) in transfers {
            let tk = slot_tokens.remove(0);
            let addr = mercuryrustlib::deposit::get_deposit_bitcoin_address(
                &self.inner.cc,
                &self.inner.config.wallet_name,
                &tk,
                u32::try_from(TOKEN_PIECE_SATS)?,
            )
            .await?;
            splits.push((addr.clone(), TOKEN_PIECE_SATS, *amount));
            piece_addrs.push(addr);
        }
        let change_tk = slot_tokens.remove(0);
        let change_addr = mercuryrustlib::deposit::get_deposit_bitcoin_address(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            &change_tk,
            u32::try_from(change_sats)?,
        )
        .await?;
        splits.push((change_addr.clone(), change_sats, token_change));

        let parent_backups = self.carrier_spend_generation(&carrier_id).await?;
        let server_info = mercuryrustlib::utils::info_config(&self.inner.cc).await?;

        // F7 WRITE-AHEAD: the plan becomes durable BEFORE the carrier is terminalized, so a crash in
        // the pre-signature window is classifiable by the recovery reader instead of silent. This
        // lane had no journal at all until now — see `finish_structural_batch_spend`.
        let carrier_op = format!(
            "{}:{}",
            carrier.utxo_txid.clone().unwrap_or_default(),
            carrier.utxo_vout.unwrap_or_default()
        );
        let mut journal = StructuralSpendRecord {
            op_id: uuid::Uuid::new_v4().to_string(),
            lane: LANE_BATCH_SPLIT.to_string(),
            stage: StructuralStage::Prepared,
            asset_id: asset_id.to_string(),
            // Diagnostics only for this lane: the real per-piece recipients live in `batch_pieces`.
            receiver_address: format!("{n} recipients"),
            token_amount: total,
            token_change,
            carrier_ids: vec![carrier_id.clone()],
            carrier_ops: vec![carrier_op],
            slot_tokens: vec![change_tk.clone()],
            piece_addr: piece_addrs[0].clone(),
            change_addr: change_addr.clone(),
            piece_sats: TOKEN_PIECE_SATS,
            change_sats,
            // A batch is never latched, so a replay may legitimately finish the local rebuild.
            latched: false,
            signed_tx: None,
            txid: None,
            piece_vout: None,
            change_vout: None,
            consignment: None,
            blinding: None,
            piece_id: None,
            change_id: None,
            batch_pieces: transfers
                .iter()
                .zip(piece_addrs.iter())
                .map(|((recipient, amount), addr)| BatchPiece {
                    recipient: recipient.clone(),
                    addr: addr.clone(),
                    sats: TOKEN_PIECE_SATS,
                    token_amount: *amount,
                    vout: None,
                    piece_id: None,
                    handed_over: false,
                })
                .collect(),
        };
        self.journal_write(&journal).await?;

        // One colored split spends the carrier once -> spend budget 1. MUST stay ahead of the
        // co-signature (see the journal module note).
        mercuryrustlib::lightning_latch::set_spend_budget(
            &self.inner.cc,
            &self.inner.config.wallet_name,
            &carrier_id,
            1,
        )
        .await?;
        crash_point("after_structural_terminalize");
        // Same seal derivation as the single-recipient split, but in a DISJOINT rung space. The
        // arity alone does not keep the two lanes apart: this function rejects only an EMPTY
        // recipient list, so `n == 1` is reachable and gives `splits.len() == 2` — exactly the
        // single lane's arity, over the same carrier at the same spend generation, i.e. the byte
        // identical seal. `BATCH_SPLIT_RUNG_FLAG` moves every batch rung out of the single lane's
        // reach at every arity, including 1.
        let split_blinding =
            batch_split_seal(&carrier_id, parent_backups, splits.len() as u32).blinding();
        let split = {
            let rgb = self.rgb().await?;
            let w = rgb.as_ref().unwrap();
            mercuryrustlib::rgb::create_colored_split_tx(
                &self.inner.cc,
                w,
                &mut carrier,
                asset_id,
                &splits,
                parent_backups + 1,
                false,
                None,
                &self.inner.config.network.to_string(),
                server_info.initlock,
                server_info.interval,
                split_blinding,
            )
            .await?
        };

        // F7 COMMIT POINT: the co-signature is spent and irreplaceable, so the signed child material
        // is made durable HERE — before the N+1 sub-coin registrations, the RGB stash mutation, the
        // N envelope writes and the N hand-overs. Everything after this line is replayable from the
        // journal by `recover_structural_spends`.
        journal.signed_tx = Some(split.signed_tx.clone());
        journal.txid = Some(split.txid.clone());
        journal.consignment = Some(split.consignment.clone());
        journal.blinding = Some(split.blinding);
        for (i, p) in journal.batch_pieces.iter_mut().enumerate() {
            p.vout = Some(split.output_vouts[i]);
        }
        journal.piece_vout = Some(split.output_vouts[0]);
        journal.change_vout = Some(split.output_vouts[n]);
        self.journal_stage(&mut journal, StructuralStage::Signed)
            .await?;
        crash_point("after_structural_sign");

        // Registration -> RGB change -> envelopes -> hand-overs, each stage journalled. THE SAME
        // code the recovery reader runs, so a replay cannot drift from the live path.
        self.finish_structural_batch_spend(&mut journal, true).await
    }

    /// Receive-side token hook, called by `claim()` for each newly claimed coin: if its backup
    /// rows carry a consignment envelope, validate the consignment off-chain against the coin's
    /// exit branch and book the balance under the consignment's verified contract id.
    ///
    /// # [D3] Why this path also ACCEPTS the transfer into the RGB stock
    ///
    /// It used to book a received piece with exactly two RGB calls — `import_asset_offchain`
    /// (rgb-lib `save_new_asset`, which imports the CONTRACT and never `accept_transfer`s the
    /// transfer) and `register_statechain` (a SQLITE row). Neither one reveals the seal at the
    /// piece's own funding output and neither one gives the stock an ord for the split/combine
    /// witness. So the receiver's stock held **nothing** at that outpoint, and every later
    /// `color_psbt` over it answered
    /// `Invalid coloring info: total amount in output_map (N) greater than available (0)` — while
    /// `get_asset_balance` cheerfully reported the full amount, because that number comes from
    /// sqlite. The piece was booked, visible, and permanently unspendable: it could not be
    /// coloured (so no CTES-R ladder could ever be built for it) and it could not be split,
    /// combined or paid from.
    ///
    /// `RGB_E2E=16` measures the whole thing with its own control: the SAME consignment over the
    /// SAME outpoint in the SAME wallet goes from `WitnessUnknownToStock` / witness ord `None` /
    /// `available (0)` to `Spendable` / `tentative` / `Ok` when — and only when — `accept_ladder`
    /// is added. Nothing about the piece, its size or its bytes was ever at fault.
    ///
    /// So the legacy lane now makes the same third call the COLOURED lane already makes
    /// ([`Self::accept_colored_ladder`]): [`RgbWallet::accept_ladder`] over the coin's exit branch,
    /// revealing the ONE seal a legacy piece has — its funding output — with the blinding the
    /// sender conveyed on the very backup row that carries the envelope. A legacy piece is simply a
    /// one-rung ladder.
    ///
    /// **This is the same defect as the four `Invalid coloring info` reproductions over a
    /// legacy-lane piece's own funding output** (sdk78 assertion (c), and `RGB_E2E=16` parts B–D):
    /// they were all the receiver never having accepted the transfer, so this closes D3 as well.
    /// It is NOT the E7 class — E7 is a witness the stock HOLDS an ord for and then archives; here
    /// the stock held no ord at all and no bundle was ever invalidated.
    ///
    /// Ordering is deliberate and matches the coloured lane: validate → import → accept → register.
    /// `register_statechain` is LAST because the claim pass's retry rescan skips any coin whose
    /// outpoint is already a booked token carrier, so registering before the accept would make a
    /// failed accept permanent and un-retryable.
    pub(crate) async fn accept_incoming_tokens(&self, statechain_id: &str) -> Result<Option<(String, u64)>> {
        if self.inner.config.rgb_data_dir.is_none() || self.inner.config.rgb_proxy_url.is_none() {
            return Ok(None);
        }
        // [CTES-R] A conveyed COLOURED ladder carries its RGB half in the `tesr-` bundle, not in a
        // backup-row consignment envelope, so it is decided FIRST. `Ok(None)` means "not coloured",
        // and the legacy envelope lane below runs unchanged.
        if let Some(booked) = self.accept_colored_ladder(statechain_id).await? {
            return Ok(Some(booked));
        }
        // [CTES-R] …and a conveyed COLOURED SPLIT CHILD carries its RGB half in the `ctesr-` bundle,
        // which neither of the other two lanes can see. Same contract: `Ok(None)` means "not a
        // coloured child", and the legacy envelope lane below runs unchanged.
        if let Some(booked) = self.accept_colored_child_bundle(statechain_id).await? {
            return Ok(Some(booked));
        }
        // `Ok(None)` is read by `book_incoming_token` as "this coin is plain sats, nothing to
        // record" — a CLEAN ABSENCE that records no status and leaves nothing to retry. The old
        // `Err(_) => return Ok(None)` handed that verdict to every failure of this read, so a coin
        // whose backup rows were momentarily unreadable was silently classified as carrying no
        // token at all. Only a genuinely missing row may say that now; a failed read propagates as a
        // transient error, which keeps the coin quarantined and marked Pending for the next pass.
        let Some(backups) = read_backup_rows(
            &self.inner.cc.pool,
            &self.inner.config.wallet_name,
            statechain_id,
        )
        .await?
        else {
            return Ok(None);
        };
        // [D3] The envelope AND the seal blinding are read off the SAME row, because that is how the
        // sender writes them (`rgb_consignment` + `rgb_blinding` are set together on the piece's
        // first backup row, in both the split and the batch lane) and because a blinding taken from
        // some other row would open a seal that has nothing to do with this consignment.
        let Some((envelope, row_blinding)) = backups
            .iter()
            .find_map(|b| b.rgb_consignment.clone().map(|c| (c, b.rgb_blinding)))
        else {
            return Ok(None);
        };
        let env: ConsignmentEnvelope = serde_json::from_str(&envelope)
            .map_err(|e| anyhow!("malformed consignment envelope: {e}"))?;

        // Branch txids: the un-broadcast witnesses the consignment chain resolves against.
        //
        // This set is THE input to validation. Its old `.unwrap_or_default()` manufactured an EMPTY
        // witness set out of a DB read failure, and an empty set does not fail loudly — it makes the
        // validator try to resolve the branch from the indexer, which cannot see un-broadcast
        // transactions, so it comes back "not valid". Before C2 that verdict was labelled
        // PERMANENT-INVALID and un-quarantined a perfectly genuine carrier; even after C2 it wastes
        // the pass on a question we already knew we could not answer. A missing branch row is still
        // a legitimate shape (an on-chain witness needs no off-chain branch) and yields an empty set
        // — but only when the row is genuinely absent, never when the read failed.
        let raw_branch: Vec<String> = read_backup_rows(
            &self.inner.cc.pool,
            &self.inner.config.wallet_name,
            &format!("branch-{statechain_id}"),
        )
        .await?
        .into_iter()
        .flatten()
        .map(|b| b.tx)
        .collect();
        let txids = branch_witness_txids(&raw_branch)?;

        let record = self.record().await?;
        let coin = record
            .coins
            .iter()
            .find(|c| c.statechain_id.as_deref() == Some(statechain_id) && c.duplicate_index == 0)
            .ok_or_else(|| anyhow!("received coin not found"))?;
        let (txid, vout, sats) = (
            coin.utxo_txid.clone().unwrap_or_default(),
            coin.utxo_vout.unwrap_or_default(),
            coin.amount.unwrap_or_default() as u64,
        );

        let mut rgb = self.rgb().await?;
        let w = rgb.as_mut().unwrap();
        // THE shared predicate (F1) — byte-for-byte the same check the SSP's pre-payment gate runs,
        // including the `booked == env.a` equality. Errors carry the `PERMANENT-INVALID:` prefix that
        // claim() matches to un-quarantine a coin that can never book (so a griefer cannot lock a
        // victim's sats forever by attaching a garbage consignment), as distinct from a TRANSIENT
        // RGB-proxy/indexer error (no prefix), which keeps the coin quarantined and retried.
        let (contract_id, booked) =
            verify_consignment_assignment(w, &env, &txids, &txid, vout)?;
        // [D3] Without the blinding the seal at this coin's funding output cannot be revealed, and a
        // piece booked without it is exactly the un-colourable, unspendable coin this lane used to
        // produce. UN-prefixed on purpose: a missing field is not a verdict the RGB validator
        // reached, so the coin stays QUARANTINED (its allocation protected from a plain-BTC spend)
        // and is retried, rather than being un-quarantined into ordinary sats.
        let seal_blinding = row_blinding.ok_or_else(|| {
            anyhow!(
                "token consignment for {statechain_id} carries no seal blinding (rgb_blinding), so \
                 the allocation at {txid}:{vout} cannot be accepted into the stash and the piece \
                 would be booked but permanently un-colourable — carrier stays QUARANTINED"
            )
        })?;
        tokio::task::block_in_place(|| -> Result<()> {
            // First sight of this contract: import it (genesis + history) into the stash so the
            // allocation rows have their asset to reference — validated against the same branch.
            w.import_asset_offchain(&env.c, &txids)?;
            // [D3] …and ACCEPT the transfer into the stock, revealing the seal at this coin's own
            // funding output. A legacy piece is a ONE-RUNG ladder: `txids` is its exit branch
            // (root-first, ending in the split/combine that carved it) and the single seal is its
            // payload output. This is the call whose absence made every received legacy piece
            // un-colourable; see the method doc.
            let received = w.accept_ladder(&env.c, &txids, &[(txid.clone(), vout, seal_blinding)])?;
            if received != booked {
                // Both numbers come from the same validated consignment, so a disagreement means
                // the seal that was opened is not the one the assignment was read at.
                return Err(anyhow!(
                    "{PERMANENT_INVALID_SENTINEL}: accepting the token consignment booked {received} \
                     at {txid}:{vout}, but the same consignment assigns {booked} there"
                ));
            }
            w.register_statechain(&txid, vout, sats, &contract_id, booked, &[])?;
            Ok(())
        })?;
        Ok(Some((contract_id, booked)))
    }

    /// Validate a PENDING (un-claimed) token transfer's consignment WITHOUT booking it, returning
    /// `(contract_id, booked_amount)` — the amount the consignment *cryptographically* assigns to
    /// the coin's witness outpoint `funding_txid:vout`. Security gate for audit [4]: the SSP's
    /// pre-payment check calls this to verify a latched colored coin actually carries the invoiced
    /// asset + amount BEFORE it pays the Lightning invoice. A HODL swap forces payment before the
    /// coin can be claimed, so the post-claim balance-delta check is a backstop, not the gate; and
    /// the envelope's advisory `env.a` is attacker-controlled, so only this consignment-derived
    /// amount is trustworthy. Read-only: `validate_offchain_chain_info` and `accept_offchain_amount`
    /// load the consignment into a temp dir and query it, mutating neither the stash nor the wallet,
    /// so it does not disturb the later `accept_incoming_tokens` booking.
    ///
    /// F1: this runs the IDENTICAL predicate the claim path runs
    /// (`verify_consignment_assignment`), envelope-equality check included. Previously it omitted the
    /// `booked == env.a` equality, so a payer could mutate the envelope amount, pass this gate, take
    /// the irreversible Lightning payment, and leave the SSP holding a coin that fails at claim.
    pub async fn validate_pending_token(
        &self,
        consignment_env: &str,
        branch_txs: &[String],
        funding_txid: &str,
        funding_vout: u32,
    ) -> Result<(String, u64)> {
        let env: ConsignmentEnvelope = serde_json::from_str(consignment_env)
            .map_err(|e| anyhow!("malformed consignment envelope: {e}"))?;
        let txids = branch_witness_txids(branch_txs)?;
        let mut rgb = self.rgb().await?;
        let w = rgb.as_mut().ok_or_else(|| anyhow!("RGB engine not configured"))?;
        verify_consignment_assignment(w, &env, &txids, funding_txid, funding_vout)
    }
}

/// **THE RETIREMENT ASSERTION, SDK half: no production route reaches the coloured-SPLIT lane
/// without passing the retirement gate first.**
///
/// `mercuryrustlib::rgb::create_colored_split_tx` and `create_colored_combine_tx` are the two
/// primitives that spend a carrier's FUNDING output `F` directly. That is the lane CTES-R replaces,
/// and it is a RIVAL of a coloured trigger `T` over the same outpoint. With `colored_ladder` on it
/// must be unreachable — so every call site must sit behind
/// [`UtexoWallet::refuse_legacy_colored_split_lane`], which refuses outright when the flag is on.
///
/// Like its sibling in `mercuryrustlib::tesr`, this is a grep over this module's own source rather
/// than a behavioural test, and for the same reason: the hazard is a NEW route added later, which no
/// behavioural test anticipates.
/// [S4b] **The lane fork, and the refusal behind it.**
///
/// A coloured spine tip is the sender's own change from a coloured split, and it is a CARRIER: the
/// change is registered with the RGB engine, so selection picks it for a second payment. The fork
/// that decides coloured-vs-plain read only the `tesr-` row, so a tip answered `false` and went down
/// the plain lane — which spends `SP.out[K]` uncoloured and destroys the allocation sitting on it.
///
/// These pin the two halves of the fix over CODE with comments stripped: the detector consults the
/// tip row, and the coloured engine refuses a tip by NAME rather than with a generic "no ladder"
/// message that reads like data loss.
#[cfg(test)]
mod s4b_lane_fork_tests {
    fn code_of(needle: &str) -> String {
        let s = include_str!("tokens.rs");
        let at = s.find(needle).unwrap_or_else(|| panic!("{needle} exists"));
        let end = s[at..].find("\n    /// ").map(|e| at + e).unwrap_or(s.len());
        s[at..end]
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_lane_fork_recognises_a_coloured_spine_tip() {
        let code = code_of("async fn carrier_is_colored(");
        assert!(
            code.contains("load_spine_tip("),
            "`carrier_is_colored` must consult the `spinetip-` row too. A coloured tip answering \
             `false` does not mean 'unsupported' — it means the coin is routed to the PLAIN lane, \
             which spends the outpoint its allocation is booked at and burns it"
        );
        assert!(
            code.contains("is_some_and(|t| t.is_colored())"),
            "and the tip must be reported coloured only when it actually carries an allocation"
        );
    }

    #[test]
    fn a_tip_carrier_is_dispatched_to_the_batch_not_split_as_a_root() {
        let code = code_of("async fn colored_in_ladder_pay(");
        assert!(
            code.contains("colored_spine_batch_pay("),
            "the coloured engine RECEIVES tip carriers (the fork sends them here) and it splits ROOT \
             carriers — `SP` over `X_m`'s payload. A tip's `SP_{{i+1}}` sits over its own funding \
             outpoint, so it must be dispatched to the batch driver, not split as a root"
        );
    }

    /// A batch always leaves a tip. Spending one to zero would end the carrier's payment life with
    /// no funding outpoint for the next level — the whole-allocation move is conveying the tip.
    #[test]
    fn the_batch_refuses_to_spend_its_tip_to_zero() {
        let code = code_of("async fn colored_spine_batch_pay(");
        assert!(
            code.contains("token_change == 0"),
            "a batch with no change leaves no funding outpoint for the next payment"
        );
        assert!(
            code.contains("SplitLegRole::SpineTip") && code.contains("SplitLegRole::Piece"),
            "both floors apply per leg: the change is a TIP (one rung) and the payees are PIECES \
             (two), and one shared number could only carry the tip's cheaper shape by applying it \
             to every payee too"
        );
    }

    /// The spent tip's cap RIVALS the batch's `SP_{i+1}` over the same outpoint. Booking the coin
    /// WITHDRAWN is what stops this wallet's own tower broadcasting that cap and destroying the
    /// allocation it just paid away.
    #[test]
    fn the_spent_tip_is_booked_withdrawn_which_disarms_our_own_tower() {
        let code = code_of("async fn colored_spine_batch_pay(");
        assert!(
            code.contains("CoinStatus::WITHDRAWN"),
            "the spent tip must be booked WITHDRAWN — the tower's child loop gates on the coin's \
             durable status, and an armed tower would race the children with the tip's cap"
        );
        assert!(
            code.contains("persist_spine_tip(") ,
            "and the NEXT tip must be persisted before any conveyance, or a failed hand-over aborts \
             with this wallet's remaining allocation unrecorded"
        );
    }
}

#[cfg(test)]
mod retired_split_lane_census {
    /// The primitives that spend a carrier's `F` directly.
    const LEGACY_SPLIT_PRIMITIVES: &[&str] = &[
        "mercuryrustlib::rgb::create_colored_split_tx(",
        "mercuryrustlib::rgb::create_colored_combine_tx(",
    ];
    const GATE: &str = "refuse_legacy_colored_split_lane(";

    /// Method boundaries inside `impl UtexoWallet` (4-space indent), non-test source only.
    fn production_methods() -> Vec<(String, String)> {
        let src = include_str!("tokens.rs");
        let mut out: Vec<(String, String)> = Vec::new();
        let mut name = String::from("<file scope>");
        let mut body = String::new();
        for line in src.lines() {
            if line.starts_with("#[cfg(test)]") {
                break;
            }
            let decl = line
                .strip_prefix("    pub async fn ")
                .or_else(|| line.strip_prefix("    async fn "))
                .or_else(|| line.strip_prefix("    pub fn "))
                .or_else(|| line.strip_prefix("    fn "))
                .or_else(|| line.strip_prefix("pub async fn "))
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

    /// The whole call graph, not just the immediate caller: a route is safe only if the gate is on
    /// the path from the public entry point to the primitive. The three routes below are the only
    /// ones, and each is asserted to carry the gate itself or to be entered exclusively through a
    /// caller that does.
    #[test]
    fn every_legacy_split_call_site_is_behind_the_retirement_gate() {
        let methods = production_methods();
        let callers: Vec<&(String, String)> = methods
            .iter()
            .filter(|(_, b)| LEGACY_SPLIT_PRIMITIVES.iter().any(|p| b.contains(p)))
            .collect();
        assert!(
            !callers.is_empty(),
            "the census found NO call to the legacy split primitives — either they are finally \
             gone (delete this test and the gate with them) or the source parser has drifted and \
             this test is vacuous"
        );
        // Direct gate, or entered only from a gated caller. `colored_combine_transfer` is the second
        // shape: it is private, has exactly one call site, and that call site gates immediately
        // before it.
        let gated_callers: Vec<&str> = methods
            .iter()
            .filter(|(_, b)| b.contains(GATE))
            .map(|(n, _)| n.as_str())
            .collect();
        let mut ungated: Vec<String> = Vec::new();
        for (name, body) in callers {
            if body.contains(GATE) {
                continue;
            }
            // Not gated in its own body — every caller of it must be.
            let its_callers: Vec<&str> = methods
                .iter()
                .filter(|(n, b)| n != name && b.contains(&format!("self.{name}(")))
                .map(|(n, _)| n.as_str())
                .collect();
            if !its_callers.is_empty()
                && its_callers.iter().all(|c| gated_callers.contains(c))
            {
                continue;
            }
            ungated.push(format!(
                "{name} (callers: {its_callers:?}, gated callers: {gated_callers:?})"
            ));
        }
        assert!(
            ungated.is_empty(),
            "these routes reach the RETIRED coloured-split lane without passing \
             `refuse_legacy_colored_split_lane` — with `colored_ladder` on they would spend a \
             carrier's funding output `F`, rivalling that carrier's own coloured trigger: {ungated:?}"
        );
    }

    /// The gate is only worth anything if it actually keys off the flag being flipped. Asserted
    /// against the source so a future edit that turns it into a no-op fails here rather than in
    /// production.
    #[test]
    fn the_gate_refuses_exactly_when_the_coloured_lane_is_on() {
        let src = include_str!("tokens.rs");
        let start = src
            .find("fn refuse_legacy_colored_split_lane(")
            .expect("the gate exists");
        let body = &src[start..start + 1_200];
        assert!(
            body.contains("if self.inner.config.colored_ladder {"),
            "the retirement gate no longer keys off `SdkConfig::colored_ladder` — it can no longer \
             retire anything"
        );
        assert!(
            body.contains("return Err("),
            "the retirement gate no longer REFUSES; a gate that only warns retires nothing"
        );
        // …and the refusal must still be the DEFAULT answer: the only thing that can turn it into
        // an `Ok` is `migration_hatch_verdict`, whose narrowness is proved below. A gate that
        // reached `Ok` by any other route would be a retirement in name only.
        assert!(
            body.contains("migration_hatch_verdict("),
            "the retirement gate opens on something other than `migration_hatch_verdict` — the \
             hatch is no longer the single, tested reason the retired lane can run"
        );
    }
}

/// **The migration hatch, proved narrow.**
///
/// Behavioural, not a source grep: [`migration_hatch_verdict`] is free-standing precisely so the
/// decision can be driven directly, with no wallet, database, network or SE in the way. Every branch
/// that could WIDEN the hatch is exercised, because widening it is the only way this code loses
/// money — the hatch's whole justification is that a coin under the floor can never carry a rival
/// trigger, and every condition below is one of the ways that claim could stop being true.
#[cfg(test)]
mod migration_hatch_is_narrow {
    use super::{
        migration_hatch_verdict, CarrierMigrationFacts, TIER_COMMITTED_FEE_RATE, TOKEN_PIECE_SATS,
    };
    use mercuryrustlib::tesr::{colored_ladder_floor, COLORED_LADDER_DUST};

    /// The legacy piece size, written out: this is the number in circulation, not a parameter.
    const LEGACY_PIECE: u64 = 1_500;

    fn floor() -> u64 {
        colored_ladder_floor(TIER_COMMITTED_FEE_RATE, COLORED_LADDER_DUST)
    }

    fn carrier(sid: &str, sats: u64, has_ladder: bool, colourable_now: bool) -> CarrierMigrationFacts {
        CarrierMigrationFacts {
            statechain_id: sid.to_string(),
            sats,
            has_ladder,
            colourable_now,
        }
    }

    /// The class the hatch exists for. The size test is what `carrier_migration_facts` evaluates
    /// FIRST (it is `build_colored_ladder`'s own pre-flight), so the numbers are pinned here even
    /// though the verdict itself consumes the already-computed answer.
    #[test]
    fn the_legacy_piece_is_the_class_and_a_current_piece_is_not() {
        let f = floor();
        assert!(
            LEGACY_PIECE < f,
            "the legacy 1_500-sat piece must be un-colourable by SIZE — that is the class the hatch \
             was written for"
        );
        assert!(
            TOKEN_PIECE_SATS >= f,
            "a piece carved by the CURRENT code must clear the floor, so nothing this lane produces \
             re-enters the hatch through the size test"
        );
        migration_hatch_verdict(f, &[carrier("legacy", LEGACY_PIECE, false, false)])
            .expect("an un-laddered carrier that cannot be coloured is the migration class");
        assert!(
            migration_hatch_verdict(f, &[carrier("normal", TOKEN_PIECE_SATS, false, true)]).is_err(),
            "a carrier that CAN be coloured must wait for its ladder, not take the retired lane"
        );
    }

    /// An empty list is not consent. A route that reaches the gate without naming what it is about
    /// to spend has proved nothing, and the gate must not read silence as approval.
    #[test]
    fn an_empty_carrier_list_never_opens_the_hatch() {
        let why = migration_hatch_verdict(floor(), &[]).expect_err("no carriers, no hatch");
        assert!(why.contains("No carrier was named"), "unexpected refusal: {why}");
    }

    /// One colourable input closes the hatch for the WHOLE route. A combine spends every input's
    /// `F` in one transaction, so a single carrier that a `claim()` pass will ladder is a single
    /// rival trigger, and one is enough.
    #[test]
    fn one_colourable_carrier_closes_the_hatch_for_all_of_them() {
        let f = floor();
        let why = migration_hatch_verdict(
            f,
            &[
                carrier("legacy-a", LEGACY_PIECE, false, false),
                carrier("legacy-b", LEGACY_PIECE, false, false),
                carrier("normal", TOKEN_PIECE_SATS, false, true),
            ],
        )
        .expect_err("a colourable carrier in the set must close the hatch");
        assert!(why.contains("normal"), "the refusal must name the carrier that closed it: {why}");
        assert!(
            why.contains("later `claim()` pass"),
            "the refusal must say the coin is WAITING rather than doomed: {why}"
        );
        assert!(
            !why.contains("legacy-a"),
            "only the blocking carrier is named; the un-colourable ones did nothing wrong: {why}"
        );
    }

    /// **Size alone is NOT the class.** sdk78 measured a TOKEN_PIECE_SATS-sized piece — above the floor, its
    /// allocation properly booked — that rgb-lib still refuses to colour. A size-only hatch strands
    /// exactly that coin: it can never be laddered, so it can never be spent or exited either. The
    /// verdict keys on whether a ladder can actually be built, so it covers this coin too.
    #[test]
    fn an_above_floor_carrier_that_cannot_be_coloured_is_still_in_the_class() {
        migration_hatch_verdict(
            floor(),
            &[carrier("stuck-piece", TOKEN_PIECE_SATS, false, false)],
        )
        .expect(
            "a carrier no coloured ladder can be built for has no other way out — the hatch must \
             open for it whatever its size",
        );
    }

    /// Keyed on the rival's EXISTENCE, not on its colour. A carrier holding any ladder at all — even
    /// a plain one, which would itself be a defect — has a trigger that spends `F` with no timelock,
    /// and that is the hazard the retirement gate is about. It dominates `colourable_now`: a coin
    /// that already HAS a ladder is refused whether or not another could be built.
    #[test]
    fn any_ladder_at_all_closes_the_hatch() {
        let f = floor();
        for colourable in [false, true] {
            let why = migration_hatch_verdict(f, &[carrier("laddered", LEGACY_PIECE, true, colourable)])
                .expect_err("a laddered carrier is never in the migration class");
            assert!(
                why.contains("already hold a TES-R ladder"),
                "the refusal must be about the rival trigger, not the size: {why}"
            );
        }
        // …and it dominates the set: one laddered input refuses the whole route.
        // Bound to a local so the `.is_err()` sits ON the `assert!` line: the repo-wide
        // silent-degradation guard skips lines containing `assert`, and a bare `.is_err(),` alone on
        // a continuation line would otherwise need an allowlist entry whose key (`.is_err(),`) is so
        // generic it would silently cover an unrelated future site in this file.
        let verdict = migration_hatch_verdict(
            f,
            &[
                carrier("clean", LEGACY_PIECE, false, false),
                carrier("laddered", LEGACY_PIECE, true, false),
            ],
        );
        assert!(verdict.is_err(), "one laddered input closes the hatch for the whole route");
    }

    /// The floor is a function of the PROTOCOL fee rate, so a coin's membership of the class cannot
    /// drift with the mempool. Pinned here because the hatch's safety argument — "this can never be
    /// coloured" — is only permanent if the number it is compared against is.
    #[test]
    fn the_floor_is_a_protocol_constant_not_a_market_rate() {
        assert_eq!(
            mercurylib::tesr::TesrParams::mainnet().committed_fee_rate,
            TIER_COMMITTED_FEE_RATE
        );
        assert_eq!(
            mercurylib::tesr::TesrParams::regtest().committed_fee_rate,
            TIER_COMMITTED_FEE_RATE
        );
        assert_eq!(floor(), 2_058, "the coloured root floor at the protocol rate ([D4]: 3*576 + 330)");
    }
}

#[cfg(test)]
mod piece_floor_tests {
    use super::{PIECE_FEE_RATE_HEADROOM, TIER_COMMITTED_FEE_RATE, TOKEN_PIECE_SATS};
    use mercuryrustlib::tesr::{colored_child_floor, colored_ladder_floor, COLORED_LADDER_DUST};

    /// **The derivation of [`TOKEN_PIECE_SATS`], executable.**
    ///
    /// `TOKEN_PIECE_SATS` cannot be a `const fn` of the fee rate — `committed_fee_for_outputs` is
    /// `f64` arithmetic with a `ceil()` — so the constant is written out and this test IS the
    /// derivation. It fails if the constant ever falls below the coloured root floor, which is the
    /// failure that strands received pieces once the flat lane is gone.
    #[test]
    fn token_piece_sats_is_the_coloured_root_floor() {
        // The rate is a protocol constant on BOTH networks; if that stops being true the piece floor
        // has to be re-derived at whichever rate is higher.
        assert_eq!(
            mercurylib::tesr::TesrParams::mainnet().committed_fee_rate,
            TIER_COMMITTED_FEE_RATE
        );
        assert_eq!(
            mercurylib::tesr::TesrParams::regtest().committed_fee_rate,
            TIER_COMMITTED_FEE_RATE
        );

        let root = colored_ladder_floor(TIER_COMMITTED_FEE_RATE, COLORED_LADDER_DUST);
        let child = colored_child_floor(TIER_COMMITTED_FEE_RATE, COLORED_LADDER_DUST);
        // Both floors, stated as the task requires — three coloured rungs + dust, two + dust.
        // RE-DERIVED by [D4], not weakened: a coloured rung is 576 sat (168 vB * 2 + 240), because
        // rgb::COLORED_TIER_VBYTES = 168 is MEASURED on a production-finalised tier and the old 167
        // omitted the SIGHASH_ALL byte on the taproot signature.
        assert_eq!(root, 3 * 576 + 330, "coloured ROOT floor at 2 sat/vB");
        assert_eq!(root, 2_058);
        assert_eq!(child, 2 * 576 + 330, "coloured CHILD floor at 2 sat/vB");
        assert_eq!(child, 1_482);

        // THE TRAP THE OLD VALUE SAT IN, pinned so it can never be re-entered: 1_500 clears the
        // child floor (so a split carves the piece) but not the root floor (so its receiver cannot
        // ladder it). Anything in `(child, root)` is unclaimable-by-construction once the flat lane
        // is retired.
        assert!(1_500 > child && 1_500 < root, "the legacy 1_500 sat piece was inside the trap");
        assert!(
            TOKEN_PIECE_SATS >= root,
            "a received piece MUST be able to carry a full coloured ROOT ladder: \
             {TOKEN_PIECE_SATS} < {root}"
        );

        // The head-room, as arithmetic: the piece still clears the root floor at a DOUBLED committed
        // fee rate, and the constant is exactly that number (no rounding, no slack picked by hand).
        let drifted = colored_ladder_floor(
            TIER_COMMITTED_FEE_RATE * PIECE_FEE_RATE_HEADROOM,
            COLORED_LADDER_DUST,
        );
        assert_eq!(drifted, 3 * (672 + 240) + 330, "coloured ROOT floor at 4 sat/vB");
        assert_eq!(drifted, 3_066);
        assert_eq!(
            TOKEN_PIECE_SATS, drifted,
            "TOKEN_PIECE_SATS is the coloured root floor at {}x the committed rate",
            PIECE_FEE_RATE_HEADROOM
        );

        // And the child floor at the drifted rate too, so a piece can also be carved as a coloured
        // CHILD of a split at that rate.
        assert!(
            TOKEN_PIECE_SATS
                >= colored_child_floor(
                    TIER_COMMITTED_FEE_RATE * PIECE_FEE_RATE_HEADROOM,
                    COLORED_LADDER_DUST
                )
        );
    }

    /// **The carrier size is derived too, and its capacity is walked, not asserted.**
    ///
    /// Raising the piece without raising the carrier is the silent half of this change: the carrier
    /// would still be admissible, just exhausted after two sends instead of five. So the five sends
    /// are actually executed against the REAL admission guard (`split_amounts_floored` with the real
    /// `min_split_output`), and the SIXTH is required to fail.
    ///
    /// This is the **LEGACY flat coloured-split lane only** — the lane that runs while
    /// `SdkConfig::colored_ladder` is off. The CTES-R lane's very different capacity is
    /// [`the_two_coloured_lanes_have_different_send_depths`].
    #[test]
    fn carrier_supports_the_full_send_depth() {
        use super::{legacy_carrier_sats, LEGACY_CARRIER_SEND_DEPTH, TOKEN_CARRIER_SATS};
        use crate::transfer::{min_split_output, split_amounts_floored};

        let min_output = min_split_output(TIER_COMMITTED_FEE_RATE);
        // The two literals `legacy_carrier_sats` cannot express as a `const fn`, re-derived from the
        // REAL functions so a change to either fails HERE rather than silently under-funding.
        assert_eq!(min_output, 554, "330 dust + 112 vB of backup at 2 sat/vB");
        assert_eq!(super::LEGACY_CARRIER_TAIL, min_output, "the carrier tail IS min_split_output");
        assert_eq!(
            super::LEGACY_SPLIT_RESERVE_FLOOR,
            crate::transfer::split_fee_reserve(TOKEN_CARRIER_SATS),
            "the reserve floors at 300 for carriers this size"
        );
        assert_eq!(TOKEN_CARRIER_SATS, 17_384, "5 * (3066 + 300) + 554");
        assert_eq!(TOKEN_CARRIER_SATS, legacy_carrier_sats(LEGACY_CARRIER_SEND_DEPTH));

        let mut carrier = TOKEN_CARRIER_SATS;
        for send in 1..=LEGACY_CARRIER_SEND_DEPTH {
            let (change, reserve) =
                split_amounts_floored(carrier, TOKEN_PIECE_SATS, min_output).unwrap_or_else(|e| {
                    panic!("send {send} of {LEGACY_CARRIER_SEND_DEPTH} was refused: {e}")
                });
            assert_eq!(reserve, 300, "the fee reserve floors at 300 for carriers this size");
            carrier = change;
        }
        assert_eq!(carrier, min_output, "the last change lands exactly on the output floor");
        assert!(
            split_amounts_floored(carrier, TOKEN_PIECE_SATS, min_output).is_err(),
            "the carrier is sized for exactly {LEGACY_CARRIER_SEND_DEPTH} legacy sends, not more"
        );

        // The capacity is PRESERVED, not invented: the legacy 10_000-sat carrier gave the same five
        // sends at the legacy 1_500-sat piece. Keeping the carrier at 10_000 while the piece moved
        // would have cut it to two — that regression is what this test exists to prevent.
        let mut legacy = 10_000u64;
        let mut legacy_depth = 0;
        while let Ok((change, _)) = split_amounts_floored(legacy, 1_500, min_output) {
            legacy = change;
            legacy_depth += 1;
        }
        assert_eq!(legacy_depth, LEGACY_CARRIER_SEND_DEPTH, "the legacy carrier's capacity");
        let mut unraised = 10_000u64;
        let mut unraised_depth = 0;
        while let Ok((change, _)) = split_amounts_floored(unraised, TOKEN_PIECE_SATS, min_output) {
            unraised = change;
            unraised_depth += 1;
        }
        assert_eq!(unraised_depth, 2, "a 10_000-sat carrier at the new piece size: only 2 sends");
    }

    /// **[P0-6] THE two lanes do not have the same send depth, and the carrier is sized for both.**
    ///
    /// `CARRIER_SEND_DEPTH = 5` used to be a single global constant, and on the CTES-R coloured
    /// in-ladder lane it is simply false: the change of a coloured split is a depth-1 coloured
    /// child, and a coloured child can never be split again. Three things are pinned here:
    ///
    /// 1. the CTES-R requirement, recomputed from the REAL coloured sizing functions along exactly
    ///    the chain `colored_in_ladder_pay` walks (`F → T → X_0 → SP(2) → {piece, change}`);
    /// 2. that [`TOKEN_CARRIER_SATS`] covers BOTH lanes — the carrier is funded at issuance, before
    ///    `SdkConfig::colored_ladder` has decided which lane the spend will take;
    /// 3. that the CTES-R depth cap is STRUCTURAL, by a source census over the three guards in
    ///    `mercuryrustlib::tesr` that enforce it. A census and not a behavioural test for the same
    ///    reason `retired_split_lane_census` is one: the hazard is a future route, and the number it
    ///    would invalidate is a `const` nothing else re-derives.
    #[test]
    fn the_two_coloured_lanes_have_different_send_depths() {
        use super::{
            ctesr_carrier_sats, legacy_carrier_sats, CTESR_CARRIER_SEND_DEPTH,
            LEGACY_CARRIER_SEND_DEPTH, TOKEN_CARRIER_SATS,
        };
        use mercuryrustlib::rgb::{colored_tier_out_total, colored_tier_out_value};
        use mercuryrustlib::tesr::{colored_child_floor, COLORED_LADDER_DUST};

        let rate = TIER_COMMITTED_FEE_RATE;
        assert_eq!(CTESR_CARRIER_SEND_DEPTH, 1, "a coloured child can never be split again");

        // 1. The CTES-R requirement, and it is the exact inverse of the real forward walk: fund a
        //    carrier with it and `T`, `X_0`, `SP` leave precisely piece + floored change.
        let need = ctesr_carrier_sats(CTESR_CARRIER_SEND_DEPTH, rate);
        assert_eq!(need, 6_362, "T 576 + X_0 576 + SP(2) 662 + piece 3_066 + child floor 1_482");
        let after_t = colored_tier_out_value(need, rate).expect("T fits");
        let after_x = colored_tier_out_value(after_t, rate).expect("X_0 fits");
        let at_sp = colored_tier_out_total(after_x, 2, rate).expect("SP with 2 children fits");
        let floor = colored_child_floor(rate, COLORED_LADDER_DUST);
        assert_eq!(
            at_sp,
            TOKEN_PIECE_SATS + floor,
            "the derived carrier leaves exactly one piece plus a change child on the floor"
        );
        // One sat less and the change child falls below its floor — i.e. `need` is TIGHT, so the
        // `>=` below is a real check and not a vacuous one.
        let at_sp_short = colored_tier_out_total(
            colored_tier_out_value(colored_tier_out_value(need - 1, rate).unwrap(), rate).unwrap(),
            2,
            rate,
        )
        .unwrap();
        assert!(at_sp_short - TOKEN_PIECE_SATS < floor, "one sat less strands the change child");

        // 2. The shipped constant is the MAX of the two lanes, because issuance does not know the
        //    lane. Under-sizing is a refusal after the carrier is terminalized; over-sizing only
        //    parks sats in the change child.
        let legacy = legacy_carrier_sats(LEGACY_CARRIER_SEND_DEPTH);
        assert_eq!(TOKEN_CARRIER_SATS, legacy.max(need), "sized for whichever lane is dearer");
        assert!(TOKEN_CARRIER_SATS >= need, "a CTES-R carrier must be able to make its ONE send");
        assert!(
            TOKEN_CARRIER_SATS >= legacy,
            "a LEGACY carrier must be able to make its {LEGACY_CARRIER_SEND_DEPTH} sends"
        );

        // 3. The depth-1 cap is structural. `colored_in_ladder_pay` only ever loads a ROOT `tesr-`
        //    bundle, and these two guards refuse a coloured child that tries to be a parent.
        let tesr_src = include_str!("../../rust/src/tesr.rs");
        for guard in [
            "pub fn refuse_uncolored_over_colored_child(",
            "pub fn colored_child_txids(",
            "MAX_COLORED_ADOPT_DEPTH",
        ] {
            assert!(
                tesr_src.contains(guard),
                "the CTES-R send depth of {CTESR_CARRIER_SEND_DEPTH} rests on `{guard}` in \
                 clients/libs/rust/src/tesr.rs, which is no longer there — either a coloured \
                 child-level split now exists (raise CTESR_CARRIER_SEND_DEPTH and re-derive the \
                 carrier) or the guard was renamed and this census must follow it"
            );
        }
        let ladder_src = include_str!("tokens.rs");
        assert!(
            ladder_src.contains("carrier {carrier_id} has no TES-R ladder to split in-ladder"),
            "colored_in_ladder_pay must keep refusing a carrier without a ROOT `tesr-` bundle — \
             that refusal is the third leg of the depth-1 cap"
        );
    }
}

#[cfg(test)]
mod colored_k_gt_1_tests {
    use super::refuse_colored_multi_payee;

    /// **[K > 1] COLOURED K > 1 IS REFUSED BY NAME, AND K = 1 IS NOT.**
    ///
    /// The line is drawn at exactly one PAYEE, not at one output: a coloured split of a partly-spent
    /// carrier carries a change child too, and that seal is the sender's own. Leaking your own change
    /// seal to the one payee you are already transacting with is the cost §4.5 accepts; leaking payee
    /// *i*'s allocation to payee *j*, who has no relationship with them, is not.
    #[test]
    fn a_coloured_batch_of_one_is_allowed_and_two_is_refused_by_name() {
        refuse_colored_multi_payee(1).expect("coloured K = 1 is the lane that ships");
        refuse_colored_multi_payee(0).expect("an empty list is somebody else's refusal, not this one");

        let e = refuse_colored_multi_payee(2).unwrap_err().to_string();
        assert!(e.contains("coloured K > 1 refused"), "refused BY NAME: {e}");
        assert!(e.contains("SAME seal blinding"), "the refusal must state the reason: {e}");
        assert!(e.contains("build_colored_tier"), "…and name the code it is a property of: {e}");
        assert!(
            e.contains("transfer_tokens"),
            "a refusal that removes a capability must name the route that still works: {e}"
        );
        assert!(
            e.contains("Nothing has been co-signed"),
            "the carrier must be stated untouched — this fires before the split: {e}"
        );

        // The count in the message is the caller's K, not a constant.
        let e19 = refuse_colored_multi_payee(19).unwrap_err().to_string();
        assert!(e19.contains("19 recipients") && e19.contains("in 19 tries"), "{e19}");
    }

    /// **THE ENGINE IS WHERE IT IS REFUSED**, so no route reaches a shared-blinding batch by another
    /// door — and it is refused before the ladder is read, let alone co-signed.
    ///
    /// `batch_transfer_tokens` is not the only caller of `colored_in_ladder_pay`
    /// (`colored_multi_carrier_transfer` is another, and it calls it once per carrier), which is
    /// exactly why the guard cannot live at an entry point.
    #[test]
    fn the_refusal_sits_in_the_engine_ahead_of_every_read_and_every_co_sign() {
        let src = include_str!("tokens.rs");
        let at = src.find("\n    async fn colored_in_ladder_pay(").expect("the engine");
        let body = &src[at..at + src[at..].find("\n    }\n").expect("function ends")];
        let guard = body
            .find("refuse_colored_multi_payee(payouts.len())")
            .expect("the coloured payment engine must refuse K > 1");
        for later in [
            "mercuryrustlib::tesr::load(",           // the carrier's ladder
            "list_allocations",                      // the engine's booked allocation
            "build_colored_in_ladder_split(",        // the draft
            "cosign_colored_in_ladder_split(",       // the terminalizing co-sign
            "take_derived_tokens(",                  // lifetime slot allowance
        ] {
            if let Some(pos) = body.find(later) {
                assert!(
                    guard < pos,
                    "`{later}` runs at {pos}, before the K > 1 refusal at {guard} — a refusal after \
                     the carrier is read is still correct, but one after the co-sign is a carrier \
                     terminalized for a payment that was never allowed"
                );
            }
        }
        // And the only call site that can hand it more than one payout is the batch lane; the other
        // two pass a one-element list. All three go through the engine, so none can dodge the guard.
        // (The needle is assembled at runtime so this assertion does not count ITSELF.)
        let call = format!(".colored_in_ladder_pay{}", "(asset_id");
        assert_eq!(
            src.matches(call.as_str()).count(),
            3,
            "three call sites (transfer_tokens, multi-carrier, batch_transfer_tokens) — a fourth \
             needs re-checking against this guard"
        );
    }
}

#[cfg(test)]
mod envelope_tests {
    use super::ConsignmentEnvelope;

    // The consignment envelope roundtrips through JSON (as stored in BackupTx.rgb_consignment).
    #[test]
    fn envelope_roundtrip() {
        let env = ConsignmentEnvelope { c: "base64data".into(), a: 250, s: 1_500 };
        let json = serde_json::to_string(&env).unwrap();
        let back: ConsignmentEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(back.a, 250);
        assert_eq!(back.s, 1_500);
        assert_eq!(back.c, "base64data");
    }

    // REQ-21 / F1: the envelope amount is only a hint; the consignment-derived amount governs, and a
    // mismatch is rejected. This models the decision (the crypto derivation itself is covered E2E by
    // sdk02/sdk09, and the SSP pre-pay refusal by sdk37).
    //
    // The equality below now lives in exactly ONE place — `verify_consignment_assignment` — which is
    // what both `validate_pending_token` (the SSP's pre-payment gate, before an IRREVERSIBLE Lightning
    // payment) and `accept_incoming_tokens` (claim) call. The F1 defect was that the gate had its own,
    // weaker copy that omitted it. If a future edit re-splits them, sdk37's mutated-envelope assertion
    // is the tripwire.
    #[test]
    fn envelope_amount_is_a_checked_hint() {
        let booked = 250u64; // from the consignment
        let honest = ConsignmentEnvelope { c: "c".into(), a: 250, s: 1500 };
        let lying = ConsignmentEnvelope { c: "c".into(), a: 999, s: 1500 };
        assert_eq!(honest.a, booked); // accepted
        assert_ne!(lying.a, booked); // rejected (ERR-8) — on BOTH paths, pre-pay and claim
    }

    // F1 tripwire: the pre-payment gate and the claim path must resolve to the SAME predicate symbol.
    // A source-level check is the only way to state "these two are one function" — the predicate needs
    // a live RGB engine, so it cannot be exercised in a unit test.
    #[test]
    fn prepay_and_claim_share_one_predicate() {
        let src = include_str!("tokens.rs");
        // Needles are assembled at runtime so this test's own source does not match them.
        let predicate = concat!("verify_consignment_", "assignment(");
        // 1 definition + 2 call sites (validate_pending_token, accept_incoming_tokens).
        assert_eq!(
            src.matches(predicate).count(),
            3,
            "the shared consignment predicate must have exactly its definition and the two call \
             sites (pre-pay gate + claim); a pre-pay check that stops calling it is the F1 hole"
        );
        // And the equality itself must live inside the shared predicate, nowhere else.
        let equality = concat!("if booked != ", "env.a {");
        assert_eq!(
            src.matches(equality).count(),
            1,
            "the `booked == env.a` equality must exist exactly once — a second copy is a predicate \
             that can drift out of sync"
        );
    }
}

/// The two split lanes derive DISJOINT seals over the same carrier at the same spend generation —
/// including at the arity a batch of one recipient produces, which `batch_transfer_tokens` does not
/// reject (it rejects only an EMPTY recipient list).
#[cfg(test)]
mod split_seal_tests {
    // The PRODUCTION derivations, called directly — `transfer_tokens` and `batch_transfer_tokens`
    // now have no rung expression of their own, so changing either lane's seal changes these tests.
    use super::{BATCH_SPLIT_RUNG_FLAG, TierRole, batch_split_seal, single_split_seal};

    /// `transfer_tokens` always builds `splits = [piece, change]`, so its arity is fixed at 2. Pinned
    /// here so the lane-collision tests below cannot drift away from what production actually passes.
    const SINGLE_LANE_ARITY: u32 = 2;

    /// `batch_transfer_tokens` builds `splits = [piece; n] + change`, i.e. arity `n + 1`, and rejects
    /// only an EMPTY recipient list — so `n == 1` (arity 2) is reachable.
    fn batch_arity(recipients: u32) -> u32 {
        recipients + 1
    }

    /// The regression: a ONE-recipient batch has arity 2, exactly like the single lane. Keying the
    /// rung on the arity alone made the two seals identical over the same carrier and generation.
    #[test]
    fn a_one_recipient_batch_does_not_collide_with_the_single_lane() {
        for generation in 0..8u32 {
            assert_eq!(batch_arity(1), SINGLE_LANE_ARITY, "the collision precondition still holds");
            assert_ne!(
                batch_split_seal("sid-carrier", generation, batch_arity(1)).blinding(),
                single_split_seal("sid-carrier", generation, SINGLE_LANE_ARITY).blinding(),
                "a batch of one recipient must not share the single lane's seal at generation \
                 {generation}"
            );
        }
    }

    /// And nothing else in either lane's rung space collides either.
    #[test]
    fn the_two_split_lanes_never_share_a_rung() {
        use std::collections::HashSet;
        let mut seen: HashSet<u64> = HashSet::new();
        let mut n = 0usize;
        for generation in 0..16u32 {
            assert!(
                seen.insert(
                    single_split_seal("sid-carrier", generation, SINGLE_LANE_ARITY).blinding()
                )
            );
            n += 1;
            for recipients in 1..64u32 {
                assert!(
                    seen.insert(
                        batch_split_seal("sid-carrier", generation, batch_arity(recipients))
                            .blinding()
                    ),
                    "batch({recipients}) at generation {generation} reused a blinding"
                );
                n += 1;
            }
        }
        assert_eq!(seen.len(), n, "every split-lane seal must be distinct");
    }

    /// The lanes are disjoint BY CONSTRUCTION, not merely by hash luck: both derive under
    /// `TierRole::Split` over the same carrier and generation, and the only difference production
    /// creates is the flag bit in the rung. Asserting on the pre-image (rather than only on the
    /// 64-bit digest) means a future edit that drops the flag fails here even if the two digests
    /// happened not to collide in the sampled range above.
    #[test]
    fn the_batch_lane_is_separated_by_the_flag_bit_in_the_rung_itself() {
        let single = single_split_seal("sid-carrier", 7, SINGLE_LANE_ARITY);
        let batch = batch_split_seal("sid-carrier", 7, batch_arity(1));
        assert_eq!(single.role, TierRole::Split);
        assert_eq!(batch.role, TierRole::Split);
        assert_eq!(single.statechain_id, batch.statechain_id);
        assert_eq!(single.tier_index, batch.tier_index);
        assert_eq!(single.rung & BATCH_SPLIT_RUNG_FLAG, 0, "the single lane must never set the flag");
        assert_eq!(
            batch.rung & BATCH_SPLIT_RUNG_FLAG,
            BATCH_SPLIT_RUNG_FLAG,
            "the batch lane must always set the flag"
        );
        assert_eq!(batch.rung & !BATCH_SPLIT_RUNG_FLAG, batch_arity(1));
    }
}

/// C2 — a TRANSIENT RGB-resolver failure must never be laundered into a PERMANENT, irreversible
/// un-quarantine of a genuine carrier.
///
/// The un-quarantine is one-way: `book_incoming_token` writes a durable `token-rejected-<id>` row,
/// `consignment_bearing_outpoints` then stops treating the coin as a carrier, and any plain-BTC path
/// may spend it and destroy the RGB allocation. Nothing re-quarantines it. So the ONLY thing allowed
/// to trigger it is a verdict the RGB validator actually reached.
#[cfg(test)]
mod transient_vs_permanent_tests {
    use super::{
        scrub_permanent_sentinel, verdict_rejection, ValidationVerdict, PERMANENT_INVALID_SENTINEL,
    };

    /// THE receiver's real classifier, restated from `wallet.rs::book_incoming_token`:
    /// `if msg.contains("PERMANENT-INVALID") { un-quarantine } else { stay quarantined, retry }`.
    /// Everything below asserts through this so the tests measure the decision production makes.
    fn un_quarantines(err: &anyhow::Error) -> bool {
        err.to_string().contains(PERMANENT_INVALID_SENTINEL)
    }

    /// The headline: the resolver was down, the carrier is genuine, and it stays QUARANTINED.
    #[test]
    fn a_transient_resolver_failure_keeps_a_genuine_carrier_quarantined() {
        // Exactly what rgb-lib reports when a witness cannot be resolved: valid == false with the
        // "resolver" tag. Before C2 the receiver saw only `valid == false` and called it permanent.
        let verdict = ValidationVerdict::from_rgb_lib(false, Some("resolver"));
        assert_eq!(verdict, ValidationVerdict::Unresolved);

        let err = verdict_rejection(verdict, Some("electrum: connection refused"))
            .expect("an unresolved consignment must still be an error — fail closed");
        assert!(
            !un_quarantines(&err),
            "a transient resolver outage must NOT un-quarantine the carrier; got: {err}"
        );
        assert!(
            err.to_string().contains("QUARANTINED"),
            "the transient error should say the carrier is being kept: {err}"
        );
    }

    /// The other half: a real INVALID verdict still un-quarantines, so a griefer cannot lock a
    /// victim's sats forever with a garbage consignment. Hardening must not cost this.
    #[test]
    fn a_genuine_invalid_verdict_still_un_quarantines() {
        let verdict = ValidationVerdict::from_rgb_lib(false, Some("invalid"));
        assert_eq!(verdict, ValidationVerdict::PermanentlyInvalid);
        let err = verdict_rejection(verdict, Some("operation not in consignment"))
            .expect("an invalid consignment is an error");
        assert!(un_quarantines(&err), "a real INVALID verdict must un-quarantine: {err}");
    }

    /// A valid consignment produces no rejection at all.
    #[test]
    fn a_valid_consignment_is_not_rejected() {
        assert_eq!(ValidationVerdict::from_rgb_lib(true, None), ValidationVerdict::Valid);
        assert!(verdict_rejection(ValidationVerdict::Valid, None).is_none());
    }

    /// An `error` tag this bridge has never seen must fail CLOSED — transient, keep the quarantine,
    /// keep retrying — and never be guessed into a permanent rejection.
    #[test]
    fn an_unrecognised_failure_tag_is_treated_as_transient() {
        for tag in [None, Some(""), Some("weird-new-arm"), Some("INVALID"), Some("Resolver")] {
            let verdict = ValidationVerdict::from_rgb_lib(false, tag);
            assert_eq!(
                verdict,
                ValidationVerdict::Unresolved,
                "unrecognised tag {tag:?} must fail closed to transient"
            );
            let err = verdict_rejection(verdict, None).expect("still an error");
            assert!(
                !un_quarantines(&err),
                "unrecognised tag {tag:?} must not un-quarantine: {err}"
            );
        }
    }

    /// The classifier matches on a SUBSTRING, and the detail quoted into a transient error is
    /// attacker-influenced. A griefer who plants the sentinel in that text must not thereby win the
    /// permanent un-quarantine.
    #[test]
    fn an_attacker_cannot_smuggle_the_sentinel_through_a_transient_detail() {
        let hostile = "resolver died while reading PERMANENT-INVALID: pay no attention";
        let err = verdict_rejection(ValidationVerdict::Unresolved, Some(hostile))
            .expect("still an error");
        assert!(
            !un_quarantines(&err),
            "a sentinel planted in untrusted detail must be scrubbed; got: {err}"
        );
        assert!(
            scrub_permanent_sentinel(hostile).contains("<redacted-marker>"),
            "the scrubber should leave a visible trace of what it removed"
        );
    }
}

/// The other half of the class in this file: a database that cannot be READ must never be reported
/// as a database that says NOTHING IS THERE.
/// **[CTES-R] A `tesr-` row nobody can parse is not the same answer as "not a carrier".**
///
/// The silent-degradation class, in its exact CTES-R spelling: `colored_ladder_sids` classified
/// every ladder row with one `filter_map`, and a row that would not deserialize was dropped. That
/// served the exit (refuse — correct) and betrayed the quarantine (not-a-carrier — the coin flows
/// into plain-BTC selection and spending it destroys the allocation). The census now reports the
/// unreadable rows as their own bucket so each caller can choose, and these tests pin the direction.
#[cfg(test)]
mod unreadable_ladder_row_tests {
    use super::classify_tesr_rows;

    fn rows(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter().map(|(k, j)| (k.to_string(), j.to_string())).collect()
    }

    /// The load-bearing one. A `tesr-` row that will not parse must land in `unreadable` — NOT be
    /// dropped — so the quarantine (which unions the two buckets) still covers that coin.
    #[test]
    fn a_row_that_will_not_parse_is_reported_not_dropped() {
        let (colored, unreadable) = classify_tesr_rows(rows(&[
            ("tesr-corrupt", "{ this is not json"),
            ("tesr-truncated", r#"{"version":1,"statechain_id":"x""#),
        ]));
        assert!(
            colored.is_empty(),
            "an unparseable row is not evidence of a coloured walk: {colored:?}"
        );
        assert_eq!(
            unreadable.len(),
            2,
            "both unparseable rows must be REPORTED, not silently dropped: {unreadable:?}"
        );
        assert!(unreadable.contains("corrupt") && unreadable.contains("truncated"));
    }

    /// …and the quarantine's union therefore contains it, while the exit's PROVEN set does not.
    /// This is the whole asymmetry in one assertion: the same row, opposite defaults.
    #[test]
    fn the_quarantine_admits_what_the_exit_refuses() {
        let (proven, unreadable) = classify_tesr_rows(rows(&[("tesr-sid1", "not json")]));
        let mut quarantined = proven.clone();
        quarantined.extend(unreadable);
        assert!(
            !proven.contains("sid1"),
            "unilateral_exit must NOT treat an unreadable bundle as a coloured ladder"
        );
        assert!(
            quarantined.contains("sid1"),
            "plain-BTC selection MUST still be kept off a coin whose bundle could not be read — \
             this is the assertion the old `.ok()?` failed"
        );
    }

    /// Rows that are not `tesr-` rows at all (a `ctesr-` child bundle, a `branch-` witness list, a
    /// coin's own backup row) are none of this function's business and must not become phantom
    /// quarantine entries keyed on a mangled id.
    #[test]
    fn only_tesr_rows_are_classified() {
        let (colored, unreadable) = classify_tesr_rows(rows(&[
            ("ctesr-child", "not json"),
            ("branch-sid", "not json"),
            ("sid-plain-backup", "not json"),
        ]));
        assert!(colored.is_empty(), "{colored:?}");
        assert!(
            unreadable.is_empty(),
            "a non-`tesr-` row must be ignored entirely: {unreadable:?}"
        );
    }
}

/// **[CATS/V4 — F1] The exit-tip registration must see EVERY record shape that can hold an
/// allocation.**
///
/// `mercuryrustlib::tesr::colored_exit_move`'s `match` is exhaustive, so a new record shape cannot
/// silently vanish inside the resolver. It cannot see the other half of the defect: a caller that
/// never CONSTRUCTS one of the variants. That is precisely how the coloured spine tip was lost —
/// the resolution here was an `if let root … else if let child … else { None }` chain, and a tip has
/// neither row, so it took the trailing `else` and came back `Ok(None)`, which
/// `register_exit_tip_best_effort` maps to no event, no fault, no error.
///
/// A source census, for the same reason its two siblings in this file are: the hazard is a route
/// added LATER, which no behavioural test written today anticipates.
#[cfg(test)]
mod exit_tip_registration_census {
    /// The body of `register_colored_exit_tip`, up to the next method at the same indent.
    fn registration_body() -> String {
        let src = include_str!("tokens.rs");
        let start = src
            .find("    pub(crate) async fn register_colored_exit_tip(")
            .expect("`register_colored_exit_tip` exists — this census is about that function");
        let rest = &src[start..];
        // The next sibling item at 4-space indent ends the body.
        let end = rest[1..]
            .find("\n    /// ")
            .map(|at| at + 1)
            .unwrap_or(rest.len());
        rest[..end].to_string()
    }

    /// Every record shape is LOADED and every variant is CONSTRUCTED at the one site that books a
    /// completed coloured exit. Dropping any of the three is silent in production: the coin's
    /// allocation is left advertised at an outpoint its own exit has already spent.
    #[test]
    fn all_three_ladder_record_shapes_are_loaded_and_routed_through_one_resolver() {
        let body = registration_body();
        // Non-vacuity, both ends: the extracted span is a real body, and it stops AT that body —
        // a runaway span would satisfy every assertion below out of the rest of the file.
        assert!(
            body.len() > 500,
            "the body extractor drifted — this census would now be scanning nothing"
        );
        assert!(
            !body.contains("fn refuse_if_colored_ladder("),
            "the extracted span ran past `register_colored_exit_tip` into the next method — the \
             assertions below would then be satisfied by unrelated code"
        );
        for loader in ["tesr::load(", "tesr::load_child(", "tesr::load_spine_tip("] {
            assert!(
                body.contains(loader),
                "`register_colored_exit_tip` no longer reads `{loader}` — a coin backed by that \
                 record shape would complete its coloured exit and keep advertising its allocation \
                 on the outpoint the walk just spent"
            );
        }
        for variant in ["LadderRecord::Root(", "LadderRecord::Child(", "LadderRecord::Tip("] {
            assert!(
                body.contains(variant),
                "`{variant}` is never constructed here — the resolver's exhaustive `match` cannot \
                 catch a shape the CALLER never hands it, which is exactly how the coloured spine \
                 tip fell through"
            );
        }
        assert!(
            body.contains("colored_exit_move"),
            "the registration no longer routes through the single exhaustive resolver — an ad-hoc \
             chain here is what the F1 fallthrough was"
        );
    }
}

#[cfg(test)]
mod absence_vs_unreadable_tests {
    use super::read_backup_rows;

    async fn pool() -> sqlx::Pool<sqlx::Sqlite> {
        sqlx::SqlitePool::connect("sqlite::memory:")
            .await
            .expect("open in-memory sqlite")
    }

    /// No `backup_txs` table at all models an unreadable/absent schema — the read FAILED, so the
    /// caller must be told, not handed an empty answer that reads as "this coin bears no token".
    #[tokio::test]
    async fn an_unreadable_database_is_an_error_not_an_empty_answer() {
        let p = pool().await;
        let out = read_backup_rows(&p, "w", "branch-sid").await;
        assert!(
            out.is_err(),
            "a failed read must propagate; got {:?}",
            out.map(|o| o.map(|v| v.len()))
        );
    }

    /// With the table present and no matching row, the absence is GENUINE and is reported as such —
    /// this is the shape a coin with an on-chain witness (no off-chain branch) legitimately has.
    #[tokio::test]
    async fn a_genuinely_missing_row_is_a_real_absence() {
        let p = pool().await;
        sqlx::query(
            "CREATE TABLE backup_txs (statechain_id TEXT, wallet_name TEXT, txs TEXT)",
        )
        .execute(&p)
        .await
        .expect("create table");
        let out = read_backup_rows(&p, "w", "branch-sid").await.expect("read ok");
        assert!(out.is_none(), "a missing row is a real 'nothing here', not a failure");
    }

    /// And a row that IS there comes back intact.
    #[tokio::test]
    async fn an_existing_row_is_returned() {
        let p = pool().await;
        sqlx::query("CREATE TABLE backup_txs (statechain_id TEXT, wallet_name TEXT, txs TEXT)")
            .execute(&p)
            .await
            .expect("create table");
        let txs = serde_json::json!([{
            "tx_n": 1,
            "tx": "0200beef",
            "client_public_nonce": "",
            "server_public_nonce": "",
            "client_public_key": "",
            "server_public_key": "",
            "blinding_factor": "",
        }])
        .to_string();
        sqlx::query("INSERT INTO backup_txs (statechain_id, wallet_name, txs) VALUES ($1,$2,$3)")
            .bind("branch-sid")
            .bind("w")
            .bind(&txs)
            .execute(&p)
            .await
            .expect("insert");
        let out = read_backup_rows(&p, "w", "branch-sid")
            .await
            .expect("read ok")
            .expect("row present");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].tx, "0200beef");
    }
}

/// F7 — the durable prepare/commit journal that keeps a structural colored spend recoverable across
/// a crash between "the parent's spend budget is terminalized" and "the signed child material is
/// persisted".
///
/// The centrepiece is a REAL process kill (`std::process::abort()`, i.e. SIGABRT — no unwinding, no
/// destructors, no buffered writes flushed) executed by a child process between the two steps. The
/// parent then reopens the same sqlite file and asserts the recovery reader can see exactly what
/// survived. A simulated "crash" that just returns early would prove nothing about durability; only
/// killing the process does.
#[cfg(test)]
mod structural_journal_tests {
    use super::{
        classify_prepared, journal_open_entries, journal_stranded_carriers, journal_upsert,
        StructuralSpendOutcome, StructuralSpendRecord, StructuralStage,
    };

    /// Set on the child process; its value is the stage the child dies AFTER committing.
    const CRASH_AT: &str = "SDK_F7_CRASH_AT";
    /// Sqlite file shared between parent and child.
    const CRASH_DB: &str = "SDK_F7_CRASH_DB";

    fn record(stage: StructuralStage) -> StructuralSpendRecord {
        StructuralSpendRecord {
            op_id: "op-under-test".to_string(),
            lane: "colored_split".to_string(),
            stage,
            asset_id: "rgb:contract-1".to_string(),
            receiver_address: "tml1receiver".to_string(),
            token_amount: 250,
            token_change: 750,
            carrier_ids: vec!["carrier-sid".to_string()],
            carrier_ops: vec!["aa11:0".to_string()],
            slot_tokens: vec!["tok-a".to_string(), "tok-b".to_string()],
            piece_addr: "bcrt1piece".to_string(),
            change_addr: "bcrt1change".to_string(),
            piece_sats: 1_500,
            change_sats: 8_500,
            latched: false,
            signed_tx: None,
            txid: None,
            piece_vout: None,
            change_vout: None,
            consignment: None,
            blinding: None,
            piece_id: None,
            change_id: None,
            batch_pieces: Vec::new(),
        }
    }

    /// The signed child material the old code kept ONLY in process memory — the thing the crash used
    /// to destroy.
    fn sign(rec: &mut StructuralSpendRecord) {
        rec.signed_tx = Some("0200000000010111feedface".to_string());
        rec.txid = Some("f00dbabe".repeat(8));
        rec.piece_vout = Some(0);
        rec.change_vout = Some(1);
        rec.consignment = Some("Y29uc2lnbm1lbnQ=".to_string());
        rec.blinding = Some(0xdead_beef_u64);
        rec.stage = StructuralStage::Signed;
    }

    async fn open_pool(db: &str) -> sqlx::Pool<sqlx::Sqlite> {
        sqlx::SqlitePool::connect(&format!("sqlite:{db}?mode=rwc"))
            .await
            .expect("open sqlite")
    }

    /// The child: journal the operation the way the production path does, then DIE at the requested
    /// point. `prepared` models a kill in the pre-signature window (budget pinned, no co-signature);
    /// `signed` models a kill in the window that used to lose everything — right after the SE
    /// returned the co-signature, before any sub-coin was registered.
    async fn child_role(db: &str, crash_at: &str) -> ! {
        let pool = open_pool(db).await;
        let mut rec = record(StructuralStage::Prepared);
        // Write-ahead: this lands BEFORE `set_spend_budget` in production.
        journal_upsert(&pool, "w", &rec).await.expect("prepare");
        if crash_at == "signed" {
            // ... `set_spend_budget` + the co-signature happen here in production ...
            sign(&mut rec);
            journal_upsert(&pool, "w", &rec).await.expect("signed");
        }
        // Hard kill: no unwinding, no Drop, no flush. Anything readable afterwards is on disk
        // because sqlite fsynced the commit, which is the property under test.
        std::process::abort();
    }

    fn spawn_crashing_child(db: &str, crash_at: &str) -> std::process::Output {
        let exe = std::env::current_exe().expect("test binary path");
        std::process::Command::new(exe)
            .arg("--exact")
            .arg("tokens::structural_journal_tests::a_hard_kill_between_the_two_steps_is_recovered")
            .arg("--nocapture")
            .env(CRASH_DB, db)
            .env(CRASH_AT, crash_at)
            .output()
            .expect("spawn crashing child")
    }

    fn scratch_db(tag: &str) -> String {
        let mut p = std::env::temp_dir();
        p.push(format!("sdk-f7-journal-{tag}-{}.sqlite", uuid::Uuid::new_v4()));
        p.to_string_lossy().to_string()
    }

    #[tokio::test]
    async fn a_hard_kill_between_the_two_steps_is_recovered() {
        // ---- child role -------------------------------------------------------------------
        if let (Ok(db), Ok(at)) = (std::env::var(CRASH_DB), std::env::var(CRASH_AT)) {
            child_role(&db, &at).await;
        }

        // ---- parent role: kill AFTER the signed material was committed ---------------------
        let db = scratch_db("signed");
        let out = spawn_crashing_child(&db, "signed");
        assert!(
            !out.status.success(),
            "the child was supposed to die by abort(), not exit cleanly: {out:?}"
        );
        let pool = open_pool(&db).await;
        let open = journal_open_entries(&pool, "w").await.expect("read journal");
        assert_eq!(open.len(), 1, "the interrupted spend must still be visible after the kill");
        let rec = &open[0];
        assert_eq!(rec.stage, StructuralStage::Signed);
        // THE regression this test exists for: the co-signed child transaction, its consignment and
        // its blinding survived a process kill, so the cooperative path is replayable. Before the
        // journal these lived only in memory and the parent was already terminal at the SE.
        assert_eq!(rec.signed_tx.as_deref(), Some("0200000000010111feedface"));
        assert_eq!(rec.consignment.as_deref(), Some("Y29uc2lnbm1lbnQ="));
        assert_eq!(rec.blinding, Some(0xdead_beef_u64));
        assert_eq!(rec.piece_vout, Some(0));
        assert_eq!(rec.change_vout, Some(1));
        assert_eq!(rec.carrier_ids, vec!["carrier-sid".to_string()]);
        assert_eq!(rec.receiver_address, "tml1receiver");
        // A `Signed` entry is never classified as lost — it is replayed.
        assert!(
            journal_stranded_carriers(&pool, "w").await.unwrap().is_empty(),
            "a replayable entry must not ban its carrier"
        );
        drop(pool);
        let _ = std::fs::remove_file(&db);

        // ---- parent role: kill in the PRE-signature window --------------------------------
        let db = scratch_db("prepared");
        let out = spawn_crashing_child(&db, "prepared");
        assert!(!out.status.success(), "child must abort");
        let pool = open_pool(&db).await;
        let open = journal_open_entries(&pool, "w").await.expect("read journal");
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].stage, StructuralStage::Prepared);
        assert!(open[0].signed_tx.is_none());
        // The recovery reader asks the SE whether the pinned budget was consumed and decides from
        // that alone; both answers are exercised as the pure decision below.
        assert_eq!(classify_prepared(&[false]), StructuralSpendOutcome::Retryable);
        assert_eq!(
            classify_prepared(&[true]),
            StructuralSpendOutcome::CooperativePathLost
        );
        drop(pool);
        let _ = std::fs::remove_file(&db);
    }

    /// Resolved entries leave the reader's view, and a `Stranded` one bans its carriers from any
    /// further colored spend (fail closed — the SE can never co-sign them again).
    #[tokio::test]
    async fn resolved_entries_leave_the_open_set_and_stranded_carriers_are_banned() {
        let db = scratch_db("resolve");
        let pool = open_pool(&db).await;
        let mut rec = record(StructuralStage::Prepared);
        journal_upsert(&pool, "w", &rec).await.unwrap();
        assert_eq!(journal_open_entries(&pool, "w").await.unwrap().len(), 1);

        rec.stage = StructuralStage::Abandoned;
        journal_upsert(&pool, "w", &rec).await.unwrap();
        assert!(journal_open_entries(&pool, "w").await.unwrap().is_empty());
        assert!(journal_stranded_carriers(&pool, "w").await.unwrap().is_empty());

        rec.stage = StructuralStage::Stranded;
        journal_upsert(&pool, "w", &rec).await.unwrap();
        assert!(journal_open_entries(&pool, "w").await.unwrap().is_empty());
        assert_eq!(
            journal_stranded_carriers(&pool, "w").await.unwrap(),
            vec!["carrier-sid".to_string()],
            "a carrier whose co-signature was consumed by a lost spend must never be re-selected"
        );

        // Journals of OTHER wallets are invisible (the reader is per-wallet).
        assert!(journal_open_entries(&pool, "other").await.unwrap().is_empty());
        assert!(journal_stranded_carriers(&pool, "other").await.unwrap().is_empty());
        drop(pool);
        let _ = std::fs::remove_file(&db);
    }

    /// A combine's entry is lost the moment ANY of its carriers went terminal: the combine consumes
    /// all N co-signatures together and cannot be rebuilt from a subset.
    #[test]
    fn one_terminal_carrier_condemns_the_whole_combine() {
        assert_eq!(
            classify_prepared(&[false, false, false]),
            StructuralSpendOutcome::Retryable
        );
        assert_eq!(
            classify_prepared(&[false, true, false]),
            StructuralSpendOutcome::CooperativePathLost
        );
        // No carriers at all is not "safe by default" in the dangerous direction: an empty set can
        // only arise from a malformed record, and it resolves to the retry-safe branch, which never
        // hands anything to a receiver.
        assert_eq!(classify_prepared(&[]), StructuralSpendOutcome::Retryable);
    }
}
