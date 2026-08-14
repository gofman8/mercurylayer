use crate::types::Balance;

/// Wallet events, mirroring the Spark SDK's event set. Emitted by the background watcher
/// (`UtexoWallet::start_background`) and by explicit `claim()` calls.
#[derive(Clone, Debug)]
pub enum WalletEvent {
    /// A deposit reached the confirmation target and is now spendable.
    DepositConfirmed { address: String, amount_sats: u64 },
    /// An incoming transfer was claimed (key handover completed).
    TransferClaimed { statechain_ids: Vec<String> },
    /// An incoming transfer this wallet was expecting was CANCELLED by its sender (with the
    /// recipient's consent, or before the mailbox message was ever posted). The payment will never
    /// complete and no coin will appear.
    ///
    /// This has its own event because the alternative is indistinguishable from silence: the
    /// receiving slot stays empty either way, so an app driven by `TransferClaimed` alone would show
    /// a user an idle mailbox for money that was withdrawn. The claim pass that reports this is
    /// otherwise NORMAL — deposits and other transfers in the same pass are claimed and evented as
    /// usual — so this must not be read as "the pass failed".
    TransferCancelled { statechain_ids: Vec<String> },
    /// Balance changed for any reason (deposit, claim, send).
    BalanceUpdate { balance: Balance },
    /// An incoming token transfer was validated (off-chain consignment) and booked.
    TokenTransferClaimed {
        asset_id: String,
        amount: u64,
        statechain_id: String,
    },
    /// A unilateral-exit branch broadcast hit a mempool conflict: a DIFFERENT transaction is
    /// already spending the branch root, i.e. someone is racing this exit (e.g. a malicious sender
    /// front-running with a competing spend). The exit did NOT go through as-is; the app should
    /// fee-bump / alert / re-attempt rather than assume the coin exited.
    ExitBranchConflict { statechain_id: String },
    /// An off-chain sub-coin is within the safety margin of its exit-race deadline (audit [17]):
    /// an ancestor could soon broadcast a stale backup. `auto_exit_due` broadcasts the locktime-free
    /// exit branch when this fires; an offline owner MUST run a watchtower that does the same.
    ExitDeadlineApproaching { statechain_id: String, deadline_block: u32, tip: u32 },
    /// A coin nearing its backup-ladder floor was automatically **re-anchored** (refreshed): the old
    /// coin is spent on-chain and a fresh full-ladder coin (`amount − fee`) is confirming. Emitted by
    /// the `auto_refresh_due` maintenance pass (the background watcher and the pre-spend hook of
    /// `transfer`), so an aging coin is refreshed before the user's action with only the fee visible.
    CoinRefreshed { old_statechain_id: String, new_statechain_id: String, fee_sats: u64 },
    /// A fresh confirmed deposit had its TES-R (V2) exit ladder auto-established at claim
    /// the coin is now transferable via the R′ path.
    LadderEstablished { statechain_id: String },
    /// A V2 coin was found **triggered** (its funding `F` was spent — a contested exit) and the
    /// owner's watchtower pass (`defend_ladders`) raced the ladder, broadcasting `tiers_broadcast`
    /// tier tx(s) this pass. The adopted current state carries the strictly-lowest CSV, so it matures
    /// first and the funds land at the owner's own key. Emitted per pass until the exit completes.
    LadderDefended { statechain_id: String, tiers_broadcast: u32 },
    /// A received **token-carrier** sub-coin nearing its clawback deadline was automatically
    /// materialized on-chain (its exit branch was broadcast), settling the RGB allocation so a
    /// malicious sender can no longer claw back the shared root. Emitted by `auto_exit_due`; the
    /// plain (RGB-unaware) exit path still refuses carriers, so this is their dedicated protection.
    TokenCarrierMaterialized { statechain_id: String, deadline_block: u32, tip: u32 },
    /// [D13] A **plain** (non-token) split leaf nearing its clawback deadline was automatically
    /// force-exited by `auto_exit_due`. A leaf carries no flat backup of its own and its exit walk
    /// is a chain of relative timelocks, so it must be STARTED `deadline_block` blocks — the
    /// parent's `min(L_k)` less the walk length — before a prior owner of the parent can void it.
    /// Distinct from [`Self::TokenCarrierMaterialized`] on purpose: that event says "a token
    /// allocation was settled"; this one says "a plain sats leaf was driven to L1 to beat its
    /// deadline", and an integrator that treated the two the same would mis-report a plain coin as a
    /// token settlement.
    LeafExitForced { statechain_id: String, deadline_block: u32, tip: u32 },
    /// A CONFIRMED coin was **left on the flat (un-laddered) lane** by the `claim()` ladder pass, so
    /// it has no TES-R exit ladder and cannot be conveyed on the R′ path.
    ///
    /// Laddering is otherwise unconditional, so this is the app's only advance notice that a
    /// particular coin is flat-only — a review flagged the previous silence as a UX defect, because
    /// the owner discovered it at transfer time (or, worse, never). Some reasons are permanent for
    /// the coin ([`LadderSkipReason::RgbCarrier`], [`LadderSkipReason::FundingNotOnChain`]) and some
    /// clear themselves on a later pass ([`LadderSkipReason::CoordinatorUnavailable`]); the event is
    /// emitted only when the recorded reason CHANGES, so a polling watcher does not spam it.
    ///
    /// ⚠️ Because it is transition-only, an app that STARTS after the transition never sees it.
    /// [`crate::UtexoWallet::ladder_skip_reason`] / [`crate::UtexoWallet::flat_only_coins`] read the
    /// same persisted record back on demand and are the authority for "is this coin flat-only?".
    LadderSkipped { statechain_id: String, reason: LadderSkipReason },
    /// 🔴 **A deadline-critical background pass could not SEE, so it did not act.** The wallet is
    /// *blind*, not idle — and the two are indistinguishable from the outside unless this is
    /// surfaced, which is exactly the failure an external review flagged (F3): the near-deadline
    /// watcher enumerated its RGB token carriers with `unwrap_or_default()`, so a storage / indexer /
    /// RGB-engine fault produced an EMPTY carrier set, the materialisation loop found "no carriers"
    /// and the pass reported success while protecting nothing. Every such pass now fails CLOSED and
    /// emits this.
    ///
    /// `detail` is the underlying error, for logs. It is emitted on EVERY failing pass (so a
    /// subscriber that starts late still learns the wallet is blind on the next poll), and the same
    /// state is retained on the wallet — read it back with
    /// [`crate::UtexoWallet::watchtower_faults`] / [`crate::UtexoWallet::is_watchtower_blind`],
    /// which are the authority for "is my protection running right now?".
    ///
    /// ⚠️ An app that holds received off-chain coins or RGB tokens MUST treat this as an alert:
    /// while it persists, nothing is racing a clawback or a hostile trigger on this wallet's behalf.
    WatchtowerBlind { pass: WatchtowerPass, detail: String },
    /// **[CTES-R]** A coloured unilateral exit COMPLETED and its final payload output was registered
    /// with the RGB engine, so `get_asset_balance` / `list_allocations` / `blind_receive` now see the
    /// allocation at the exit tip instead of at the spent funding outpoint.
    ///
    /// The tip pays a Mercury seed-derived key that is not in the engine's BDK descriptor, so no
    /// wallet sync can discover it — this registration is the only thing that moves the engine's view.
    ColoredExitTipRegistered { statechain_id: String, outpoint: String },
    /// 🔴 **[CTES-R]** A coloured unilateral exit completed on chain but the engine could NOT be told
    /// where the allocation went. The coin is safe — the asset is on chain at the owner's own key and
    /// the pre-signed walk is finished — but every UTXO-driven rgb-lib view is now STALE: it still
    /// reports the asset at the SPENT pre-exit outpoint. Re-run the exit pass (it is idempotent) or
    /// re-register the tip by hand before spending the asset.
    ColoredExitTipUnregistered { statechain_id: String, detail: String },
}

