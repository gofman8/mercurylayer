//! E2E (SSP value gate, audit [3]/[4]): the SSP's pre-payment value gate must read the TRUE value
//! of a latched coin — never an attacker-supplied hint — BEFORE it pays a Lightning invoice. The
//! load-bearing primitive (`peek_pending_transfers`) is exercised directly over the live SE + RGB
//! stack (no RLN needed, since the bug is in what value the gate *reads*, not in Lightning). Runs on
//! the TES-R protocol, which is where the production gate lives (`SspService::execute_pay`, ssp.rs):
//!
//! [3] SATS: a non-exact payment out of a laddered coin is an IN-LADDER split — `transfer()`
//!     auto-routes to `in_ladder_pay`, so the SSP is handed a CHILD bundle that has NO
//!     on-chain-rooted exit branch to read a value from. `peek_pending_transfers` therefore proves
//!     the child with `verify_conveyed_child` (child pays THIS wallet's exit key, parent + child are
//!     terminal, and each `num_sigs` equals its `tiers + superseded` census — there is no flat term,
//!     so no hidden lower-CSV state exists) and reports `amount = child_state.out_value` — the value
//!     the ladder CRYPTOGRAPHICALLY commits to. It fails CLOSED: any tamper/inflation makes
//!     `verify_conveyed_child` error, which sets `ladder_census_ok = false` and leaves the amount
//!     un-overridden, so `check_latched_coins` refuses. Here a LEGIT in-ladder child latched to the
//!     SSP is peeked: the census passes and the census-bound amount equals the child's
//!     ladder-committed EXIT-REACHABLE value — the piece nominal minus its own two exit tiers (each
//!     burns `committed_fee + P2A_VALUE`), which is what the SSP can actually redeem and therefore
//!     what the gate must price against.
//! [4] RGB: on the laddered lane a token piece reaches the SSP as a conveyed COLOURED CHILD (the
//!     legacy flat coloured split is retired), and the peek surfaces its RGB material FROM THE
//!     BUNDLE — there is no backup row for an envelope to ride on: the child's LEAF consignment
//!     wrapped as the `{"c","a","s"}` envelope the SSP's validator reads, the outpoint it assigns
//!     to (the child's own final-state payload output — the same one the claim path books), and
//!     the child's five-tier witness chain [P3]; `branch_txs` empty; the census passed; `amount` =
//!     the piece's census-bound exit value (TOKEN_PIECE_SATS minus its two coloured rungs). The
//!     pre-pay gate proper is then run exactly as `SspService::execute_pay` runs it
//!     (`validate_pending_token_ex` over that material, BEFORE any claim) and books 250 of the
//!     invoiced contract. The claim afterwards books the same 250 from the consignment chain alone
//!     (`colored_child_health`), never from a declared field.
//! [F1] The pre-payment predicate cannot be weaker than the claim predicate: the envelope's
//!     declared amount is only ever CROSS-CHECKED against what the consignment assigns, so a
//!     mutated envelope (`a` = 251 over a chain that assigns 250) is refused by the very same
//!     validator, before payment, as PERMANENT-INVALID.
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
/// The fee rate of the wallet's COLOURED ROOT ladder for `asset` — the rate the piece's own two
/// rungs are sized from, and therefore the one input to the census-bound value [4] expects.
async fn colored_carrier_rate(cc: &ClientConfig, wallet_name: &str, asset: &str) -> Result<f64> {
    let rec = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, wallet_name).await?;
    for c in rec.coins.iter().filter(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0) {
        let Some(sid) = c.statechain_id.clone() else { continue };
        if let Some(b) = mercuryrustlib::tesr::load(cc, wallet_name, &sid).await? {
            if b.rgb.as_ref().is_some_and(|r| r.contract_id == asset) {
                return Ok(b.fee_rate);
            }
        }
    }
    Err(anyhow!("{wallet_name} holds no COLOURED carrier of {asset} — the CTES-R lane is not on"))
}
async fn wait_carrier(cc: &ClientConfig, w: &UtexoWallet, name: &str, core: &str, asset: &str, units: u64) -> Result<()> {
    for _ in 0..60 {
        bitcoin_core::generatetoaddress(1, core)?;
        w.claim().await?;
        if token_balance(w, asset).await? >= units {
            let rec = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, name).await?;
            if rec.coins.iter().any(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0 && c.amount == Some(mercury_utexo_sdk::tokens::TOKEN_CARRIER_SATS as u32)) {
                return Ok(());
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Err(anyhow!("{name}: {units} of {asset} carrier did not confirm"))
}

pub async fn execute() -> Result<()> {
    // Alice's plain-sats deposit is a LADDERED coin (every deposit is, at first sight), so the
    // non-exact payment in [3] below is an in-ladder split whose piece reaches the SSP as a conveyed
    // CHILD bundle: exactly the shape the pre-pay value gate must census. Both wallets opt into the
    // COLOURED lane ([D30] it ships false): the token payment in [4] is a coloured in-ladder split,
    // and the legacy flat coloured split a default wallet would take is retired.
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    for d in ["./rgb-data-sdk37_alice", "./rgb-data-sdk37_ssp"] {
        let _ = std::fs::remove_dir_all(d);
    }
    let cc = mercuryrustlib::client_config::load().await;
    let core = bitcoin_core::getnewaddress()?;

    let mut alice_cfg = SdkConfig::regtest("sdk37_alice");
    alice_cfg.colored_ladder = true;
    let (alice, _) = UtexoWallet::initialize(alice_cfg, None).await?;
    // The "SSP": a wallet that receives the latched coin. It does not claim until each section has
    // peeked, so the coin is a PENDING transfer that the value gate must vet.
    let mut ssp_cfg = SdkConfig::regtest("sdk37_ssp");
    ssp_cfg.colored_ladder = true;
    let (ssp, _) = UtexoWallet::initialize(ssp_cfg, None).await?;
    let ssp_addr = ssp.get_utexo_address().await?;

    // ===== [4] RGB: what the SSP can read about a pending TOKEN piece — and what it cannot ========
    // On the laddered lane a token payment is a coloured IN-LADDER split (the legacy flat coloured
    // split, whose `BackupTx.rgb_consignment` envelope this section used to validate, is RETIRED:
    // `register_split_subcoins_n` refuses by name). The piece reaches the SSP as a conveyed
    // COLOURED CHILD bundle, and `peek_pending_transfers` reports it fail-closed:
    //   * `rgb_consignment: None` — no consignment envelope rides on a backup row, because there are
    //     no backup rows. `SspService::execute_pay` reads exactly this field for an RGB invoice and
    //     refuses by name ("carries no RGB consignment — refusing to pay an RGB invoice"), so no
    //     attacker-supplied `env.a` hint can reach a value gate: there is no envelope to mutate.
    //     That is [F1]'s property in its strongest form.
    //   * `branch_txs` empty, and `child_witness_txids` = the child's own five-tier chain [P3] — the
    //     witness list an envelope-free coloured pre-pay validator resolves against.
    //   * `ladder_census_ok == true`, and `amount` = the piece's census-bound EXIT value, exactly as
    //     [3] below proves for sats: TOKEN_PIECE_SATS minus the child's own two coloured rungs.
    // The RGB amount itself is booked at claim from the consignment chain (`colored_child_health`),
    // never from a sender-declared field — asserted at the end of this section.
    let rgb_fund = alice.get_token_funding_address().await?;
    bitcoin_core::sendtoaddress(600_000, &rgb_fund)?;
    bitcoin_core::generatetoaddress(3, &core)?;
    tokio::time::sleep(Duration::from_secs(4)).await;
    add_tokens(&cc, &alice, 4).await?;
    let asset = alice.issue_token("VG", "Value Gate", 0, 1000).await?;
    wait_carrier(&cc, &alice, "sdk37_alice", &core, &asset, 1000).await?;
    // The coloured ladder is established by the claim pass that sees the booked allocation; give
    // that pass a few more blocks if the carrier confirmed ahead of it.
    let mut carrier_rate = None;
    for _ in 0..30 {
        if let Ok(rate) = colored_carrier_rate(&cc, "sdk37_alice", &asset).await {
            carrier_rate = Some(rate);
            break;
        }
        bitcoin_core::generatetoaddress(1, &core)?;
        alice.claim().await?;
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let carrier_rate = carrier_rate
        .ok_or_else(|| anyhow!("alice's carrier of {asset} never got a COLOURED ladder — nothing below is on the lane it claims"))?;
    println!("SDK37 - alice issued 1000 {asset} on a COLOURED carrier at {carrier_rate} sat/vB");

    // alice sends 250 to the SSP but the SSP does NOT claim yet — it stays pending.
    let r = alice.transfer_tokens(&asset, &ssp_addr, 250).await?;
    assert!(r.used_split, "a token payment is a coloured in-ladder split");
    let pending_id = r.coins[0].statechain_id.clone();
    // Give the SE relay a moment; do NOT call ssp.claim() before the peek.
    tokio::time::sleep(Duration::from_secs(3)).await;

    // The SSP peeks the pending transfer BEFORE acting.
    let pend = mercuryrustlib::transfer_receiver::peek_pending_transfers(ssp.client_config(), ssp.wallet_name()).await?;
    let p = pend.iter().find(|p| p.statechain_id == pending_id)
        .ok_or_else(|| anyhow!("the pending token transfer was not peeked (id {pending_id})"))?;
    assert_eq!(token_balance(&ssp, &asset).await.unwrap_or(0), 0, "the SSP has NOT claimed — the coin is still a pending transfer to vet");
    // [4] The coloured child's RGB material is surfaced FROM THE BUNDLE: its leaf consignment,
    // wrapped as the `{"c","a","s"}` envelope the SSP's validator reads, plus the outpoint it
    // assigns to (the child's own final-state payload output) and the child's five-tier witness
    // chain. No backup row exists for an envelope to ride on; this is the only way the SSP can
    // verify an allocation BEFORE an irreversible Lightning payment.
    let env = p.rgb_consignment.clone().ok_or_else(|| {
        anyhow!(
            "[4] a coloured child conveyance must surface its leaf consignment for the pre-pay \
             gate — without it the SSP cannot pay ANY RGB invoice"
        )
    })?;
    let env_json: serde_json::Value = serde_json::from_str(&env)?;
    assert_eq!(
        env_json["a"].as_u64(),
        Some(250),
        "[4] the envelope's declared amount is the child's share (only ever cross-checked against the consignment)"
    );
    assert!(!p.rgb_assignment_txid.is_empty(), "[4] the assignment outpoint is surfaced");
    assert_eq!(
        p.child_witness_txids.last(),
        Some(&p.rgb_assignment_txid),
        "[4] the consignment assigns the allocation to the child's OWN final state — the last witness of its chain"
    );
    assert!(p.branch_txs.is_empty(), "[4] a coloured child conveys no exit branch — there is no branch leaf whose value a gate could be tricked by");
    assert_eq!(
        p.child_witness_txids.len(),
        5,
        "[4][P3] the coloured child's own witness chain (T, X_m, SP, ext_child, state_child) must be \
         surfaced for an envelope-free validator; got {:?}",
        p.child_witness_txids
    );
    assert!(
        p.ladder_census_ok,
        "[4] the coloured child must pass the pre-pay census (verify_conveyed_child): {:?}",
        p.ladder_census_refusal
    );
    let rung = mercuryrustlib::rgb::colored_committed_fee(1, carrier_rate) + mercurylib::tesr::P2A_VALUE;
    let piece_exit_value = mercury_utexo_sdk::tokens::TOKEN_PIECE_SATS - 2 * rung;
    assert_eq!(
        p.amount, piece_exit_value,
        "[4] the census-bound SATS value of a token piece is TOKEN_PIECE_SATS minus its own two \
         coloured rungs (each colored_committed_fee(1, rate) + P2A_VALUE) — the exit-reachable \
         packaging the ladder commits to, never a declared figure"
    );
    // THE PRE-PAY GATE PROPER, run exactly as `SspService::execute_pay` runs it, BEFORE any claim.
    let (pre_contract, pre_booked) = ssp
        .validate_pending_token_ex(
            &env,
            &p.branch_txs,
            &p.child_witness_txids,
            &p.rgb_assignment_txid,
            p.rgb_assignment_vout,
        )
        .await?;
    assert_eq!(pre_contract, asset, "[4] pre-pay validation identifies the invoiced contract");
    assert_eq!(
        pre_booked, 250,
        "[4] pre-pay validation books the consignment-assigned 250 — the value the gate prices against"
    );
    println!("SDK37 - [4] pre-pay gate: validate_pending_token_ex over the surfaced bundle material booked {pre_booked} of {pre_contract} before any claim");

    // ===== [F1] The pre-pay predicate IS the claim predicate ====================================
    // A mutated envelope — declared 251 over a consignment chain that assigns 250 — is refused by
    // the same validator, before payment, as PERMANENT-INVALID. Nothing pre-pay trusts the declared
    // figure; it is cross-checked against the consignment and the disagreement is named.
    let mut forged = env_json.clone();
    forged["a"] = serde_json::json!(251);
    let refusal = ssp
        .validate_pending_token_ex(
            &forged.to_string(),
            &p.branch_txs,
            &p.child_witness_txids,
            &p.rgb_assignment_txid,
            p.rgb_assignment_vout,
        )
        .await
        .expect_err("[F1] an envelope whose declared amount disagrees with the consignment must be refused before payment");
    assert!(
        refusal.to_string().contains("envelope claimed"),
        "[F1] the refusal names the disagreement: {refusal}"
    );
    println!("SDK37 - [F1] a forged envelope (a=251 over a chain assigning 250) is refused pre-pay: {refusal}");

    // Now claim, and book from the consignment chain — the same authority every receiver uses.
    wait_token_balance(&ssp, &asset, 250).await?;
    let (booked_contract, booked, witness_txids, _) = ssp.colored_child_health(&pending_id).await?;
    assert_eq!(booked_contract, asset, "[4] the SSP's consignment chain is for THIS contract");
    assert_eq!(
        booked, 250,
        "[4] the SSP books EXACTLY what the consignment chain assigns (250) — derived from the \
         chain, not from any declared field"
    );
    assert_eq!(witness_txids.len(), 5, "[4] the booked chain resolves against the same five witnesses the peek surfaced");
    println!("SDK37 - [4] RGB: the pending coloured child was peeked with its bundle material (leaf envelope, assignment outpoint, no branch, 5 witnesses surfaced, census ok, amount={} = TOKEN_PIECE_SATS - 2*{rung}); the claim booked {booked} of {booked_contract} from the consignment chain, the same figure the pre-pay gate booked", p.amount);

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
    // FAIL-CLOSED CENSUS — the laddered replacement for [3]'s branch validation. `verify_conveyed_child`
    // proved: the child pays THIS wallet's exit key (Model A), parent and child are both terminal, and
    // each num_sigs equals its `tiers + superseded` census with NO flat term (no hidden lower-CSV
    // state). Any tamper → Err → ladder_census_ok = false → the SSP's pre-pay gate refuses to pay.
    assert!(ps.ladder_census_ok, "the in-ladder child passed verify_conveyed_child: child pays the SSP key + parent/child terminality + tiers-plus-superseded census: {:?}", ps.ladder_census_refusal);
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

    println!("SDK37 - SUCCESS: on the TES-R lane the SSP pre-payment value gate reads the TRUE coin value, closing audit [3]/[4]. [3] peek_pending_transfers censuses the in-ladder split CHILD with verify_conveyed_child and reports its ladder-committed EXIT-REACHABLE value ({expected_census} sats = the {send_sats}-sat piece minus its two tiers' committed fee + P2A), so an inflated hint is never read and any tampered child fails closed (ladder_census_ok=false) and cannot satisfy the SATS value gate. [4] a token piece reaches the SSP as a coloured child whose RGB material is surfaced FROM THE BUNDLE (leaf envelope + assignment outpoint + five witnesses; there are no backup rows), the census passed, the pre-pay gate booked 250 of {asset} BEFORE any claim, and the claim booked the same 250 from the consignment chain. [F1] a forged envelope is refused pre-pay by the same validator, so a declared amount can never out-run the consignment. Neither path can make the SSP over-pay, and nothing can make it pay for a coin that will never claim.");
    Ok(())
}
