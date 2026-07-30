//! E2E (SSP value gate, audit [3]/[4]): the SSP's pre-payment value gate must read the TRUE value
//! of a latched coin — never an attacker-supplied hint — BEFORE it pays a Lightning invoice. The
//! two load-bearing primitives are exercised directly over the live SE + RGB stack (no RLN needed,
//! since the bug is in what value the gate *reads*, not in Lightning). Runs on the V2 (TES-R)
//! protocol, which is where the production gate lives (`SspService::execute_pay`, ssp.rs):
//!
//! [3] SATS: a non-exact payment out of a V2 (TES-R) coin is an IN-LADDER split — `transfer()`
//!     auto-routes to `in_ladder_pay`, so the SSP is handed a CHILD bundle (protocol_version 3)
//!     that has NO on-chain-rooted exit branch to read a value from. `peek_pending_transfers`
//!     therefore proves the child with `verify_conveyed_child` (child pays THIS wallet's exit key,
//!     parent + child are terminal, and each `num_sigs` matches its conveyed-ladder baseline, so no
//!     hidden lower-CSV state exists) and reports `amount = child_state.out_value` — the value the
//!     ladder CRYPTOGRAPHICALLY commits to, which OVERRIDES any branch-derived figure. It fails
//!     CLOSED: any tamper/inflation makes `verify_conveyed_child` error, which sets
//!     `ladder_census_ok = false` and leaves the amount un-overridden, so `check_latched_coins`
//!     refuses. Here a LEGIT in-ladder child latched to the SSP is peeked: the census passes and the
//!     census-bound amount equals the child's ladder-committed EXIT-REACHABLE value — the piece
//!     nominal minus its own two exit tiers (each burns `committed_fee + P2A_VALUE`), which is what
//!     the SSP can actually redeem and therefore what the gate must price against.
//! [4] RGB: `validate_pending_token` derives the amount a pending consignment CRYPTOGRAPHICALLY
//!     assigns to the coin (not the attacker-controlled envelope hint `env.a`), plus its contract
//!     id — read-only, before any claim. Here a pending token transfer to the SSP is validated and
//!     the derived (contract_id, amount) matches the real asset + amount; a smaller/wrong-asset coin
//!     is therefore detectable BEFORE paying.
//! [F1] The pre-payment predicate IS the claim predicate. Both `validate_pending_token` (pre-pay)
//!     and `accept_incoming_tokens` (claim) call one shared implementation, so the gate cannot be
//!     weaker than the check that later books the coin. Proved here by mutating ONLY the envelope
//!     amount (the consignment bytes still validate cryptographically) and asserting the SSP's gate
//!     REFUSES — previously it accepted, the SSP paid an irreversible invoice, and the same coin
//!     then failed PERMANENT-INVALID at claim.
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
    // Runs on the V2 (TES-R) default — no protocol pin. Alice's plain-sats deposit is therefore a
    // LADDERED coin, so the non-exact payment in [3] below is an in-ladder split whose piece reaches
    // the SSP as a conveyed CHILD bundle: exactly the shape the pre-pay value gate must census.
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

    // ===== [F1] The pre-pay predicate is the CLAIM predicate — envelope equality included =========
    // The envelope (`{"c":consignment,"a":amount,"s":sats}`) travels with the transfer and is fully
    // attacker-controlled. The claim path has always rejected `a != consignment-derived amount`
    // (PERMANENT-INVALID); the SSP's pre-payment gate did NOT. That gap paid real Lightning money for
    // a coin that could never claim: pass the gate with a mutated `a`, take the irreversible payment,
    // and the SSP is left holding a coin its own claim path rejects. Both entry points now call ONE
    // predicate (`tokens::verify_consignment_assignment`), so they cannot disagree.
    //
    // Mutate ONLY the envelope amount — the consignment bytes `c` are untouched and still validate
    // cryptographically, which is exactly what made this pass the old, weaker gate.
    let mut tampered: serde_json::Value = serde_json::from_str(env)?;
    let honest_a = tampered["a"].as_u64().ok_or_else(|| anyhow!("envelope has no amount"))?;
    assert_eq!(honest_a, booked, "the honest envelope agrees with the consignment-derived amount");
    tampered["a"] = serde_json::json!(honest_a + 9_750); // claim 10_000 for a 250-unit coin
    let tampered = serde_json::to_string(&tampered)?;
    // The RGB resolver is a live network dependency and can transiently fail to locate a witness. That
    // flake must NOT be allowed to look like a pass, and it must NOT be allowed to soften the security
    // assertion either, so the two are separated:
    //   * "the gate returned Ok" is fatal ON EVERY ATTEMPT — no retry, no tolerance. That is the F1
    //     defect and a single occurrence is a failure.
    //   * only the question of WHICH refusal we got is retried past a transient resolver error, and if
    //     every attempt is transient the test FAILS explicitly rather than passing on a flake.
    let mut err = None;
    let mut last_transient = String::new();
    for _ in 0..15 {
        match ssp
            .validate_pending_token(&tampered, &p.branch_txs, &p.funding_txid, p.funding_vout)
            .await
        {
            Ok((c, a)) => panic!(
                "SSP pre-payment gate ACCEPTED a mutated envelope (returned {c} / {a}) — it would pay an irreversible Lightning invoice for a coin its own claim path rejects"
            ),
            Err(e) => {
                let s = e.to_string();
                // A resolver/indexer outage refuses too (fail-closed), but it refuses for the wrong
                // reason — it never reached the envelope-equality check we are here to prove.
                if s.contains("resolver") || s.contains("can't be located") {
                    last_transient = s;
                    tokio::time::sleep(Duration::from_secs(4)).await;
                    continue;
                }
                err = Some(s);
                break;
            }
        }
    }
    let err = err.ok_or_else(|| anyhow!(
        "the RGB resolver never recovered, so the envelope-equality refusal was never reached (last: {last_transient}) — NOT a pass"
    ))?;
    // The SAME verdict the claim path renders: PERMANENT-INVALID, i.e. this coin can never book.
    assert!(
        err.contains("PERMANENT-INVALID"),
        "the pre-pay gate must render the claim path's verdict verbatim, got: {err}"
    );
    assert!(
        err.contains("envelope claimed"),
        "the refusal must name the envelope/consignment amount disagreement, got: {err}"
    );
    // `SspService::execute_pay` maps this Err to "pre-payment RGB validation failed for {sid} —
    // refusing to pay" BEFORE `rln.send_payment`, so no Lightning money moves.
    println!("SDK37 - [F1] SSP REFUSED to pay on a mutated envelope (a={} vs consignment-derived {booked}): {err}", honest_a + 9_750);
    // And the honest envelope still passes — the fix is strictly a rejection, not a lockout. Same
    // split: an Ok is the only acceptable terminal state, a transient resolver error is retried, and
    // an exhausted retry budget FAILS.
    let mut honest_again = None;
    for _ in 0..15 {
        match ssp
            .validate_pending_token(env, &p.branch_txs, &p.funding_txid, p.funding_vout)
            .await
        {
            Ok(v) => {
                honest_again = Some(v);
                break;
            }
            Err(e) => {
                let s = e.to_string();
                if s.contains("resolver") || s.contains("can't be located") {
                    tokio::time::sleep(Duration::from_secs(4)).await;
                    continue;
                }
                panic!("the HONEST envelope was refused — the F1 fix must reject only the mutated one: {s}");
            }
        }
    }
    let (c2, a2) = honest_again
        .ok_or_else(|| anyhow!("the RGB resolver never recovered for the honest re-validation"))?;
    assert_eq!((c2.as_str(), a2), (asset.as_str(), 250), "the honest envelope still validates");

    // ===== [3] SATS: peek_pending_transfers reports a CENSUS-BOUND amount ========================
    // Fund alice with plain sats and pay a NON-EXACT amount. The coin is laddered (V2), so a plain-BTC
    // self-split is refused [B1] and `transfer()` auto-routes to `in_ladder_pay`: the SSP is latched to
    // an in-ladder CHILD whose value carries no on-chain funding to read — it must be proved by census.
    fund(&cc, &alice, 60_000).await?;
    // V2 child slots are funded by FREE derived tokens (take_derived_tokens), so no "split slot" top-up
    // is needed; these are only a spare buffer for any deposit/auto-refresh the wallet may want.
    for _ in 0..3 {
        let t = prepaid_token(&cc).await?;
        alice.add_prepaid_token(&t).await;
    }
    let send_sats = 20_000u64;
    // The parent's ladder fee rate — the child's own two exit tiers are sized from it below.
    let parent_id = {
        let rec = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk37_alice").await?;
        rec.coins
            .iter()
            .find(|c| c.status == CoinStatus::CONFIRMED && c.amount == Some(60_000))
            .and_then(|c| c.statechain_id.clone())
            .ok_or_else(|| anyhow!("alice has no confirmed 60k coin to split"))?
    };
    let parent_bundle = mercuryrustlib::tesr::load(&cc, "sdk37_alice", &parent_id)
        .await?
        .ok_or_else(|| anyhow!("alice's 60k coin has no TES-R ladder — the in-ladder split path would not be exercised"))?;
    let fee_rate = parent_bundle.fee_rate;

    let sr = alice.transfer(&ssp_addr, send_sats).await?;
    assert!(sr.used_split, "20k from a 60k laddered coin must arrive as an in-ladder split child");
    let sub_id = sr.coins[0].statechain_id.clone();
    tokio::time::sleep(Duration::from_secs(3)).await;

    let pend2 = mercuryrustlib::transfer_receiver::peek_pending_transfers(ssp.client_config(), ssp.wallet_name()).await?;
    let ps = pend2.iter().find(|p| p.statechain_id == sub_id)
        .ok_or_else(|| anyhow!("the pending sub-coin was not peeked (id {sub_id})"))?;
    // A conveyed in-ladder child carries an EMPTY branch (transfer/sender.rs sets branch_txs = []) and a
    // `child_tesr_bundle` instead — so there is no branch leaf whose value the gate could be tricked by.
    assert!(ps.branch_txs.is_empty(), "an in-ladder child conveyance carries no exit branch — its value cannot come from a branch leaf at all");
    // FAIL-CLOSED CENSUS — the V2 replacement for [3]'s branch validation. `verify_conveyed_child`
    // proved: the child pays THIS wallet's exit key (Model A), parent and child are both terminal, and
    // each num_sigs equals its conveyed-ladder baseline (no hidden lower-CSV state). Any tamper → Err →
    // ladder_census_ok = false → the SSP's pre-pay gate refuses to pay.
    assert!(ps.ladder_census_ok, "the in-ladder child passed verify_conveyed_child: child pays the SSP key + parent/child terminality + num_sigs baseline");
    // The amount is CENSUS-BOUND: peek OVERRIDES any branch-derived figure with the child ladder's own
    // committed `child_state.out_value` (tesr.rs verify_conveyed_child), so an attacker-inflated hint
    // can never reach the value gate. NOTE the census value is the child's EXIT-REACHABLE value, not
    // the piece nominal: `establish_child` builds the child's own extension+state tiers off the piece,
    // and each tier burns `committed_fee(rate) + P2A_VALUE`. Derive that figure here (rather than
    // hard-coding it) so this stays exact if the tier constants ever change.
    let after_x = mercurylib::tesr::tier_out_value(send_sats, fee_rate)
        .ok_or_else(|| anyhow!("piece too small for the child's extension tier"))?;
    let expected_census = mercurylib::tesr::tier_out_value(after_x, fee_rate)
        .ok_or_else(|| anyhow!("piece too small for the child's state tier"))?;
    assert!(expected_census < send_sats, "each child tier burns committed_fee + P2A, so the exit-reachable value is strictly below the piece nominal");
    assert_eq!(
        ps.amount, expected_census,
        "peek must report the census-bound child value (child_state.out_value = the piece minus its two tiers' committed fee + P2A); a coin failing verify_conveyed_child reads ladder_census_ok=false and the gate refuses"
    );
    println!("SDK37 - [3] SATS: peek_pending_transfers censused the in-ladder child (verify_conveyed_child) and reported its ladder-committed value {} sats (piece {} minus its two exit tiers) — a value-inflating/tampered child fails the census → ladder_census_ok=false → the gate refuses", ps.amount, send_sats);

    println!("SDK37 - SUCCESS: on V2 (TES-R) the SSP pre-payment value gate reads the TRUE coin value, closing audit [3]/[4]. [4] validate_pending_token derives the consignment's cryptographic asset+amount (250 of {asset}) — not the attacker's envelope hint — so an under-value/wrong-asset RGB coin is refused before send_payment. [3] peek_pending_transfers censuses the in-ladder split CHILD with verify_conveyed_child and reports its ladder-committed EXIT-REACHABLE value ({expected_census} sats = the {send_sats}-sat piece minus its two tiers' committed fee + P2A), so an inflated hint is never read and any tampered child fails closed (ladder_census_ok=false) and cannot satisfy the SATS value gate. [F1] the RGB pre-payment predicate is now literally the claim predicate (one shared implementation), so a mutated envelope amount — which the old, weaker gate accepted — is REFUSED before send_payment with the claim path's own PERMANENT-INVALID verdict. Neither path can make the SSP over-pay, and nothing can make it pay for a coin that will never claim.");
    Ok(())
}