/// Which deadline-critical pass reported a [`WalletEvent::WatchtowerBlind`] / a retained
/// [`crate::WatchtowerFault`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum WatchtowerPass {
    /// [`crate::UtexoWallet::auto_exit_due`] — force-exit plain sub-coins and MATERIALIZE received
    /// token carriers before their clawback deadline. Blind ⟹ a received RGB token can be clawed
    /// back by its sender.
    AutoExit,
    /// [`crate::UtexoWallet::defend_ladders`] — the per-block TES-R defence that races a hostile
    /// trigger with the owner's own (strictly-lowest-CSV) tiers. Blind ⟹ a contested exit runs
    /// unopposed.
    DefendLadders,
}

impl WatchtowerPass {
    /// Stable wire spelling (used by the sdk-daemon JSON event stream and every non-Rust wrapper).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::AutoExit => "auto-exit",
            Self::DefendLadders => "defend-ladders",
        }
    }
}

/// Why the `claim()` ladder pass left a coin un-laddered. See [`WalletEvent::LadderSkipped`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LadderSkipReason {
    /// The coin carries an RGB allocation. PERMANENT until CTES-R colouring lands: a plain tier
    /// spend carries no state transition and would DESTROY the allocation (terminal freeze).
    RgbCarrier,
    /// The coin is flagged `single_use` — a terminalized/combine carrier. Same terminal-freeze
    /// reason as [`Self::RgbCarrier`], reached through the flag rather than the allocation set.
    TerminalizedCarrier,
    /// This wallet holds RGB state that could not be read this pass, so the carrier set is unknown.
    /// Fail-closed: nothing in the wallet is laddered rather than risk laddering a carrier.
    RgbStateUnavailable,
    /// [B0] The coin's funding output `F` is not on chain (a split sub-coin's `F` is an un-broadcast
    /// split output), so a ladder trigger would have no prevout to spend. PERMANENT for the coin —
    /// laddering a sub-coin is the in-ladder split's job (`establish_child`), not this pass's.
    ///
    /// Recorded only on POSITIVE evidence (a `branch-<id>` exit chain or a `ctesr-<id>` child
    /// bundle). A funding output that merely could not be looked up is
    /// [`Self::FundingUnresolvable`] instead — this reason licenses flat conveyance, so it must
    /// never be written on a transient chain-backend fault.
    FundingNotOnChain,
    /// The coin's funding output could not be resolved this pass (chain backend unreachable, or a
    /// funding outpoint the wallet cannot parse), so the pass cannot tell whether the coin is an
    /// off-chain sub-coin ([`Self::FundingNotOnChain`], permanent) or an ordinary on-chain root that
    /// simply has not been laddered yet. TRANSIENT: the next pass retries, and it licenses nothing.
    FundingUnresolvable,
    /// The coordinator has no aggregate on record for this statechain id (a pre-migration-0009
    /// legacy coin), so no receiver could bind a ladder built over it. Laddering it would burn three
    /// irreversible SE co-signs for nothing. The coin stays flat, claimable and withdrawable.
    NotBindable,
    /// The coordinator could not be reached (or returned no record) this pass, so bindability is
    /// unknown. TRANSIENT: the next pass retries.
    CoordinatorUnavailable,
    /// **[D69]** No enclave attestation identity is pinned in this build or configured for this
    /// network, so no attestation can be verified and terminality cannot be established.
    /// **PERMANENT** for this build+configuration — retrying changes nothing — and it licenses
    /// NOTHING. Kept distinct from [`Self::CoordinatorUnavailable`] because that one tells the
    /// operator to wait, and waiting for a configuration value to appear is not a plan.
    AttestationIdentityUnpinned,
    /// The ladder could not be bound for a reason that is NOT the pre-0009 legacy case: the funding
    /// output is not v1 taproot, its scriptPubKey would not decode, or the coordinator's aggregate
    /// does not control `F` (a decoy-shaped coin). Distinct from [`Self::NotBindable`] on purpose —
    /// only "the coordinator recorded no aggregate" is the permanent, harmless explanation that
    /// licenses a flat conveyance, and a blanket `is_err()` on the binding precheck used to fold all
    /// of these into it. Licenses NOTHING; the coin needs operator attention.
    BindingUnresolved,
    /// Establishing the ladder failed (SE co-sign refused, signature budget, network). TRANSIENT in
    /// principle — the next pass retries — but a coin stuck on this reason is flat-only in practice,
    /// which is exactly why it is surfaced.
    EstablishFailed,
    /// A duplicate deposit (`duplicate_index != 0`). PERMANENT: a duplicate never becomes index 0,
    /// and the statechain id's ladder belongs to the index-0 coin.
    DuplicateDeposit,
    /// [M2] The coin's `tesr-<sid>` row exists but could not be READ (corrupt / unparseable JSON), so
    /// the pass cannot tell whether the coin already has a ladder. It refuses to establish a second
    /// one over the unreadable row. TRANSIENT-looking but in practice terminal until the row is
    /// restored from a recovery bundle — and it is the single most important case to surface,
    /// because the conveyance path ALSO refuses such a coin (a ladder we cannot read is not a ladder
    /// we may assume away). The coin stays withdrawable and unilaterally exitable throughout.
    LadderUnreadable,
}

