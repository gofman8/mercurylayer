use crate::types::Balance;

/// Wallet events, mirroring the Spark SDK's event set. Emitted by the background watcher
/// (`SparkWallet::start_background`) and by explicit `claim()` calls.
#[derive(Clone, Debug)]
pub enum WalletEvent {
    /// A deposit reached the confirmation target and is now spendable.
    DepositConfirmed { address: String, amount_sats: u64 },
    /// An incoming transfer was claimed (key handover completed).
    TransferClaimed { statechain_ids: Vec<String> },
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
}
