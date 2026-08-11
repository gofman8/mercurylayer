use serde::{Deserialize, Serialize};

/// Wallet balance: BTC in sats plus per-asset token balances.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Balance {
    /// Spendable now (confirmed coins).
    pub available_sats: u64,
    /// Detected but not yet spendable (mempool / below confirmation target).
    pub pending_sats: u64,
    /// Outgoing transfers awaiting the receiver's claim.
    pub in_transfer_sats: u64,
    /// RGB asset balances (empty when token support is disabled in config).
    pub tokens: Vec<TokenBalance>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TokenBalance {
    /// RGB contract id (the token identifier).
    pub asset_id: String,
    pub ticker: Option<String>,
    pub name: Option<String>,
    pub precision: u8,
    /// Settled, spendable amount.
    pub balance: u64,
    /// Total including unsettled allocations.
    pub total: u64,
}

/// One statechain coin moved by a transfer.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TransferredCoin {
    pub statechain_id: String,
    pub amount_sats: u64,
}

/// Result of a `transfer` call: the set of coins handed over to the receiver.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TransferResult {
    pub receiver_address: String,
    pub total_sats: u64,
    pub coins: Vec<TransferredCoin>,
    /// True if an off-chain split was performed to mint the exact amount.
    pub used_split: bool,
}

/// Result of a claim pass (incoming transfers + newly confirmed deposits).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ClaimResult {
    pub claimed_transfers: u32,
    pub confirmed_deposits: Vec<DepositAddressInfo>,
    /// Per-statechain outcome for coins that arrived carrying an RGB token consignment (external
    /// review finding 5). A claimed sats transfer and its token booking are separate steps: the
    /// Mercury coin can be CONFIRMED while RGB acceptance is still pending or has failed. These let
    /// an app show token status instead of trusting `claimed_transfers` to mean "tokens received".
    pub token_results: Vec<TokenClaimStatus>,
    /// Incoming transfers this pass found had been CANCELLED by their sender — the payment will
    /// never complete. Reported here rather than raised, so one withdrawn payment cannot discard the
    /// deposits and receipts of the same pass; also emitted as `WalletEvent::TransferCancelled`.
    /// Never counted in `claimed_transfers`.
    pub cancelled_transfers: Vec<String>,
}