impl LadderSkipReason {
    /// Stable wire/DB spelling, persisted in the wallet DB so the conveyance path can tell a
    /// legitimately-flat coin from an unexplained one. Never change these strings casually.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::RgbCarrier => mercuryrustlib::transfer_sender::FLAT_RGB_CARRIER,
            Self::TerminalizedCarrier => mercuryrustlib::transfer_sender::FLAT_TERMINALIZED_CARRIER,
            Self::RgbStateUnavailable => mercuryrustlib::transfer_sender::FLAT_RGB_STATE_UNAVAILABLE,
            Self::FundingNotOnChain => mercuryrustlib::transfer_sender::FLAT_FUNDING_NOT_ONCHAIN,
            Self::FundingUnresolvable => mercuryrustlib::transfer_sender::FLAT_FUNDING_UNRESOLVABLE,
            Self::NotBindable => mercuryrustlib::transfer_sender::FLAT_NOT_BINDABLE,
            Self::CoordinatorUnavailable => mercuryrustlib::transfer_sender::FLAT_COORDINATOR_UNAVAILABLE,
            Self::AttestationIdentityUnpinned => {
                mercuryrustlib::transfer_sender::FLAT_ATTESTATION_UNPINNED
            }
            Self::BindingUnresolved => mercuryrustlib::transfer_sender::FLAT_BINDING_UNRESOLVED,
            Self::EstablishFailed => mercuryrustlib::transfer_sender::FLAT_ESTABLISH_FAILED,
            Self::DuplicateDeposit => mercuryrustlib::transfer_sender::FLAT_DUPLICATE_DEPOSIT,
            Self::LadderUnreadable => mercuryrustlib::transfer_sender::FLAT_LADDER_UNREADABLE,
        }
    }

    /// Parse a persisted spelling back. `None` for a spelling this build does not know — a forward
    /// value written by a newer client, which callers must treat as "flat-only for an unknown
    /// reason", never as "no reason recorded".
    pub fn from_str(s: &str) -> Option<Self> {
        const ALL: &[LadderSkipReason] = &[
            LadderSkipReason::RgbCarrier,
            LadderSkipReason::TerminalizedCarrier,
            LadderSkipReason::RgbStateUnavailable,
            LadderSkipReason::FundingNotOnChain,
            LadderSkipReason::FundingUnresolvable,
            LadderSkipReason::NotBindable,
            LadderSkipReason::CoordinatorUnavailable,
            LadderSkipReason::AttestationIdentityUnpinned,
            LadderSkipReason::BindingUnresolved,
            LadderSkipReason::EstablishFailed,
            LadderSkipReason::DuplicateDeposit,
            LadderSkipReason::LadderUnreadable,
        ];
        ALL.iter().copied().find(|r| r.as_str() == s)
    }

    /// Would this reason LICENSE conveying the coin on the flat lane?
    ///
    /// Only the structurally-permanent reasons would. A transient reason ("we could not decide this
    /// pass") must never harden into "flat is fine forever".
    ///
    /// ⚠️ **A PREDICTION, not the decision.** The authority is
    /// [`mercuryrustlib::transfer_sender::assert_flat_conveyance_is_legitimate`], which re-proves
    /// every licence from live evidence at conveyance time (the `branch-`/`ctesr-` exit material for
    /// [`Self::FundingNotOnChain`], the coin's own `single_use` flag for
    /// [`Self::TerminalizedCarrier`], the coordinator's live answer for [`Self::NotBindable`]).
    /// `false` here is reliable — the classifier is strictly stricter than this predicate — while
    /// `true` means "the classifier will say yes provided the evidence is still there". This exists
    /// so an app can warn ahead of a `send`, never so it can skip the classifier.
    pub fn permits_flat_conveyance(&self) -> bool {
        mercuryrustlib::transfer_sender::is_legitimate_flat_reason(self.as_str())
    }
}
