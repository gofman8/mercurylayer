//! E2E (SDK_E2E=87): **THE CARRIER VARIANT of the deadline pass — it leaves a laddered carrier
//! ALONE, at any margin, and the RGB-safe sever it used to reach for is still there when the OWNER
//! asks for it by name.**
//!
//! # What this test used to measure, and why that is now the wrong answer
//!
//! [D46] opened `deadline_safety_due`'s UNILATERAL route to token carriers: a carrier within
//! `margin_blocks` of its calendar floor was SEVERED by its own pre-signed `T` (never re-anchored —
//! a plain re-anchor spends the carrier's funding outpoint into a fresh aggregate and destroys the
//! allocation). The floor it measured against was the flat backup chain's absolute nLockTime,
//! booked on `coin.locktime`. That chain is gone: a carrier's ladder is co-signed at first sight of
//! `F` in place of the flat `tx1`, `coin.locktime` is `None` for life, and `coin_near_final` — the
//! predicate both routes of the pass select on — is therefore never true. "Near its deadline" is
//! not a state a laddered coin can be in.
//!
//! So the RIGHT thing for the pass to do to a carrier is NOTHING, and this test now asserts that in
//! each of the three ways "something" could have happened:
//!
//! * **(b) nothing re-anchored, nothing severed, nothing UNDEFENDED.** `deadline_safety_due` at a
//!   margin a hundred times the regtest `initlock` returns `Ok((vec![], vec![]))`. The `Ok` half
//!   matters as much as the empty vectors: [D51] made an `Err` from this pass mean "this wallet is
//!   unprotected", and a carrier it cannot find a floor for must not be reported that way. The
//!   coin's `locktime` is `None`; its exit estimate has no deadline, no blindness and no wait.
//!   `auto_exit_due` at the same margin is equally a no-op (an issued carrier has no exit branch).
//! * **(c) `F` was spent by NOTHING** — not by the trigger, not by a re-anchor. Nothing was
//!   broadcast: every tier of the ladder is still off-chain.
//! * **(d) the allocation is untouched**, by the consignment-chain authority (`colored_ladder_health`)
//!   and by the engine's own allocation set.
//! * **(e) CONTROL — the sever exists and is RGB-safe.** So that (b) is not "the pass did nothing
//!   because nothing works": the owner invokes `sever_from_f` by name ([D67]), `F` is spent by THIS
//!   coin's own trigger and the consignment chain still validates for the whole supply. The remedy
//!   is event-driven and owner-driven; it is not on a calendar.
//!
//! Run: SDK_E2E=87 ML_NETWORK=regtest cargo run   (regtest stack up)

use std::time::Duration;

use anyhow::{anyhow, Result};
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};
use mercurylib::wallet::CoinStatus;
use mercuryrustlib::client_config::ClientConfig;

use crate::bitcoin_core;
use crate::sdk40_tesr_consensus::is_outpoint_spent;

/// **WHICH transaction spent `(txid, vout)`** — not merely whether something did.
///
/// A re-anchor spends `F` exactly as a sever does, so "is F spent?" cannot tell the safe outcome
/// from the one that destroys the allocation. Walks the funding script's history and returns the
/// txid of the transaction that consumes the outpoint.
fn outpoint_spender(cc: &ClientConfig, txid: &str, vout: u32) -> Option<String> {
    use electrum_client::bitcoin::{consensus::deserialize, OutPoint, Transaction, Txid};
    use electrum_client::ElectrumApi;
    use std::str::FromStr;
    let f = Txid::from_str(txid).ok()?;
    let raw = cc.electrum_client.transaction_get_raw(&f).ok()?;
    let tx: Transaction = deserialize(&raw).ok()?;
    let spk = &tx.output.get(vout as usize)?.script_pubkey;
    let target = OutPoint { txid: f, vout };
    for h in cc.electrum_client.script_get_history(spk).ok()? {
        if h.tx_hash == f {
            continue;
        }
        let Ok(raw2) = cc.electrum_client.transaction_get_raw(&h.tx_hash) else { continue };
        let Ok(cand) = deserialize::<Transaction>(&raw2) else { continue };
        if cand.input.iter().any(|i| i.previous_output == target) {
            return Some(h.tx_hash.to_string());
        }
    }
    None
}

/// The transaction as the CHAIN has it, or `None` if the backend has never heard of it.
fn onchain(cc: &ClientConfig, txid: &str) -> Option<electrum_client::bitcoin::Transaction> {
    use electrum_client::bitcoin::Txid;
    use electrum_client::ElectrumApi;
    use std::str::FromStr;
    let t = Txid::from_str(txid).ok()?;
    cc.electrum_client.transaction_get(&t).ok()
}

