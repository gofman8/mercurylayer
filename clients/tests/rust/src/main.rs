
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
pub mod rgb15_colored_tier_builder;
pub mod rgb_dump;
pub mod sdk01_wallet_flow;
pub mod sdk02_token_flow;
pub mod sdk04_adversarial;
pub mod sdk09_ifa_batch;
pub mod sdk11_parity_methods;
pub mod sdk12_adversarial;
pub mod sdk15_fresh_doublesign;
pub mod sdk16_onboarding;
pub mod sdk17_oor_chain;
pub mod sdk19_receive_failure;
pub mod sdk20_adversarial_gate;
pub mod sdk21_remote_sspclient;
pub mod chaos22_concurrent_users;
pub mod chaos22_cheats;
pub mod chaos22_oracle;
pub mod sdk23_rgb_ln_swap;
pub mod sdk24_receive_cancel;
pub mod sdk25_receive_delayed_claim;
pub mod sdk29_granularity_tokens;
pub mod sdk30_refresh;
pub mod sdk31_token_combine;
pub mod sdk32_token_over_time;
pub mod sdk34_token_watchtower;
pub mod sdk36_derived_tokens;
pub mod sdk37_ssp_value_gate;
pub mod sdk38_sponsor_stiff;
pub mod sdk39_depth2_token_exit;
pub mod sdk40_tesr_consensus;
pub mod sdk41_tesr_transfer;
pub mod sdk42_tesr_lifecycle;
pub mod sdk43_tesr_rollover;
pub mod sdk44_tesr_params;
pub mod sdk45_tesr_watchtower;
pub mod sdk46_tesr_rprime;
pub mod sdk47_tesr_rprime_transfer;
pub mod sdk48_v2_native_deposit;
pub mod sdk49_model_a_transfer;
pub mod sdk50_v2_unilateral_exit;
pub mod sdk51_v2_watchtower;
pub mod sdk52_v2_rgb_carrier;
pub mod sdk53_v2_latch_guard;
pub mod sdk54_verify_bundle_adversarial;
pub mod sdk55_backup_chain_adversarial;
pub mod sdk56_keystone_retry_idempotent;
pub mod sdk57_owner_share_binding;
pub mod sdk58_inladder_split;
pub mod sdk59_inladder_pay;
pub mod sdk60_child_firstclass;
pub mod sdk63_v2_lightning_pay;
pub mod sdk64_v2_lightning_receive;
pub mod sdk65_inladder_lightning_pay;
pub mod sdk66_inladder_pay_failure;
pub mod sdk67_inladder_lightning_receive;
pub mod sdk68_v2_pay_failure_reclaim;
pub mod sdk69_transfer_many_inladder;
pub mod sdk70_verifier_binding_adversarial;
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

    // mercury-utexo-sdk smoke (SDK_E2E=1): deposit -> exact-subset transfer -> auto-claim ->
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
    // IFA issuance + mint + batch token transfer (SDK_E2E=9): G3.
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("9") {
        sdk09_ifa_batch::execute().await?;
        return Ok(());
    }
    // Parity methods (SDK_E2E=11): identity signing, multi-recipient sats, Utexo invoices (Spark-compatible), queries.
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("11") {
        sdk11_parity_methods::execute().await?;
        return Ok(());
    }
    // Adversarial regressions (SDK_E2E=12): single_use sub-coins, honest branch accept, nonce reuse.
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("12") {
        sdk12_adversarial::execute().await?;
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
    // Tokens over time (SDK_E2E=32): issue/receive tokens, idle past every ladder horizon, then
    // test not-lost / cooperative send / unilateral materialization / the received-token clawback window.
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("32") {
        sdk32_token_over_time::execute().await?;
        return Ok(());
    }
    // Token-carrier watchtower (SDK_E2E=34): auto_exit_due now auto-materializes a received token
    // carrier before its clawback deadline (branch-only), defeating the sender's stale-backup
    // clawback; issued/flat carriers (no branch) are left untouched.
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("34") {
        sdk34_token_watchtower::execute().await?;
        return Ok(());
    }
    // Derived deposit tokens (SDK_E2E=36): split/combine/refresh slots are FREE derived slots
    // (SE-minted vouchers against the parent statechain — never the paid onboarding pool);
    // onboarding still charges; owner-auth (single-use nonce) + per-parent lifetime cap enforced.
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("36") {
        sdk36_derived_tokens::execute().await?;
        return Ok(());
    }
    // SSP value gate (SDK_E2E=37): peek_pending_transfers branch-validates sats [3] +
    // validate_pending_token derives the true consignment asset/amount [4] — over SE+RGB, no RLN.
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("37") {
        sdk37_ssp_value_gate::execute().await?;
        return Ok(());
    }
    // Sponsored-refresh bounded loss (SDK_E2E=38): a broke sponsor makes refresh_sponsored error
    // but the user keeps the refreshed amount-fee coin.
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("38") {
        sdk38_sponsor_stiff::execute().await?;
        return Ok(());
    }
    // Depth-2 token exit (SDK_E2E=39): a token piece two colored-splits deep materializes on-chain,
    // preserving the RGB allocation.
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("39") {
        sdk39_depth2_token_exit::execute().await?;
        return Ok(());
    }
    // TES-R (Utexo V2) consensus core (SDK_E2E=40): un-broadcast immunity, CSV enforcement, full
    // unilateral exit, and cooperative de-trigger — co-signed by the unchanged blind SE.
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("40") {
        sdk40_tesr_consensus::execute().await?;
        return Ok(());
    }
    // TES-R off-chain transfer safety (SDK_E2E=41): after Alice pays Bob, Bob's lower-CSV state wins
    // the exit race and Alice cannot claw back.
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("41") {
        sdk41_tesr_transfer::execute().await?;
        return Ok(());
    }
    // TES-R wallet lifecycle + persistence (SDK_E2E=42): establish → renew off-chain → persist →
    // reload from DB → unilateral exit from the reloaded bundle.
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("42") {
        sdk42_tesr_lifecycle::execute().await?;
        return Ok(());
    }
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("43") {
        sdk43_tesr_rollover::execute().await?;
        return Ok(());
    }
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("44") {
        sdk44_tesr_params::execute().await?;
        return Ok(());
    }
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("45") {
        sdk45_tesr_watchtower::execute().await?;
        return Ok(());
    }
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("46") {
        sdk46_tesr_rprime::execute().await?;
        return Ok(());
    }
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("47") {
        sdk47_tesr_rprime_transfer::execute().await?;
        return Ok(());
    }
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("48") {
        sdk48_v2_native_deposit::execute().await?;
        return Ok(());
    }
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("49") {
        sdk49_model_a_transfer::execute().await?;
        return Ok(());
    }
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("50") {
        sdk50_v2_unilateral_exit::execute().await?;
        return Ok(());
    }
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("51") {
        sdk51_v2_watchtower::execute().await?;
        return Ok(());
    }
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("52") {
        sdk52_v2_rgb_carrier::execute().await?;
        return Ok(());
    }
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("53") {
        sdk53_v2_latch_guard::execute().await?;
        return Ok(());
    }
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("54") {
        sdk54_verify_bundle_adversarial::execute().await?;
        return Ok(());
    }
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("55") {
        sdk55_backup_chain_adversarial::execute().await?;
        return Ok(());
    }
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("56") {
        sdk56_keystone_retry_idempotent::execute().await?;
        return Ok(());
    }
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("57") {
        sdk57_owner_share_binding::execute().await?;
        return Ok(());
    }
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("58") {
        sdk58_inladder_split::execute().await?;
        return Ok(());
    }
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("59") {
        sdk59_inladder_pay::execute().await?;
        return Ok(());
    }
    // First-class children (SDK_E2E=60): a RECEIVED in-ladder split child is re-transferred OFF-CHAIN
    // (alice -> bob -> carol) and only carol's exit touches the chain — Spark-parity multi-hop.
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("60") {
        sdk60_child_firstclass::execute().await?;
        return Ok(());
    }
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("63") {
        sdk63_v2_lightning_pay::execute().await?;
        return Ok(());
    }
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("64") {
        sdk64_v2_lightning_receive::execute().await?;
        return Ok(());
    }
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("65") {
        sdk65_inladder_lightning_pay::execute().await?;
        return Ok(());
    }
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("66") {
        sdk66_inladder_pay_failure::execute().await?;
        return Ok(());
    }
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("67") {
        sdk67_inladder_lightning_receive::execute().await?;
        return Ok(());
    }
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("68") {
        sdk68_v2_pay_failure_reclaim::execute().await?;
        return Ok(());
    }
    // transfer_many on a LADDERED parent (SDK_E2E=69): the multi-child in-ladder split, with the
    // [B1] retained-trigger attack executed for real — both recipients still exit for their amounts.
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("69") {
        sdk69_transfer_many_inladder::execute().await?;
        return Ok(());
    }
    // Verifier binding + payload_vout fail-closed (SDK_E2E=70): a fully co-signed DECOY ladder over an
    // attacker-owned outpoint is accepted by the unbound verifier and REJECTED once the authority is
    // derived from the coin [C-1]; a genuine tier cannot be counted twice [C-2]; every migrated
    // `payload_vout` site fails closed with a named error.
    if std::env::var("SDK_E2E").as_deref() == std::result::Result::Ok("70") {
        sdk70_verifier_binding_adversarial::execute().await?;
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
    // CTES-R coloured tier builder + per-tier seal blinding (RGB_E2E=15): promotes the E1/E2 gate
    // harnesses — coloured 1-payload and N-payload tiers at exactly 2.000 sat/vB, off-chain
    // validation against an un-broadcast txid, and the decisive >=3-rival / non-minimum-internal-txid
    // test with the retired global blinding as the negative control. Needs bitcoind + electrs +
    // the RGB proxy only (no Mercury server, no lockbox).
    if std::env::var("RGB_E2E").as_deref() == std::result::Result::Ok("15") {
        rgb15_colored_tier_builder::execute().await?;
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
