
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
pub mod rgb09_send_receive_blinded_witness;
pub mod rgb10_history_and_selftransfer;
pub mod rgb11_issue_schemas_uda_cfa;
pub mod rgb12_validate_offchain_negative;
pub mod rgb13_consignment_integrity;
pub mod rgb14_metadata_and_ifa_supply;
pub mod rgb_dump;
pub mod sdk01_wallet_flow;
pub mod sdk02_token_flow;
pub mod sdk03_lightning_swap;
pub mod sdk04_adversarial;
pub mod sdk05_lightning_pay;
pub mod sdk06_lightning_receive;
pub mod sdk07_unilateral_exit;
pub mod sdk08_terminal_node;
pub mod sdk09_ifa_batch;
pub mod sdk10_terminal_parent_verify;
pub mod sdk11_parity_methods;
pub mod sdk12_adversarial;
pub mod sdk13_stale_state;
pub mod sdk14_watcher_race;
pub mod sdk15_fresh_doublesign;
pub mod sdk16_onboarding;
pub mod sdk17_oor_chain;
pub mod sdk18_pay_failure_reclaim;
pub mod sdk19_receive_failure;
pub mod sdk20_adversarial_gate;
pub mod sdk21_remote_sspclient;
pub mod chaos22_concurrent_users;
pub mod chaos22_cheats;
pub mod chaos22_oracle;
pub mod sdk23_rgb_ln_swap;
pub mod sdk24_receive_cancel;
pub mod sdk25_receive_delayed_claim;
pub mod sdk26_invalidation_scale;
pub mod sdk27_invalidation_time;
pub mod sdk28_granularity_sats;
pub mod sdk29_granularity_tokens;
pub mod sdk30_refresh;
pub mod sdk31_token_combine;
pub mod rln;
pub mod utils;
use anyhow::{Result, Ok};

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> Result<()> {

    // Trace logging is opt-in via RUST_LOG (no-op when unset): surfaces reqwest/rgb-lib/SDK
    // log lines so the full-suite run can be reviewed for anomalies (delays, retries, errors).
    let _ = env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("off"),
    )
    .format_timestamp_millis()
    .try_init();

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
    // IFA issuance + mint + batch token transfer (SDK_E2E=9): G3.
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("9") {
        sdk09_ifa_batch::execute().await?;
        return Ok(());
    }
    // Receiver terminal-parent verification (SDK_E2E=10): G1 adversarial.
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("10") {
        sdk10_terminal_parent_verify::execute().await?;
        return Ok(());
    }
    // Parity methods (SDK_E2E=11): identity signing, multi-recipient sats, Spark invoices, queries.
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("11") {
        sdk11_parity_methods::execute().await?;
        return Ok(());
    }
    // Adversarial regressions (SDK_E2E=12): single_use sub-coins, honest branch accept, nonce reuse.
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("12") {
        sdk12_adversarial::execute().await?;
        return Ok(());
    }
    // Stale-state broadcast (SDK_E2E=13): old-state claw-back is rejected/defeated + watcher detects.
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("13") {
        sdk13_stale_state::execute().await?;
        return Ok(());
    }
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("14") {
        sdk14_watcher_race::execute().await?;
        return Ok(());
    }
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("15") {
        sdk15_fresh_doublesign::execute().await?;
        return Ok(());
    }
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("16") {
        sdk16_onboarding::execute().await?;
        return Ok(());
    }
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("17") {
        sdk17_oor_chain::execute().await?;
        return Ok(());
    }
    // Lightning PAY failure + reclaim (SDK_E2E=18): unroutable pay -> SSP claims nothing -> reclaim.
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("18") {
        sdk18_pay_failure_reclaim::execute().await?;
        return Ok(());
    }
    // Lightning RECEIVE failure (SDK_E2E=19): never paid -> no preimage, receiver can't claim.
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("19") {
        sdk19_receive_failure::execute().await?;
        return Ok(());
    }
    // Adversarial SSP gate (SDK_E2E=20): C2 wrong-recipient + C3 undersized -> SSP refuses to pay.
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("20") {
        sdk20_adversarial_gate::execute().await?;
        return Ok(());
    }
    // Remote SspClient over HTTP (SDK_E2E=21): pay + receive against a deployed mercury-ssp server.
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("21") {
        sdk21_remote_sspclient::execute().await?;
        return Ok(());
    }
    // Concurrent chaos/property test (SDK_E2E=22): N users act in parallel (enter/send/receive/
    // split/exit/withdraw) + cheat (broadcast old state); a spec-invariant oracle audits the trace.
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("22") {
        chaos22_concurrent_users::execute().await?;
        return Ok(());
    }
    // RGB assets over Lightning (SDK_E2E=23): issue -> colored channel -> asset invoice -> pay,
    // driven by the SDK's RlnClient asset methods (the LN half of a statechain<->LN RGB swap).
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("23") {
        sdk23_rgb_ln_swap::execute().await?;
        return Ok(());
    }
    // Lightning -> Mercury RECEIVE aborted after payment (SDK_E2E=24): payer pays -> HTLC HELD by the
    // fork's HODL invoice -> SSP cancels via /cancelhodlinvoice -> payer refunded, SSP keeps its coin.
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("24") {
        sdk24_receive_cancel::execute().await?;
        return Ok(());
    }
    // Adversarial delayed-claim (SDK_E2E=25): a receiver who delays past the coordinated SE latch
    // window gets nothing; the SSP keeps the coin and the payer is refunded (audit [2]/[5]).
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("25") {
        sdk25_receive_delayed_claim::execute().await?;
        return Ok(());
    }
    // Invalidation at SCALE (SDK_E2E=26): depth-4 off-chain split chain (each parent publicly
    // terminal, branch rows == depth, measured exit vsizes per depth as 'ECON depth=...' lines),
    // a 3-wide transfer_many fan-out from ONE split, and a full unilateral exit of the deepest
    // leaf (whole branch instantly broadcast, backup after its own fresh ~initlock ladder).
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("26") {
        sdk26_invalidation_scale::execute().await?;
        return Ok(());
    }
    // Invalidation over TIME (SDK_E2E=27): exact one-interval ladder decrement across 6 full-coin
    // hops, the sharp exit-maturity boundary (wait_blocks==2 at locktime-2, completes at the
    // locktime), the OPEN audit-[17] deadline gap on a k=2 pre-transferred parent (WARN line with
    // the exact gap), and the SE epoch gate on the plain-sats deposit path.
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("27") {
        sdk27_invalidation_time::execute().await?;
        return Ok(());
    }
    // Granularity, plain sats (SDK_E2E=28): exact-subset payments (no split), an exact off-chain
    // split (12_345 out of 100k, change 86_655, parent publicly terminal), the sub-dust boundary
    // refusal (typed error, coin untouched) + 330-sat minimum piece, and a depth-2 re-split of a
    // received piece (~155 vB per branch level, measured as ECON lines).
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("28") {
        sdk28_granularity_sats::execute().await?;
        return Ok(());
    }
    // Granularity, RGB tokens (SDK_E2E=29): raw-unit precision (0.10 / 0.01 booked exactly), a
    // depth-2 token exit (colored branch broadcast, opret anchors + allocation settled on-chain
    // on the exited outpoint), spent-carrier change becoming plain splittable BTC, and the
    // one-carrier-per-transfer limitation (typed error; 60+40 works where 100 fails).
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("29") {
        sdk29_granularity_tokens::execute().await?;
        return Ok(());
    }
    // Refresh / re-anchor (SDK_E2E=30): reset a coin's ladder + root deadline in one on-chain tx;
    // old outpoint spent (old backups dead), fresh coin at a fresh ladder, user pays the fee.
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("30") {
        sdk30_refresh::execute().await?;
        return Ok(());
    }
    // Multi-carrier token combine (SDK_E2E=31): pay an amount spanning several carriers via one
    // colored combine; receiver validates the multi-input branch + requires ALL carriers terminal.
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("31") {
        sdk31_token_combine::execute().await?;
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
    if std::env::var("RGB_E2E").as_deref() == std::result::Result::Ok("9") {
        rgb09_send_receive_blinded_witness::execute().await?;
        return Ok(());
    }
    if std::env::var("RGB_E2E").as_deref() == std::result::Result::Ok("10") {
        rgb10_history_and_selftransfer::execute().await?;
        return Ok(());
    }
    if std::env::var("RGB_E2E").as_deref() == std::result::Result::Ok("11") {
        rgb11_issue_schemas_uda_cfa::execute().await?;
        return Ok(());
    }
    if std::env::var("RGB_E2E").as_deref() == std::result::Result::Ok("12") {
        rgb12_validate_offchain_negative::execute().await?;
        return Ok(());
    }
    if std::env::var("RGB_E2E").as_deref() == std::result::Result::Ok("13") {
        rgb13_consignment_integrity::execute().await?;
        return Ok(());
    }
    if std::env::var("RGB_E2E").as_deref() == std::result::Result::Ok("14") {
        rgb14_metadata_and_ifa_supply::execute().await?;
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
