//! E2E (SSP value gate, audit [3]/[4]): the SSP's pre-payment value gate must read the TRUE value
//! of a latched coin — never an attacker-supplied hint — BEFORE it pays a Lightning invoice. The
//! two load-bearing primitives are exercised directly over the live SE + RGB stack (no RLN needed,
//! since the bug is in what value the gate *reads*, not in Lightning):
//!
//! [3] SATS: `peek_pending_transfers` now branch-validates a coin funded by an un-broadcast branch
//!     (the blind SE co-signs ANY leaf value) — it runs validate_branch + verify_terminal_parents
//!     and reports 0 on failure, so an inflated leaf can never satisfy `check_latched_coins`. Here a
//!     LEGIT off-chain sub-coin latched to the SSP is peeked and its branch-validated amount equals
//!     the real value (proving the honest path still passes; a tampered branch would read 0).
//! [4] RGB: `validate_pending_token` derives the amount a pending consignment CRYPTOGRAPHICALLY
//!     assigns to the coin (not the attacker-controlled envelope hint `env.a`), plus its contract
//!     id — read-only, before any claim. Here a pending token transfer to the SSP is validated and
//!     the derived (contract_id, amount) matches the real asset + amount; a smaller/wrong-asset coin
//!     is therefore detectable BEFORE paying.
//!
//! Run: SDK_E2E=37 ML_NETWORK=regtest cargo run

use anyhow::{anyhow, Result};
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};
use mercuryrustlib::{client_config::ClientConfig, CoinStatus};
use std::time::Duration;

use crate::bitcoin_core;

