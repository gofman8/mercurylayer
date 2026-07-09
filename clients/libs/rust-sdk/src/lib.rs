//! # mercury-spark-sdk
//!
//! Spark-parity wallet SDK for Mercury Layer statechains with RGB assets.
//!
//! The API mirrors `@buildonspark/spark-sdk` (`SparkWallet`) on a **single-SE** Mercury backend
//! (blind-MuSig2 2-of-2 owner+SE) with **RGB** as the token standard. Every low-level concern —
//! deposit tokens, coin/UTXO management, backup transactions, coin selection, off-chain
//! split/combine for exact amounts, consignments, claim polling — is owned by the SDK; the
//! application deals in addresses and amounts only.
//!
//! ```no_run
//! # async fn demo() -> anyhow::Result<()> {
//! use mercury_spark_sdk::{SparkWallet, SdkConfig};
//!
//! let (wallet, mnemonic) = SparkWallet::initialize(SdkConfig::regtest("alice"), None).await?;
//! let address = wallet.get_spark_address().await?;      // ml1.../tml1... statechain address
//! let balance = wallet.get_balance().await?;            // sats + token balances
//! wallet.transfer(&address, 50_000).await?;             // off-chain, exact amount
//! # Ok(()) }
//! ```

pub mod config;
pub mod events;
#[cfg(test)]
mod granularity_model;
#[cfg(test)]
mod invalidation_model;
pub mod invoice;
pub mod lightning;
pub mod refresh;
pub mod select;
pub mod ssp;
pub mod tokens;
pub mod transfer;
pub mod types;
pub mod wallet;
pub mod watchtower;

pub use config::SdkConfig;
pub use events::WalletEvent;
pub use types::{Balance, ClaimResult, CoinInfo, DepositAddressInfo, ExitCostEstimate, ExitStatus, SdkError, TokenBalance, TokenTx, TransferResult, WithdrawalFeeQuote};
pub use invoice::{SparkInvoice, decode_spark_invoice, encode_spark_invoice};
pub use refresh::RefreshResult;
pub use wallet::SparkWallet;
pub use watchtower::{watch_pass, WatchBundle, WatchEntry};