const ALICE: &str = "sdk87_alice";
const SUPPLY: u64 = 5_000;
/// Far above any height a regtest coin could have been "due" at under the old calendar (`initlock`
/// is 1 000). The margin is a PARAMETER of the pass, so driving it this way exercises the same
/// branch a real deadline would have — and that branch must select nothing.
const HUGE_MARGIN: u32 = 100_000;

async fn wallet(name: &str) -> Result<UtexoWallet> {
    // [D30] `colored_ladder` SHIPS FALSE, so it must be set explicitly. A carrier on the plain
    // ladder is refused at build time by D35's lane rule, so without this the test would be about
    // a shape that cannot exist rather than about the deadline pass.
    let mut cfg = SdkConfig::regtest(name);
    cfg.colored_ladder = true;
    let (w, _) = UtexoWallet::initialize(cfg, None).await?;
    Ok(w)
}

async fn add_tokens(cc: &ClientConfig, w: &UtexoWallet, n: usize) -> Result<()> {
    for _ in 0..n {
        let t = mercuryrustlib::deposit::get_token(cc).await?;
        let id = crate::utils::handle_token_response(cc, &t).await?;
        w.add_prepaid_token(&id).await;
    }
    Ok(())
}

/// Every coloured ROOT carrier of `asset` this wallet holds.
async fn colored_carriers(
    cc: &ClientConfig,
    wallet_name: &str,
    asset: &str,
) -> Result<Vec<(String, mercuryrustlib::tesr::TesrBundle)>> {
    let rec = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, wallet_name).await?;
    let mut out = Vec::new();
    for c in rec
        .coins
        .iter()
        .filter(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0)
    {
        let Some(sid) = c.statechain_id.clone() else { continue };
        if let Some(b) = mercuryrustlib::tesr::load(cc, wallet_name, &sid).await? {
            if b.rgb.as_ref().is_some_and(|r| r.contract_id == asset) {
                out.push((sid, b));
            }
        }
    }
    Ok(out)
}

async fn token_balance(w: &UtexoWallet, asset: &str) -> Result<u64> {
    Ok(w.get_token_balances()
        .await?
        .into_iter()
        .find(|t| t.asset_id == asset)
        .map(|t| t.balance)
        .unwrap_or(0))
}

