
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
pub mod rgb04_single_use;
pub mod rgb05_combine3;
pub mod rgb06_dag3;
pub mod rgb07_epoch;
pub mod rgb08_wide_combine;
pub mod rgb_dump;
pub mod sdk01_wallet_flow;
pub mod sdk02_token_flow;
pub mod sdk03_lightning_swap;
pub mod sdk04_adversarial;
pub mod sdk05_lightning_pay;
pub mod sdk06_lightning_receive;
pub mod sdk07_unilateral_exit;
pub mod sdk08_terminal_node;
pub mod rln;
pub mod utils;
use anyhow::{Result, Ok};

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> Result<()> {

    // mercury-spark-sdk smoke (SDK_E2E=1): deposit -> exact-subset transfer -> auto-claim ->
    // off-chain-split transfer (exact amount w/ change) -> auto-claim -> cooperative exit.
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("1") {
        sdk01_wallet_flow::execute().await?;
        return Ok(());
    }
    // SDK token flow (SDK_E2E=2): issue RGB asset onto a statechain coin -> off-chain token
    // transfer (colored split + consignment in the transfer msg) -> verified booking -> exit.
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("2") {
        sdk02_token_flow::execute().await?;
        return Ok(());
    }
    // SDK lightning-swap legs (SDK_E2E=3): latch transfer locked on an SE preimage; claim gated on
    // settlement; preimage matches the payment hash (Spark SSP preimage-swap parity).
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("3") {
        sdk03_lightning_swap::execute().await?;
        return Ok(());
    }
    // Mercury -> Lightning via SSP (SDK_E2E=5): pay a real BOLT11 from statechain balance.
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("5") {
        sdk05_lightning_pay::execute().await?;
        return Ok(());
    }
    // Lightning -> Mercury via SSP (SDK_E2E=6): receive a real LN payment as a statechain coin.
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("6") {
        sdk06_lightning_receive::execute().await?;
        return Ok(());
    }
    // Practical unilateral exit (SDK_E2E=7): estimate cost+wait, branch out instantly, mine past
    // the backup locktime, exit completes with zero SE involvement.
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("7") {
        sdk07_unilateral_exit::execute().await?;
        return Ok(());
    }
    // Terminal-node enforcement (SDK_E2E=8): the SE refuses any co-signature on a split parent.
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("8") {
        sdk08_terminal_node::execute().await?;
        return Ok(());
    }
    // RLN harness smoke (LN_SMOKE=1): two rgb-lightning-node daemons, funded channel, real BOLT11.
    if std::env::var("LN_SMOKE").as_deref() == std::result::Result::Ok("1") {
        let (a, b) = rln::setup_ln_pair("/tmp/rln-smoke").await?;
        let invoice = b.ln_invoice(100_000, None, 300).await?;
        let hash = a.send_payment(&invoice).await?;
        for _ in 0..30 {
            if b.invoice_status(&invoice).await? == "Succeeded" { break; }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
        assert_eq!(b.invoice_status(&invoice).await?, "Succeeded");
        println!("LN SMOKE - SUCCESS: A paid B's 100k-msat invoice over a real channel (hash {hash})");
        return Ok(());
    }
    // SDK adversarial guard rails (SDK_E2E=4): typed refusals, split-parent double-spend refusal,
    // idempotent claims, double-withdraw refusal.
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("4") {
        sdk04_adversarial::execute().await?;
        return Ok(());
    }
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
    // SE single-use probe (RGB_E2E=4): the SE must refuse a 2nd conflicting spend of a node.
    if std::env::var("RGB_E2E").as_deref() == std::result::Result::Ok("4") {
        rgb04_single_use::execute().await?;
        return Ok(());
    }
    // 3-input combine (RGB_E2E=5): many deposit coins -> one payment + change.
    if std::env::var("RGB_E2E").as_deref() == std::result::Result::Ok("5") {
        rgb05_combine3::execute().await?;
        return Ok(());
    }
    // 3-level off-chain DAG (RGB_E2E=6): split -> combine -> split, all un-broadcast, 3-witness chain.
    if std::env::var("RGB_E2E").as_deref() == std::result::Result::Ok("6") {
        rgb06_dag3::execute().await?;
        return Ok(());
    }
    // Stage 4 epoch deadline (RGB_E2E=7): SE co-signs in the active period, REFUSES a new co-signature
    // past the deadline, and unilateral exit (broadcasting a pre-co-signed branch) needs no SE call.
    if std::env::var("RGB_E2E").as_deref() == std::result::Result::Ok("7") {
        rgb07_epoch::execute().await?;
        return Ok(());
    }
    // Wide-combine scale test (RGB_E2E=8): a user manufactures N sub-coins (by splitting one coin) and
    // combines all N in one SE-co-signed tx -> a single payment + change. The combine primitive scales.
    if std::env::var("RGB_E2E").as_deref() == std::result::Result::Ok("8") {
        rgb08_wide_combine::execute().await?;
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
