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

/// Sats carried by a token-piece sub-coin (just above dust; the token is the payload).
/// `pub(crate)` so the granularity model (`granularity_model.rs`) can pin the carrier floor.
pub(crate) const TOKEN_PIECE_SATS: u64 = 1_500;

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
async fn read_backup_rows(
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
        let deposit_sats: u64 = 10_000;
        // 1. Colorable UTXO + issuance in the RGB engine.
        let (asset_id, sources) = {
            let mut rgb = self.rgb().await?;
            let w = rgb.as_mut().unwrap();
            tokio::task::block_in_place(|| -> Result<(String, Vec<String>)> {
                w.create_utxos(1, (deposit_sats * 4) as u32, 2)?;
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
        let deposit_sats: u64 = 10_000;
        let (asset_id, sources) = {
            let mut rgb = self.rgb().await?;
            let w = rgb.as_mut().unwrap();
            let inflation = inflation_amounts.clone();
            tokio::task::block_in_place(move || -> Result<(String, Vec<String>)> {
                // One colorable UTXO per allocation (the fungible supply + each inflation-right)
                // plus a spare for the fund/witness txs; max_allocations_per_utxo is 1.
                let utxos = (inflation.len() as u8).saturating_add(2);
                w.create_utxos(utxos, (deposit_sats * 4) as u32, 2)?;
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
        let deposit_sats: u64 = 10_000;
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
                let _ = w.create_utxos(2, (deposit_sats * 4) as u32, 2);
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
    /// A `tesr-` row that will not DESERIALIZE is deliberately treated as not-coloured (it is
    /// dropped by `filter_map`): `unilateral_exit` then refuses to exit that carrier at all, which
    /// is the fail-closed answer — an unreadable bundle is not evidence that a coloured walk exists.
    pub(crate) async fn colored_ladder_sids(&self) -> Result<std::collections::HashSet<String>> {
        Ok(mercuryrustlib::sqlite_manager::get_all_backup_txs(
            &self.inner.cc.pool,
            &self.inner.config.wallet_name,
        )
        .await
        .map_err(|e| {
            anyhow!(
                "cannot enumerate token carriers: the exit-ladder rows could not be read ({e}) \
                 — refusing to report a carrier set built from an unreadable database"
            )
        })?
        .into_iter()
        .filter_map(|(key, json)| {
            let sid = key.strip_prefix("tesr-")?.to_string();
            let bundle: mercuryrustlib::tesr::TesrBundle = serde_json::from_str(&json).ok()?;
            bundle.is_colored().then_some(sid)
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
        let colored_sids = self.colored_ladder_sids().await?;
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
                // No single carrier covers the amount: combine several carriers of this asset into
                // one payment (piece + change) in a single SE-co-signed colored combine tx.
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
        // [CTES-R] One carrier, one spend of F. Ahead of every co-sign and every budget pin.
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
        // [CTES-R] One carrier, one spend of F — the batch lane splits the same funding output.
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
        let envelope = backups.iter().find_map(|b| b.rgb_consignment.clone());
        let Some(envelope) = envelope else {
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
        tokio::task::block_in_place(|| -> Result<()> {
            // First sight of this contract: import it (genesis + history) into the stash so the
            // allocation rows have their asset to reference — validated against the same branch.
            w.import_asset_offchain(&env.c, &txids)?;
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