/// Outcome of booking an incoming token consignment for one claimed coin.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TokenClaimStatus {
    pub statechain_id: String,
    pub state: TokenClaimState,
    /// Booked asset id + amount when `state == Booked`.
    pub asset_id: Option<String>,
    pub amount: Option<u64>,
    /// Human-readable reason when `state == Pending` or `Rejected`.
    pub detail: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TokenClaimState {
    /// Consignment validated and the allocation is booked.
    Booked,
    /// Consignment present but not yet booked (transient RGB error) — the coin is quarantined from
    /// plain-BTC spends and retried on the next claim pass.
    Pending,
    /// Consignment permanently invalid (griefer's garbage / amount mismatch) — the coin is NOT a
    /// token carrier; its sats are ordinary spendable BTC.
    Rejected,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DepositAddressInfo {
    pub address: String,
    pub amount_sats: u64,
    pub statechain_id: Option<String>,
}

/// Typed SDK errors surfaced to applications (everything else comes through as `anyhow::Error`).
#[derive(Debug, thiserror::Error)]
pub enum SdkError {
    #[error("deposit token payment required: pay {fee_sats} sats to {deposit_address} (token {token_id}), then retry")]
    TokenPaymentRequired {
        token_id: String,
        deposit_address: String,
        fee_sats: u64,
    },
    #[error("insufficient balance: requested {requested_sats} sats, available {available_sats}")]
    InsufficientBalance {
        requested_sats: u64,
        available_sats: u64,
    },
    #[error("no exact coin subset for {requested_sats} sats and off-chain split is disabled for this call")]
    NoExactAmount { requested_sats: u64 },
    #[error("token support is not configured (set rgb_proxy_url + rgb_data_dir)")]
    TokensNotConfigured,
    /// Terminal dust case (B4 / refresh economics): a coin's value is at or below its renewal
    /// (re-anchor) fee, so it cannot pay to keep itself alive and cannot be moved on its own. It is
    /// not lost — combine it with another coin (the aggregate can cover the fee) — but on its own
    /// there is nothing to do. The SDK excludes such coins from routine auto-refresh; this error
    /// surfaces only when a stuck coin is the sole way to fund a payment.
    #[error("coin {statechain_id} ({amount_sats} sats) cannot cover its {fee_sats}-sat renewal fee — its maintenance cost exceeds its value; combine it with another coin to rescue it")]
    CoinBelowMaintenanceCost {
        statechain_id: String,
        amount_sats: u64,
        fee_sats: u64,
    },
    /// **[K > 1 prerequisite 4] The derived-slot allowance bounds a batch.**
    ///
    /// A K-recipient in-ladder batch needs `K + 1` fresh statechain slots (K payee pieces plus the
    /// sender's change), and every slot costs one DERIVED token vouched by the coin being split.
    /// The SE issues at most `max_derived_tokens_per_statechain` of those per parent, counted over
    /// its LIFETIME with spent rows included, so `K + 1` can never exceed the cap: K ≤ 63 at the
    /// default 64. Because each spine level is a fresh statechain, that is a bound PER LEVEL, not a
    /// budget the wallet spends down.
    ///
    /// Refused locally, by name, before any SE call — the server's own refusal is a bare 400 that
    /// arrives after the caller has already committed to a recipient list.
    #[error("a batch of {recipients} recipients needs {slots} statechain slots (one per payee plus the sender's change), but a coin may only ever vouch {cap} derived slots — split the payment into batches of at most {max_recipients} recipients")]
    BatchTooManyRecipients {
        recipients: usize,
        /// `recipients + 1`.
        slots: usize,
        cap: u32,
        /// `cap - 1`.
        max_recipients: usize,
    },
    /// A self-describing decoder refused input whose declared format version it does not support —
    /// named so a caller can tell an unknown-version payload from malformed bytes. (D11: the wire
    /// encodings are frozen; a decoder must reject a version it cannot interpret rather than
    /// mis-parse it.)
    #[error("unsupported {kind} version {found} (this build supports version {supported})")]
    UnsupportedVersion {
        kind: &'static str,
        found: u64,
        supported: u64,
    },
}

/// A cost preview for a send (B4 / refresh economics): the renewal (re-anchor) cost is folded into
/// the transfer's fee and paid on-demand, like a payment fee — so the app can show the user the
/// TOTAL up front instead of a coin silently shrinking in the background. `renewal_fee_sats` is the
/// re-anchor cost this particular send will trigger (0 when no coin it touches is due), so the fee
/// only rises at the renewal boundary.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TransferQuote {
    pub amount_sats: u64,
    /// Reserved miner/split fee for the transfer itself.
    pub network_fee_sats: u64,
    /// On-chain re-anchor cost this send triggers because a coin it uses is due for renewal (0 = none).
    pub renewal_fee_sats: u64,
    /// `network_fee_sats + renewal_fee_sats` — the all-in cost, presented like a payment fee.
    pub total_fee_sats: u64,
    /// Whether the wallet can cover `amount_sats + total_fee_sats` from non-stuck coins.
    pub fundable: bool,
    /// statechain_ids of coins whose value is below their renewal fee (stuck unless combined).
    pub stuck_coins: Vec<String>,
    pub note: String,
}

/// A spendable/known coin, for the query API (the Utexo leaf/UTXO inventory, like Spark's).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CoinInfo {
    pub statechain_id: Option<String>,
    pub amount_sats: u64,
    pub status: String,
    pub utxo_txid: Option<String>,
    pub utxo_vout: Option<u32>,
    /// True for an off-chain sub-coin (its funding tx is un-broadcast).
    pub off_chain: bool,
}

/// Fee quote for a withdrawal / cooperative exit.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WithdrawalFeeQuote {
    pub n_coins: u32,
    pub est_vbytes: u64,
    pub fee_rate_sat_vb: f64,
    pub fee_sats: u64,
}

/// One token-contract transaction (query API): (kind, status, amount, txid).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TokenTx {
    pub kind: String,
    pub status: String,
    pub amount: u64,
    pub txid: String,
}

