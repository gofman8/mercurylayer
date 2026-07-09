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
    /// An off-chain sub-coin is within the safety margin of its exit-race deadline (audit [17]):
    /// an ancestor could soon broadcast a stale backup. `auto_exit_due` broadcasts the locktime-free
    /// exit branch when this fires; an offline owner MUST run a watchtower that does the same.
    ExitDeadlineApproaching { statechain_id: String, deadline_block: u32, tip: u32 },
    /// A coin nearing its backup-ladder floor was automatically **re-anchored** (refreshed): the old
    /// coin is spent on-chain and a fresh full-ladder coin (`amount − fee`) is confirming. Emitted by
    /// the `auto_refresh_due` maintenance pass (the background watcher and the pre-spend hook of
    /// `transfer`), so an aging coin is refreshed before the user's action with only the fee visible.
    CoinRefreshed { old_statechain_id: String, new_statechain_id: String, fee_sats: u64 },
    /// A received **token-carrier** sub-coin nearing its clawback deadline was automatically
    /// materialized on-chain (its exit branch was broadcast), settling the RGB allocation so a
    /// malicious sender can no longer claw back the shared root. Emitted by `auto_exit_due`; the
    /// plain (RGB-unaware) exit path still refuses carriers, so this is their dedicated protection.
    TokenCarrierMaterialized { statechain_id: String, deadline_block: u32, tip: u32 },
}
