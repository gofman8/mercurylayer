//! E2E (SDK_E2E=87): **[D46 / Stage 3] THE CARRIER VARIANT of the deadline pass.**
//!
//! The plan listed "the carrier variant of every deadline and allocation test" as owed, with the
//! reason: *the coloured lane is where three separate bounds turned out to be sat-denominated
//! descriptions of a victim who loses an ASSET.* This is that test for the deadline bound, and it is
//! the live exercise of [D46], which until now had only a CI guard.
//!
//! # What D46 changed, and why a carrier needed its own test
//!
//! `deadline_safety_due` has two routes. The COOPERATIVE one (`auto_refresh_due`) excludes token
//! carriers, correctly and permanently: a plain re-anchor spends the carrier's funding outpoint into
//! a fresh aggregate and **destroys its RGB allocation**. The UNILATERAL one excluded them too — and
//! there the exclusion had no justification. It left the carrier lane with **zero** automatic
//! deadline coverage, in the one lane where the loss is an asset rather than sats.
//!
//! The forced action is the coin's own pre-signed `T`: a tier of its own ladder, carrying its own
//! state. It does not re-aggregate and it does not move the allocation, which is precisely why it is
//! safe on a carrier where a re-anchor is not. **That distinction is what this test measures** — not
//! that something happened, but that the RIGHT thing happened and the allocation survived it.
//!
//! # Structure
//!
//! * **(a)** A coloured carrier exists, and the wallet agrees it is one.
//! * **(b)** `deadline_safety_due` SEVERS it and does NOT re-anchor it. The carrier's id appears in
//!   the severed list and in no re-anchor result — the two outcomes are asserted separately,
//!   because "something was done" is exactly the claim that would hide a destroyed allocation.
//! * **(c)** `F` is spent **by this coin's own trigger**, not by anything else. A re-anchor also
//!   spends `F`; only the spender's txid tells them apart, and getting that wrong is the whole
//!   failure mode.
//! * **(d)** The allocation SURVIVED. The ledger still assigns alice her raw units, and the carrier's
//!   own bundle still resolves — which is the assertion the sat-denominated version of this test
//!   could not make.
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

const ALICE: &str = "sdk87_alice";
const SUPPLY: u64 = 5_000;
/// Far above any coin's remaining calendar on regtest (`initlock` is 1 000), so every laddered coin
/// is "due" without mining a thousand blocks. The margin is a PARAMETER of the pass, so driving it
/// this way exercises the same branch a real deadline would, at a hundredth of the wall-clock.
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

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    let alice = wallet(ALICE).await?;
    let cc = alice.client_config().clone();
    let core = bitcoin_core::getnewaddress()?;

    // ===== (a) A COLOURED CARRIER =================================================================
    // Fund alice's RGB engine — issuance is the ISSUER's on-chain cost and it is paid from a plain
    // wallet the engine controls, not from a statechain coin.
    let rgb_fund = alice.get_token_funding_address().await?;
    bitcoin_core::sendtoaddress(600_000, &rgb_fund)?;
    bitcoin_core::generatetoaddress(3, &core)?;
    tokio::time::sleep(Duration::from_secs(4)).await;

    add_tokens(&cc, &alice, 4).await?;
    let asset = alice.issue_token("CDL", "Carrier Deadline", 0, SUPPLY).await?;
    // The issuance witness must CONFIRM and the carrier must be claimed before it can be laddered —
    // mining and claiming in the same loop, exactly as the token E2Es do.
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

    // ===== (b) THE PASS SEVERS IT, AND DOES NOT RE-ANCHOR IT ======================================
    //
    // Two outcomes, asserted separately and in opposite directions. "The pass did something" is the
    // claim that would hide a destroyed allocation: a re-anchor also acts, also reports success, and
    // also leaves the coin looking healthy — right up until the asset is gone.
    let (re_anchored, severed) = alice
        .deadline_safety_due(HUGE_MARGIN)
        .await
        .map_err(|e| anyhow!("[D46] the deadline pass must not go blind on a carrier: {e:#}"))?;

    assert!(
        !re_anchored.iter().any(|r| r.new_statechain_id == carrier_sid
            || r.old_statechain_id == carrier_sid),
        "[D46] THE CARRIER WAS RE-ANCHORED. A plain re-anchor spends the carrier's funding outpoint \
         into a fresh aggregate and DESTROYS the RGB allocation — this is the outcome the \
         cooperative route excludes carriers to prevent, and it must never be reachable from this \
         pass. Got: {re_anchored:?}"
    );
    assert!(
        severed.contains(&carrier_sid),
        "[D46] the carrier was NOT severed. Before D46 it was excluded from BOTH routes, which left \
         the carrier lane with zero automatic deadline coverage — in the one lane where the loss is \
         an ASSET. Severed: {severed:?}"
    );
    println!("SDK87 - [D46] the pass SEVERED the carrier and did not re-anchor it: {severed:?}");

    // ===== (c) F WAS SPENT BY THIS COIN'S OWN TRIGGER =============================================
    //
    // A re-anchor spends `F` too. Only the SPENDER tells the two apart, so asking "is F spent?"
    // would pass for the outcome (b) exists to forbid.
    let mut spender = None;
    for _ in 0..30 {
        if let Some(sp) = outpoint_spender(&cc, &f_txid, f_vout) {
            spender = Some(sp);
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    let spender = spender.ok_or_else(|| {
        anyhow!("[D46] F {f_txid}:{f_vout} was never spent, so nothing was actually severed")
    })?;
    assert_eq!(
        spender, trigger_txid,
        "[D46] F was spent by {spender}, which is NOT this coin's own trigger {trigger_txid}. The \
         forced action must be the coin's OWN pre-signed T — a tier of its own ladder carrying its \
         own state. Anything else re-aggregates, and on a carrier that is the allocation gone."
    );
    println!("SDK87 - F was spent by the carrier's OWN trigger {trigger_txid} — no re-aggregation");

    // ===== (d) THE ALLOCATION SURVIVED ============================================================
    //
    // The assertion the sat-denominated version of this test could not make. A sats-only check would
    // have passed for every outcome above, including the one that destroys the asset.
    bitcoin_core::generatetoaddress(2, &core)?;
    tokio::time::sleep(Duration::from_secs(2)).await;
    alice.claim().await?;
    let after = colored_carriers(&cc, ALICE, &asset).await?;
    assert!(
        after.iter().any(|(sid, b)| sid == &carrier_sid
            && b.rgb.as_ref().is_some_and(|r| r.amount == SUPPLY)),
        "[D46] the carrier's bundle no longer resolves to {SUPPLY} units of {asset} after the \
         sever. The whole point of forcing `T` rather than a re-anchor is that the allocation rides \
         the coin's own ladder — if it does not survive here, the unilateral route is as unsafe on a \
         carrier as the cooperative one."
    );
    assert_eq!(
        token_balance(&alice, &asset).await?,
        SUPPLY,
        "[D46] alice's balance must still be {SUPPLY} after the deadline pass severed her carrier"
    );

    println!(
        "SDK87 PASS - [D46] a carrier near its deadline is SEVERED by its own pre-signed T, never \
         re-anchored, and its allocation survives intact. Before D46 the carrier lane had NO \
         automatic deadline coverage at all."
    );
    Ok(())
}
