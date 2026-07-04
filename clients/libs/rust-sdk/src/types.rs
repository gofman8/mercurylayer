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
}

/// A spendable/known coin, for the query API (Spark's leaf/UTXO inventory).
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
    /// Blocks until the backup tx's locktime allows broadcast (0 = ready now).
    pub wait_blocks: u32,
}

impl ExitCostEstimate {
    /// Total miner fee at a given feerate (sat/vB). Branch txs carry their own pre-committed
    /// fees (the split's fee reserve); this covers everything broadcast fresh at `rate`.
    pub fn fee_sats_at(&self, rate_sat_vb: f64) -> u64 {
        (self.total_vbytes as f64 * rate_sat_vb).ceil() as u64
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
        };
        assert_eq!(e.total_vbytes, e.branch_vbytes + e.backup_vbytes);
        assert_eq!(e.fee_sats_at(2.0), 534); // ceil(267*2)
        assert_eq!(e.fee_sats_at(30.0), 8010);
        // fractional rate rounds up
        assert_eq!(e.fee_sats_at(1.5), 401); // ceil(267*1.5=400.5)
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