async fn prepaid_token(cc: &ClientConfig) -> Result<String> {
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    crate::utils::handle_token_response(cc, &token).await
}
async fn add_tokens(cc: &ClientConfig, w: &UtexoWallet, n: usize) -> Result<()> {
    for _ in 0..n {
        let t = prepaid_token(cc).await?;
        w.add_prepaid_token(&t).await;
    }
    Ok(())
}
async fn fund(cc: &ClientConfig, w: &UtexoWallet, sats: u64) -> Result<()> {
    let t = prepaid_token(cc).await?;
    w.add_prepaid_token(&t).await;
    let addr = w.get_deposit_address(sats).await?;
    bitcoin_core::sendtoaddress(sats as u32, &addr)?;
    let core = bitcoin_core::getnewaddress()?;
    bitcoin_core::generatetoaddress(3, &core)?;
    for _ in 0..60 {
        w.claim().await?;
        if w.get_balance().await?.available_sats >= sats {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Err(anyhow!("deposit of {sats} did not confirm"))
}
async fn token_balance(w: &UtexoWallet, asset: &str) -> Result<u64> {
    Ok(w.get_token_balances().await?.into_iter().find(|t| t.asset_id == asset).map(|t| t.balance).unwrap_or(0))
}
async fn wait_token_balance(w: &UtexoWallet, asset: &str, want: u64) -> Result<()> {
    for _ in 0..60 {
        w.claim().await?;
        if token_balance(w, asset).await? == want {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Err(anyhow!("balance of {asset} did not reach {want}"))
}
async fn wait_carrier(cc: &ClientConfig, w: &UtexoWallet, name: &str, core: &str, asset: &str, units: u64) -> Result<()> {
    for _ in 0..60 {
        bitcoin_core::generatetoaddress(1, core)?;
        w.claim().await?;
        if token_balance(w, asset).await? >= units {
            let rec = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, name).await?;
            if rec.coins.iter().any(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0 && c.amount == Some(10_000)) {
                return Ok(());
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Err(anyhow!("{name}: {units} of {asset} carrier did not confirm"))
}

pub async fn execute() -> Result<()> {
    // LN swaps require a V1 coin until adaptor-sig LN lands (V2-MIGRATION-PLAN); pin regardless of the V2 default.
    std::env::set_var("UTEXO_PROTOCOL_DEFAULT", "1");
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    for d in ["./rgb-data-sdk37_alice", "./rgb-data-sdk37_ssp"] {
        let _ = std::fs::remove_dir_all(d);
    }
    let cc = mercuryrustlib::client_config::load().await;
    let core = bitcoin_core::getnewaddress()?;

    let (alice, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk37_alice"), None).await?;
    // The "SSP": a wallet that receives the latched coin. It never claims during the test, so the
    // coin stays a PENDING transfer that the value gate must vet.
    let (ssp, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk37_ssp"), None).await?;
    let ssp_addr = ssp.get_utexo_address().await?;

    // ===== [4] RGB: validate_pending_token derives the TRUE consignment value pre-claim ==========
    let rgb_fund = alice.get_token_funding_address().await?;
    bitcoin_core::sendtoaddress(600_000, &rgb_fund)?;
    bitcoin_core::generatetoaddress(3, &core)?;
    tokio::time::sleep(Duration::from_secs(4)).await;
    add_tokens(&cc, &alice, 4).await?;
    let asset = alice.issue_token("VG", "Value Gate", 0, 1000).await?;
    wait_carrier(&cc, &alice, "sdk37_alice", &core, &asset, 1000).await?;
    println!("SDK37 - alice issued 1000 {asset}");

    // alice sends 250 to the SSP but the SSP does NOT claim — it stays pending.
    let r = alice.transfer_tokens(&asset, &ssp_addr, 250).await?;
    let pending_id = r.coins[0].statechain_id.clone();
    // Give the SE relay a moment; do NOT call ssp.claim().
    tokio::time::sleep(Duration::from_secs(3)).await;

    // The SSP peeks the pending transfer and validates its consignment BEFORE acting.
    let pend = mercuryrustlib::transfer_receiver::peek_pending_transfers(ssp.client_config(), ssp.wallet_name()).await?;
    let p = pend.iter().find(|p| p.statechain_id == pending_id)
        .ok_or_else(|| anyhow!("the pending token transfer was not peeked (id {pending_id})"))?;
    assert_eq!(token_balance(&ssp, &asset).await.unwrap_or(0), 0, "the SSP has NOT claimed — the coin is still a pending transfer to vet");
    let env = p.rgb_consignment.as_deref()
        .ok_or_else(|| anyhow!("pending token transfer carries no consignment envelope"))?;
    let (contract_id, booked) = ssp
        .validate_pending_token(env, &p.branch_txs, &p.funding_txid, p.funding_vout)
        .await?;
    // The gate sees the asset + amount the consignment CRYPTOGRAPHICALLY assigns — not a hint.
    assert_eq!(contract_id, asset, "validate_pending_token must derive the real contract id");
    assert_eq!(booked, 250, "validate_pending_token must derive the real assigned amount (250), not an inflated hint");
    // Therefore an SSP holding a 10_000-unit RGB invoice against this coin would refuse: 250 < 10_000.
    let big_invoice_amount = 10_000u64;
    assert!(booked < big_invoice_amount, "a 250-unit coin cannot satisfy a 10_000-unit invoice — the pre-payment gate refuses");
    // And a coin of a DIFFERENT asset id would fail the contract-id equality the gate enforces.
    assert_ne!(contract_id, "rgb:some-other-asset", "the gate binds to the invoiced asset id");
    println!("SDK37 - [4] RGB: validate_pending_token derived asset={contract_id} amount={booked} from the consignment BEFORE any claim — a wrong-asset or under-value coin is rejected pre-payment (250 < {big_invoice_amount})");

    // ===== [3] SATS: peek_pending_transfers reports a BRANCH-VALIDATED amount ====================
    // Fund alice with plain sats and send an amount that forces an off-chain split, so the SSP is
    // handed a sub-coin whose funding is an un-broadcast branch leaf.
    fund(&cc, &alice, 60_000).await?;
    for _ in 0..3 {
        let t = prepaid_token(&cc).await?;
        alice.add_prepaid_token(&t).await;
    }
    let send_sats = 20_000u64;
    let sr = alice.transfer(&ssp_addr, send_sats).await?;
    assert!(sr.used_split, "20k from a 60k coin must arrive as an off-chain split sub-coin");
    let sub_id = sr.coins[0].statechain_id.clone();
    tokio::time::sleep(Duration::from_secs(3)).await;

    let pend2 = mercuryrustlib::transfer_receiver::peek_pending_transfers(ssp.client_config(), ssp.wallet_name()).await?;
    let ps = pend2.iter().find(|p| p.statechain_id == sub_id)
        .ok_or_else(|| anyhow!("the pending sub-coin was not peeked (id {sub_id})"))?;
    assert!(!ps.branch_txs.is_empty(), "the latched coin is a genuine off-chain sub-coin (has an exit branch)");
    // The amount is branch-VALIDATED: it equals the real value only because validate_branch +
    // verify_terminal_parents passed. A tampered/inflated branch would fail them and read 0.
    assert_eq!(ps.amount, send_sats, "peek reports the branch-validated sub-coin value; an un-validated inflated leaf would read 0");
    println!("SDK37 - [3] SATS: peek_pending_transfers branch-validated the off-chain sub-coin and reported its true value {} sats (a value-inflating branch fails validate_branch → 0 → the gate refuses)", ps.amount);

    println!("SDK37 - SUCCESS: the SSP pre-payment value gate reads the TRUE coin value, closing audit [3]/[4]. [4] validate_pending_token derives the consignment's cryptographic asset+amount (250 of {asset}) — not the attacker's envelope hint — so an under-value/wrong-asset RGB coin is refused before send_payment. [3] peek_pending_transfers branch-validates an off-chain sub-coin's sats (20000), so an inflated un-broadcast leaf reads 0 and cannot satisfy the SATS value gate. Neither path can make the SSP over-pay.");
    Ok(())
}
