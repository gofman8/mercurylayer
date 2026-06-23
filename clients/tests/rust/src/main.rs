
pub mod electrs;
pub mod bitcoin_core;
pub mod ta01_sign_second_not_called;
pub mod ta02_duplicate_deposits;
pub mod ta03_multiple_deposits;
pub mod tb01_simple_transfer;
pub mod tb02_transfer_address_reuse;
pub mod tb03_simple_atomic_transfer;
pub mod tb04_simple_lightning_latch;
pub mod tb05_timelock;
pub mod tm01_sender_double_spends;
mod tv01;
pub mod rgb01_offchain_split;
pub mod rgb02_combine_transfer;
pub mod rgb03_offchain_chain;
pub mod rgb_dump;
pub mod utils;
use anyhow::{Result, Ok};

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> Result<()> {

    // Off-chain RGB split via pseudo-Spilman leaves (see docs/rgb_offchain_split_spilman.md). Needs
    // the RGB proxy + electrum indexer in addition to the Mercury stack. Run with RGB_E2E=1.
    if std::env::var("RGB_E2E").as_deref() == std::result::Result::Ok("1") {
        rgb01_offchain_split::execute().await?;
        return Ok(());
    }
    // Multi-input "combine" off-chain transition (RGB_E2E=2): spend N statechain coins in one
    // SE-co-signed tx -> recipient + change. See docs/rgb_offchain_split_spilman.md.
    if std::env::var("RGB_E2E").as_deref() == std::result::Result::Ok("2") {
        rgb02_combine_transfer::execute().await?;
        return Ok(());
    }
    // 2-deep off-chain chain (RGB_E2E=3): un-broadcast split -> un-broadcast combine, validated via
    // validate_offchain_chain over both un-broadcast witnesses. Off-chain DAG depth.
    if std::env::var("RGB_E2E").as_deref() == std::result::Result::Ok("3") {
        rgb03_offchain_chain::execute().await?;
        return Ok(());
    }

    tb01_simple_transfer::execute().await?;
    tb02_transfer_address_reuse::execute().await?;
    tb03_simple_atomic_transfer::execute().await?;
    tb04_simple_lightning_latch::execute().await?;
    tb05_timelock::execute().await?;
    tm01_sender_double_spends::execute().await?;
    ta01_sign_second_not_called::execute().await?;
    ta02_duplicate_deposits::execute().await?;
    ta03_multiple_deposits::execute().await?;
    tv01::execute().await?;

    Ok(())
}
