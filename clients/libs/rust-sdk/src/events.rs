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
}