/// The independent authority on the allocation: the CONSIGNMENT CHAIN, validated by rgb-lib
/// against the tier witnesses. [D61]/[D70]: `colored_carriers` and `token_balance` both read the
/// persisted `rgb.amount` row, so two of them are one assertion in disguise; this reads neither.
async fn assert_allocation_intact(
    alice: &UtexoWallet,
    carrier_sid: &str,
    asset: &str,
    trigger_txid: &str,
    when: &str,
) -> Result<()> {
    let (health_contract, assigned_to_final, tier_txids, _detail) =
        alice.colored_ladder_health(carrier_sid).await.map_err(|e| {
            anyhow!("{when}: the carrier's coloured ladder no longer validates off-chain: {e:#}")
        })?;
    assert_eq!(
        assigned_to_final, SUPPLY,
        "{when}: the validated consignment chain assigns {assigned_to_final} units to the ladder's \
         final state, not {SUPPLY} — the allocation was damaged"
    );
    assert_eq!(health_contract, asset, "{when}: the ladder validates for the wrong contract");
    assert!(
        tier_txids.first().is_some_and(|t| t == trigger_txid),
        "{when}: the validated chain does not start at this carrier's trigger {trigger_txid} \
         (tiers: {tier_txids:?}) — it is proving something about a different ladder"
    );
    let allocations = alice.list_token_allocations(asset).await?;
    let total: u64 = allocations.iter().map(|(_, amt)| *amt).sum();
    assert_eq!(
        total, SUPPLY,
        "{when}: the RGB engine accounts for {total} units of {asset}, not {SUPPLY}: {allocations:?}"
    );
    assert_eq!(token_balance(alice, asset).await?, SUPPLY, "{when}: alice's balance must be whole");
    Ok(())
}

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    let _ = std::fs::remove_dir_all("./rgb-data-sdk87_alice");
    let alice = wallet(ALICE).await?;
    let cc = alice.client_config().clone();
    let core = bitcoin_core::getnewaddress()?;
    let initlock = mercuryrustlib::utils::info_config(&cc).await?.initlock;
    assert!(
        HUGE_MARGIN > initlock,
        "test hygiene: the margin ({HUGE_MARGIN}) must exceed initlock ({initlock}), or 'the pass \
         selected nothing' would prove nothing"
    );

    // ===== (a) A COLOURED CARRIER =================================================================
    // Fund alice's RGB engine — issuance is the ISSUER's on-chain cost and it is paid from a plain
    // wallet the engine controls, not from a statechain coin.
    let rgb_fund = alice.get_token_funding_address().await?;
    bitcoin_core::sendtoaddress(600_000, &rgb_fund)?;
    bitcoin_core::generatetoaddress(3, &core)?;
    tokio::time::sleep(Duration::from_secs(4)).await;

    add_tokens(&cc, &alice, 4).await?;
    let asset = alice.issue_token("CDL", "Carrier Deadline", 0, SUPPLY).await?;
    let mut carriers = Vec::new();
    for _ in 0..120 {
        bitcoin_core::generatetoaddress(1, &core)?;
        alice.claim().await?;
        carriers = colored_carriers(&cc, ALICE, &asset).await?;
        if carriers.iter().any(|(_, b)| b.is_colored()) {
            carriers.retain(|(_, b)| b.is_colored());
            break;
        }
        carriers.clear();
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let (carrier_sid, carrier) = carriers
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("alice never got a COLOURED carrier of {asset}"))?;
    assert!(carrier.is_colored(), "the carrier must be on the coloured ladder");
    assert_eq!(
        carrier.rgb.as_ref().unwrap().amount,
        SUPPLY,
        "the carrier holds the whole issuance"
    );
    let f_txid = carrier.f_txid.clone();
    let f_vout = carrier.f_vout;
    let trigger_txid = carrier.trigger.txid.clone();
    let tier_txids: Vec<String> = carrier.exit_tiers().iter().map(|t| t.txid.clone()).collect();
    assert!(
        !is_outpoint_spent(&cc, &f_txid, f_vout),
        "a resting carrier publishes nothing — F must be unspent before the pass"
    );
    assert_eq!(token_balance(&alice, &asset).await?, SUPPLY);
    println!(
        "SDK87 - coloured carrier {carrier_sid} holds {SUPPLY} units; F {f_txid}:{f_vout} unspent, \
         its trigger would be {trigger_txid}"
    );

    // The wallet must AGREE it is a carrier — the cooperative route's exclusion keys on exactly this
    // set, and if the carrier were not in it, half of what (b) asserts would be vacuous.
    let carrier_outpoints = alice.list_token_allocations(&asset).await?;
    assert!(
        carrier_outpoints.iter().any(|(op, _)| op == &format!("{f_txid}:{f_vout}")),
        "the wallet does not classify its own carrier as one, so the cooperative route's exclusion \
         is not being exercised and (b) would prove nothing"
    );

    // ===== (b) THE PASS LEAVES IT ALONE — AT ANY MARGIN ===========================================
    //
    // There is no calendar on this coin for a margin to be measured against: `locktime` is `None`,
    // the exit estimate has no deadline, and `coin_near_final` is never true. So the pass must
    // select NOTHING, and must say so with `Ok`, not with an `Err` naming the carrier UNDEFENDED.
    let carrier_coin = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, ALICE)
        .await?
        .coins
        .into_iter()
        .find(|c| c.statechain_id.as_deref() == Some(carrier_sid.as_str()) && c.duplicate_index == 0)
        .ok_or_else(|| anyhow!("alice's carrier coin vanished"))?;
    assert_eq!(
        carrier_coin.locktime, None,
        "a laddered carrier has no absolute calendar: coin.locktime must be None"
    );
    let est = alice.estimate_exit_cost(&carrier_sid).await?;
    assert_eq!(est.branch_txs, 0, "an issued carrier has no exit branch");
    assert_eq!(est.exit_deadline_block, None, "no exit-race deadline exists for a laddered carrier");
    assert!(
        !est.deadline_is_unknown(),
        "the absent deadline is SAFE, not blind: {:?}",
        est.exit_deadline_blind
    );
    assert_eq!(est.wait_blocks, 0, "nothing on a laddered carrier matures on its own");

    let (re_anchored, severed) = alice
        .deadline_safety_due(HUGE_MARGIN)
        .await
        .map_err(|e| {
            anyhow!(
                "[D51] the deadline pass returned Err on a wallet holding one laddered carrier: \
                 {e:#}. Err from this pass is read as \"this wallet is unprotected\", and a carrier \
                 with no calendar floor must not be reported UNDEFENDED — nor may the pass go blind."
            )
        })?;
    assert!(
        re_anchored.is_empty(),
        "[D46] THE CARRIER WAS RE-ANCHORED at margin {HUGE_MARGIN}. A plain re-anchor spends the \
         carrier's funding outpoint into a fresh aggregate and DESTROYS the RGB allocation — and \
         it found a calendar floor on a coin that has none. Got: {re_anchored:?}"
    );
    assert!(
        severed.is_empty(),
        "the carrier was SEVERED at margin {HUGE_MARGIN}: the pass found a calendar deadline on a \
         coin that has none and converted a resting carrier into a walking exit for no reason. \
         Severed: {severed:?}"
    );
    let acted = alice.auto_exit_due(HUGE_MARGIN).await.map_err(|e| {
        anyhow!("auto_exit_due must not go blind on an issued carrier: {e:#}")
    })?;
    assert!(
        acted.is_empty(),
        "auto_exit_due at margin {HUGE_MARGIN} acted on {acted:?}: an issued carrier has no exit \
         branch and therefore no deadline"
    );
    println!(
        "SDK87 - (b) deadline_safety_due({HUGE_MARGIN}) -> Ok(([], [])) and auto_exit_due -> []: the \
         laddered carrier has no calendar floor to be near (locktime=None, deadline=None, wait=0)"
    );

    // ===== (c) F WAS SPENT BY NOTHING — AND NOTHING WAS BROADCAST ==================================
    //
    // "Is F spent?" cannot tell a sever from a re-anchor; here it must be neither. Every tier of the
    // ladder is still off-chain.
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        outpoint_spender(&cc, &f_txid, f_vout),
        None,
        "F {f_txid}:{f_vout} was SPENT by the deadline pass — by the trigger or by a re-anchor, and \
         neither is a thing a calendar-free coin can be due for"
    );
    for t in tier_txids.iter() {
        assert!(
            onchain(&cc, t).is_none(),
            "tier {t} reached the chain — the deadline pass broadcast a tier of a resting carrier"
        );
    }
    println!("SDK87 - (c) F unspent by anything; all {} tiers still off-chain", tier_txids.len());

    // ===== (d) THE ALLOCATION IS UNTOUCHED ========================================================
    assert_allocation_intact(&alice, &carrier_sid, &asset, &trigger_txid, "(d) after the pass").await?;
    let after = colored_carriers(&cc, ALICE, &asset).await?;
    assert!(
        after.iter().any(|(sid, b)| sid == &carrier_sid
            && b.rgb.as_ref().is_some_and(|r| r.amount == SUPPLY)),
        "the carrier's bundle no longer resolves to {SUPPLY} units of {asset} after the pass"
    );
    println!("SDK87 - (d) the consignment chain still validates for {SUPPLY} units; nothing moved");

    // ===== (e) CONTROL: THE SEVER EXISTS, AND IT IS RGB-SAFE — WHEN THE OWNER ASKS ===============
    //
    // Without this, (b) could be true because nothing works. The remedy [D46] built is still there;
    // what changed is WHO invokes it and WHY: never a calendar, only an owner (or a hostile trigger
    // answered by `defend_ladders`). `sever_from_f` is the NAMED remedy ([D67]) and it is
    // `unilateral_exit` on one coin — the pre-signed `T`, a tier of the coin's OWN ladder carrying
    // its own state. It does not re-aggregate and it does not move the allocation.
    let statuses = alice.sever_from_f(&carrier_sid).await.map_err(|e| {
        anyhow!("sever_from_f refused the owner's own coloured carrier: {e:#}")
    })?;
    assert!(
        !statuses.is_empty(),
        "sever_from_f reported no exit status at all — it neither severed nor said why"
    );
    let mut spender = None;
    for _ in 0..30 {
        if let Some(sp) = outpoint_spender(&cc, &f_txid, f_vout) {
            spender = Some(sp);
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    let spender = spender.ok_or_else(|| {
        anyhow!("(e) F {f_txid}:{f_vout} was never spent, so the owner's sever did nothing")
    })?;
    assert_eq!(
        spender, trigger_txid,
        "(e) F was spent by {spender}, which is NOT this coin's own trigger {trigger_txid}. The \
         sever must be the coin's OWN pre-signed T; anything else re-aggregates, and on a carrier \
         that is the allocation gone."
    );
    bitcoin_core::generatetoaddress(2, &core)?;
    tokio::time::sleep(Duration::from_secs(2)).await;
    alice.claim().await?;
    // [D70] The engine's allocation set still reports the units at the SPENT funding outpoint after
    // a sever: a CTES-R tier is not an rgb-lib transfer, so there is nothing for a sync to settle
    // until the walk completes. Measured, not a loss — the consignment-chain authority is what says
    // the allocation survived, and it is asserted inside `assert_allocation_intact`.
    assert_allocation_intact(&alice, &carrier_sid, &asset, &trigger_txid, "(e) after the sever")
        .await?;
    println!(
        "SDK87 - (e) sever_from_f spent F with the carrier's OWN trigger {trigger_txid}; the \
         consignment chain still validates for {SUPPLY} units — the remedy is owner-driven, not \
         calendar-driven"
    );

    println!(
        "SDK87 PASS - a laddered carrier has no calendar: the deadline pass at margin {HUGE_MARGIN} \
         re-anchors nothing, severs nothing and reports nothing undefended; F stays unspent and \
         every tier off-chain; the allocation is untouched. The RGB-safe sever still exists and \
         still preserves the allocation when the owner invokes it by name."
    );
    Ok(())
}