/// Cost/readiness estimate for unilaterally exiting one coin.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExitCostEstimate {
    pub statechain_id: String,
    /// Number of branch txs that must confirm before the backup (0 for flat coins).
    pub branch_txs: u32,
    pub branch_vbytes: u64,
    pub backup_vbytes: u64,
    pub total_vbytes: u64,
    /// Blocks until the backup tx's locktime allows broadcast (0 = ready now). This is when the
    /// exit COMPLETES (the backup sweeps to your address); it is NOT the safety deadline.
    pub wait_blocks: u32,
    /// SAFETY DEADLINE (block height) for an off-chain sub-coin: the earliest height at which an
    /// ANCESTOR could broadcast its own stale backup and race you. You must broadcast your exit
    /// BRANCH (which is locktime-free) before this height. Computed DEPOSIT-ANCHORED (audit [10]):
    /// the branch root's funding-confirmation height + `initlock` — the maturity of the root
    /// deposit's FIRST backup. If an ancestor was transferred k times before being split, its
    /// retained stale backup matures `k·interval` blocks EARLIER than this value (audit [17]:
    /// ancestor locktimes are not conveyed to receivers), so treat the deadline as an upper bound
    /// and act with a margin — or simply broadcast the branch eagerly; `auto_exit_due` does this.
    /// `None` for a flat on-chain coin (no off-chain ancestor can race you). A watchtower MUST use
    /// this — not `wait_blocks` (which is when the exit COMPLETES) — to decide when to force-exit.
    ///
    /// ⚠️ `None` ALONE IS NOT "SAFE". Read it together with [`Self::exit_deadline_blind`]: `None`
    /// means "safe" only when `exit_deadline_blind` is also `None`. See
    /// [`Self::deadline_is_unknown`].
    pub exit_deadline_block: Option<u32>,
    /// **Why `exit_deadline_block` is `None` (external review C1).** The coin HAS an exit branch —
    /// so a deadline exists and an ancestor CAN race it — but the deadline could not be computed
    /// (unreachable SE config, unreadable chain lookup, or a missing deposit-confirmation history
    /// entry). `Some(reason)` therefore means **"I could not tell"**, never "nothing is due".
    ///
    /// The distinction is the whole point: `deposit_anchored_exit_deadline` used to be built
    /// entirely of `.ok()?`, so any one of those three faults produced the same `None` a genuinely
    /// deadline-free flat coin produces — and `auto_exit_due` skipped the coin, concluding "nothing
    /// is due" from a total inability to tell. That is exactly the protection a received RGB token
    /// depends on. A consumer must treat `Some(_)` here as BLIND: the wallet's own
    /// [`crate::UtexoWallet::auto_exit_due`] routes it into
    /// [`crate::WalletEvent::WatchtowerBlind`] + a retained
    /// [`crate::WatchtowerFault`], and `build_watch_bundle` refuses to export a bundle containing
    /// one.
    ///
    /// Always `None` for a flat coin (no branch ⟹ genuinely no deadline).
    #[serde(default)]
    pub exit_deadline_blind: Option<String>,
}

impl ExitCostEstimate {
    /// Total miner fee at a given feerate (sat/vB). Branch txs carry their own pre-committed
    /// fees (the split's fee reserve); this covers everything broadcast fresh at `rate`.
    pub fn fee_sats_at(&self, rate_sat_vb: f64) -> u64 {
        (self.total_vbytes as f64 * rate_sat_vb).ceil() as u64
    }

    /// `true` iff this coin has a race-able exit branch whose deadline could NOT be computed — i.e.
    /// the caller is BLIND about it and must not read the absent `exit_deadline_block` as "safe".
    pub fn deadline_is_unknown(&self) -> bool {
        self.exit_deadline_blind.is_some()
    }
}

/// Outcome of a unilateral-exit attempt for one coin.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExitStatus {
    pub statechain_id: String,
    /// Branch (if any) and backup both broadcast.
    pub complete: bool,
    /// When not complete: blocks remaining until the backup is final.
    pub wait_blocks: u32,
}

