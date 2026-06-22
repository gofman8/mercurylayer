
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
pub mod rgb01_full_lifecycle;
pub mod rgb02_deposit_coop_exit;
pub mod rgb03_exit_to_onchain;
pub mod rgb04_register_statechain_utxo;
pub mod rgb05_blinded_statechain_transfer;
pub mod utils;
use anyhow::{Result, Ok};

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> Result<()> {

    // Run only the RGB-over-statechain lifecycle test when RGB_E2E=1 (it needs the RGB proxy +
    // indexer in addition to the Mercury stack). See docs/rgb_integration.md.
    if std::env::var("RGB_E2E").as_deref() == std::result::Result::Ok("1") {
        rgb01_full_lifecycle::execute().await?;
        return Ok(());
    }
    if std::env::var("RGB_E2E").as_deref() == std::result::Result::Ok("2") {
        rgb02_deposit_coop_exit::execute().await?;
        return Ok(());
    }
    if std::env::var("RGB_E2E").as_deref() == std::result::Result::Ok("3") {
        rgb03_exit_to_onchain::execute().await?;
        return Ok(());
    }
    if std::env::var("RGB_E2E").as_deref() == std::result::Result::Ok("4") {
        rgb04_register_statechain_utxo::execute().await?;
        return Ok(());
    }
    if std::env::var("RGB_E2E").as_deref() == std::result::Result::Ok("5") {
        rgb05_blinded_statechain_transfer::execute().await?;
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
