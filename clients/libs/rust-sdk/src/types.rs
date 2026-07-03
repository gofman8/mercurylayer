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