/// Pure mirror of the SE's terminal predicate (§3.6 SPEC): a node is terminal iff a spend budget
/// is set and the finalized-signature count has reached it. Documented here so clients can reason
/// about terminal state; the authoritative value comes from `GET /statechain/spend_budget`.
pub fn is_terminal(sig_budget: Option<i64>, finalized: i64) -> bool {
    matches!(sig_budget, Some(b) if finalized >= b)
}

#[cfg(test)]
mod tests {
    use super::*;

    // INV-17: exit-cost arithmetic.
    #[test]
    fn exit_cost_math() {
        let e = ExitCostEstimate {
            statechain_id: "x".into(),
            branch_txs: 1,
            branch_vbytes: 155,
            backup_vbytes: 112,
            total_vbytes: 267,
            wait_blocks: 990,
            exit_deadline_block: Some(1990),
            exit_deadline_blind: None,
        };
        assert_eq!(e.total_vbytes, e.branch_vbytes + e.backup_vbytes);
        assert_eq!(e.fee_sats_at(2.0), 534); // ceil(267*2)
        assert_eq!(e.fee_sats_at(30.0), 8010);
        // fractional rate rounds up
        assert_eq!(e.fee_sats_at(1.5), 401); // ceil(267*1.5=400.5)
        assert!(!e.deadline_is_unknown(), "a computed deadline is not blind");
    }

    /// [C1] "No deadline" and "I could not compute a deadline" must NOT be the same value.
    ///
    /// Both shapes carry `exit_deadline_block == None`, which is precisely why the old
    /// single-`Option` encoding was load-bearing for the silent-degradation class: a watchtower read
    /// the `None` produced by an unreachable SE / unreadable chain lookup as the `None` produced by
    /// a flat coin with no ancestor, and skipped the coin. `deadline_is_unknown` is the predicate
    /// that separates them; anything deadline-critical must branch on IT, not on the `Option`.
    #[test]
    fn absent_deadline_is_not_the_same_as_uncomputable_deadline() {
        let base = ExitCostEstimate {
            statechain_id: "x".into(),
            branch_txs: 0,
            branch_vbytes: 0,
            backup_vbytes: 112,
            total_vbytes: 112,
            wait_blocks: 0,
            exit_deadline_block: None,
            exit_deadline_blind: None,
        };
        // Flat coin: no branch ⟹ genuinely nothing can race it.
        assert!(!base.deadline_is_unknown(), "a flat coin is NOT blind, it is safe");

        // Branch present but the deadline could not be resolved: same `None`, opposite meaning.
        let blind = ExitCostEstimate {
            branch_txs: 1,
            branch_vbytes: 155,
            total_vbytes: 267,
            exit_deadline_blind: Some("SE config unreachable".into()),
            ..base.clone()
        };
        assert_eq!(
            blind.exit_deadline_block, base.exit_deadline_block,
            "the two shapes are indistinguishable by `exit_deadline_block` alone — that is the bug"
        );
        assert!(
            blind.deadline_is_unknown(),
            "an uncomputable deadline must report BLIND, never 'nothing is due'"
        );
    }

    // §3.6 / REQ-13: terminal predicate.
    #[test]
    fn terminal_predicate() {
        assert!(!is_terminal(None, 5)); // no budget -> never terminal
        assert!(!is_terminal(Some(2), 1)); // budget set, not reached
        assert!(is_terminal(Some(2), 2)); // reached
        assert!(is_terminal(Some(2), 3)); // exceeded
    }

    // ERR-6 / ERR-9: typed error messages carry the actionable fields.
    #[test]
    fn error_semantics() {
        let e = SdkError::TokenPaymentRequired {
            token_id: "tok".into(),
            deposit_address: "bcrt1qx".into(),
            fee_sats: 100,
        };
        let m = e.to_string();
        assert!(m.contains("100") && m.contains("bcrt1qx") && m.contains("tok"));
        let e = SdkError::InsufficientBalance { requested_sats: 200, available_sats: 40 };
        let m = e.to_string();
        assert!(m.contains("insufficient balance") && m.contains("200") && m.contains("40"));
    }
}
