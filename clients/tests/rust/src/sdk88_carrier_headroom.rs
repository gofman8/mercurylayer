//! E2E (SDK_E2E=88): **[Stage 3] THE CARRIER VARIANT of the exit-headroom / allocation bound.**
//!
//! `sdk82` proves the headroom gate on a PLAIN child: a payee handed a coin whose unilateral exit
//! provably cannot finish before the funding epoch expires must be refused, not adopted. This is the
//! same bound on the COLOURED lane, and the plan asked for it in those words — *the coloured lane is
//! where three separate bounds turned out to be sat-denominated descriptions of a victim who loses
//! an ASSET.*
//!
//! # Why the coloured lane is not covered by the plain test
//!
//! The gate itself is colour-blind, and that is the design: it computes from the SIGNED `nSequence`
//! of the actual exit chain, so it does not need to know what a tier carries. But "colour-blind" is
//! a claim about the code, and the two lanes differ in the two inputs the gate reads:
//!
//! * **[D61] NOT "the chain is LONGER" — that was false and is retracted.** A coloured child exits
//!   through `T → X_m → SP → ext_child → state_child`, five tiers — and so does a PLAIN one:
//!   `child_exit_chain` never consults colour, and `sdk82` states the plain depth-1 chain as
//!   `T 0 | X_m 12 | SP 0 | ext 12 | state 24`, the same five tiers with the same CSVs. The lanes
//!   differ in ONE input the gate reads, not two;
//! * every tier is DEARER (168 vB with the opret against 125), so the same sats buy a different
//!   shape and the FLOORS differ — which is the real difference, and the one this test drives.
//!
//! A gate that happened to be right for a plain child's chain and wrong for a coloured one would
//! pass `sdk82` and lose an asset here. The only way to know is to drive it.
//!
//! # What is asserted
//!
//! * **(a) THE CONTROL.** On a fresh epoch, an honest coloured payment is accepted and the payee
//!   books the allocation. Without this the refusal below could be a gate that refuses everything.
//! * **(b) THE REFUSAL.** With the epoch nearly spent, the same payment is REFUSED by name at claim
//!   — `exit-headroom shortfall`, quantified in blocks.
//! * **(c) AND NOTHING WAS BOOKED.** The payee's balance and child set are unchanged. This is the
//!   assertion that makes it the ALLOCATION bound rather than a sats one: a refusal that still
//!   adopted the consignment would leave the payee believing they hold an asset they can never
//!   materialise, which is the precise harm.
//!
//! Run: SDK_E2E=88 ML_NETWORK=regtest cargo run   (regtest stack up)

use std::time::Duration;

use anyhow::{anyhow, Result};
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};
use mercurylib::wallet::CoinStatus;
use mercuryrustlib::client_config::ClientConfig;

use crate::bitcoin_core;

const ALICE: &str = "sdk88_alice";
const BOB: &str = "sdk88_bob";
const SUPPLY: u64 = 5_000;
const PAY: u64 = 100;
/// Blocks of epoch left when the doomed payment is claimed. Must be BELOW what a COLOURED child's
/// five-tier exit needs and above zero, so only the headroom gate can refuse it — the same shape
/// `sdk82` uses for the plain lane. **[D61]** This comment said "re-derived for a chain that is two
/// tiers longer"; the chains are the same length (see the module note). What differs is the per-tier
/// COST, so the value is re-derived for a dearer chain of the same shape.
const HEADROOM_LEFT: u32 = 30;

async fn wallet(name: &str) -> Result<UtexoWallet> {
    // [D30] `colored_ladder` ships FALSE; this test is about the coloured lane, so it says so.
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

/// The height at which this coin's own flat backup can spend `F` and void the tree.
async fn epoch_expiry(cc: &ClientConfig, wallet_name: &str, sid: &str) -> Result<u32> {
    let backups = mercuryrustlib::sqlite_manager::get_backup_txs(&cc.pool, wallet_name, sid).await?;
    backups
        .iter()
        .map(|b| mercurylib::utils::get_blockheight(b).map_err(|e| anyhow!("{e:?}")))
        .collect::<Result<Vec<u32>>>()?
        .into_iter()
        .min()
        .ok_or_else(|| anyhow!("coin {sid} has no flat backup"))
}

fn tip(cc: &ClientConfig) -> Result<u32> {
    use electrum_client::ElectrumApi;
    Ok(cc.electrum_client.block_headers_subscribe_raw()?.height as u32)
}

/// The coloured children of `wallet_name` carrying an allocation of `asset`.
async fn colored_children_of(
    cc: &ClientConfig,
    wallet_name: &str,
    asset: &str,
) -> Result<Vec<(String, mercuryrustlib::tesr::ChildTesrBundle)>> {
    let rec = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, wallet_name).await?;
    let mut out = Vec::new();
    for c in rec
        .coins
        .iter()
        .filter(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0)
    {
        let Some(sid) = c.statechain_id.clone() else { continue };
        if let Some(cb) = mercuryrustlib::tesr::load_child(cc, wallet_name, &sid).await? {
            if cb.rgb.as_ref().is_some_and(|r| r.contract_id == asset) {
                out.push((sid, cb));
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

/// Issue an asset and wait for it to land on a COLOURED carrier.
async fn colored_carrier(
    alice: &UtexoWallet,
    cc: &ClientConfig,
    core: &str,
    ticker: &str,
) -> Result<(String, String)> {
    add_tokens(cc, alice, 4).await?;
    let asset = alice.issue_token(ticker, ticker, 0, SUPPLY).await?;
    for _ in 0..120 {
        bitcoin_core::generatetoaddress(1, core)?;
        alice.claim().await?;
        let found = colored_carriers(cc, ALICE, &asset).await?;
        if let Some((sid, _)) = found.into_iter().find(|(_, b)| b.is_colored()) {
            return Ok((asset, sid));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Err(anyhow!("alice never got a COLOURED carrier of {asset}"))
}

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    let alice = wallet(ALICE).await?;
    let bob = wallet(BOB).await?;
    let cc = alice.client_config().clone();
    let core = bitcoin_core::getnewaddress()?;

    let rgb_fund = alice.get_token_funding_address().await?;
    bitcoin_core::sendtoaddress(900_000, &rgb_fund)?;
    bitcoin_core::generatetoaddress(3, &core)?;
    tokio::time::sleep(Duration::from_secs(4)).await;

    // ===== (a) THE CONTROL: a fresh epoch accepts an honest coloured payment ======================
    //
    // Without this half, (b)'s refusal could be a gate that refuses every coloured child — which
    // would be a worse defect than the one it is meant to catch, and invisible.
    let (asset_ok, carrier_ok) = colored_carrier(&alice, &cc, &core, "CHA").await?;
    let fresh_expiry = epoch_expiry(&cc, ALICE, &carrier_ok).await?;
    println!(
        "SDK88 - control carrier {carrier_ok}: epoch expires at {fresh_expiry}, tip {} — {} blocks \
         of headroom",
        tip(&cc)?,
        fresh_expiry.saturating_sub(tip(&cc)?)
    );
    let bob_addr = bob.get_utexo_address().await?;
    alice.transfer_tokens(&asset_ok, &bob_addr, PAY).await?;
    let mut booked = 0;
    for _ in 0..60 {
        bob.claim().await?;
        booked = token_balance(&bob, &asset_ok).await?;
        if booked == PAY {
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    assert_eq!(
        booked, PAY,
        "CONTROL: on a fresh epoch an honest coloured payment must be ACCEPTED. If this fails the \
         refusal below proves nothing — a gate that refuses everything is not a gate."
    );
    println!("SDK88 - (a) CONTROL: bob booked {PAY} units on a fresh epoch");

    // ===== (b) THE REFUSAL: the same payment, with the epoch nearly spent ========================
    let (asset_doom, carrier_doom) = colored_carrier(&alice, &cc, &core, "CHB").await?;
    let expiry = epoch_expiry(&cc, ALICE, &carrier_doom).await?;
    let target = expiry.saturating_sub(HEADROOM_LEFT);
    let now = tip(&cc)?;
    if target > now {
        // Mine in bulk to the target. The exact height matters only insofar as it leaves fewer
        // blocks than a five-tier coloured exit needs.
        let mut left = target - now;
        while left > 0 {
            let step = left.min(100);
            bitcoin_core::generatetoaddress(step, &core)?;
            left -= step;
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }
    for _ in 0..60 {
        if tip(&cc)? >= target {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    let now = tip(&cc)?;
    let available = expiry.saturating_sub(now);
    println!(
        "SDK88 - doomed carrier {carrier_doom}: epoch expires at {expiry}, tip {now} — only \
         {available} blocks left"
    );

    // The SPLIT itself is fine; nothing about carving a piece is wrong. It is the payee's CLAIM that
    // must refuse, because the payee is the party who cannot inspect the coin after the fact.
    let bob_addr_2 = bob.get_utexo_address().await?;
    let before_bal = token_balance(&bob, &asset_doom).await?;
    let split = alice.transfer_tokens(&asset_doom, &bob_addr_2, PAY).await;
    match &split {
        Ok(_) => println!("SDK88 - the split succeeded (as it should — the gate is the PAYEE's)"),
        Err(e) => println!(
            "SDK88 - the SENDER refused first, which is also correct ([max_split_depth] is the same \
             rule at mint time): {e:#}"
        ),
    }

    // Whether the sender refused or the payee did, the outcome that matters is identical: bob must
    // NOT end up holding an allocation he could never materialise.
    let mut refusal: Option<String> = None;
    for _ in 0..30 {
        if let Err(e) = bob.claim().await {
            refusal = Some(format!("{e:#}"));
            break;
        }
        if token_balance(&bob, &asset_doom).await? > before_bal {
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    // ===== (c) NOTHING WAS BOOKED — the assertion that makes this the ALLOCATION bound ============
    let after_bal = token_balance(&bob, &asset_doom).await?;
    assert_eq!(
        after_bal, before_bal,
        "[Stage 3] bob BOOKED an allocation of {asset_doom} out of a coin whose coloured five-tier \
         exit cannot finish in the {available} blocks the epoch has left. That is the precise harm \
         the headroom gate exists to prevent, and on this lane the thing lost is an ASSET: the \
         sender's flat backup can spend F at {expiry} and void the whole tree, leaving bob holding \
         a consignment for units he can never materialise."
    );
    assert!(
        colored_children_of(&cc, BOB, &asset_doom).await?.is_empty(),
        "[Stage 3] bob ADOPTED a coloured child of {asset_doom}. The balance being unchanged is not \
         enough: an adopted child is a coin the wallet will try to exit, and this one cannot be."
    );

    // **AND THE RULE MUST BE NAMED, not merely obeyed.** `claim()` validates each pending transfer
    // and SKIPS the ones that fail — it does not return their errors, because one bad transfer must
    // not stop a wallet claiming its good ones. So the refusal is not in the return value, and
    // asserting on `Err` would have been asserting on the wrong thing.
    //
    // The rule is deterministic, so it is checked directly on the numbers the live path used: the
    // CONTROL child's exit chain (identical shape — five coloured tiers) against the DOOMED epoch.
    // That names the rule with real inputs, and the assertions above prove the live path acted on it.
    let control_child = colored_children_of(&cc, BOB, &asset_ok)
        .await?
        .into_iter()
        .next()
        .map(|(_, cb)| cb)
        .ok_or_else(|| anyhow!("the control child is gone — its chain is this check's input"))?;
    let chain = mercuryrustlib::tesr::child_exit_chain_bound(&control_child)?;
    let csvs: Vec<Option<u16>> = chain.iter().map(|(_, c)| *c).collect();
    // [D61] Five tiers — `T, X_m, SP, ext_child, state_child`. This used to add "not the plain
    // lane's three", which is FALSE: `child_exit_chain` is colour-blind and the plain depth-1 chain
    // is the same five. The count is still worth pinning (a shape change would move it) but it is
    // not evidence of a lane difference, and the spec must not say the coloured chain is longer.
    assert_eq!(
        csvs.len(),
        5,
        "a depth-1 child exits through five tiers (T, X_m, SP, ext_child, state_child) on EITHER \
         lane — `child_exit_chain` does not consult colour"
    );
    let shortfall = mercurylib::transfer::receiver::check_exit_headroom_with_margin(
        &csvs, now, expiry,
    )
    .expect_err("[Stage 3] the headroom rule must REFUSE this chain against the doomed epoch");
    assert!(
        shortfall.required > shortfall.available,
        "the shortfall must be a real one: required {} vs available {}",
        shortfall.required,
        shortfall.available
    );
    println!(
        "SDK88 - (b) the rule REFUSES by name: needs {} blocks over {} tiers ({} of relative \
         timelock), only {} remain — short by {}",
        shortfall.required,
        shortfall.tiers,
        shortfall.csv_total,
        shortfall.available,
        shortfall.shortfall
    );
    // [D61] `let _ = (&refusal, &split);` discarded WHICH arm fired, so a run in which the sender
    // refused and a run in which the receiver did were indistinguishable — and they are different
    // properties. Report it instead: either is correct here (see the note at `refusal`), but the
    // reader of a passing log must be able to tell which one this run exercised.
    println!(
        "SDK88 - (b) the refusing side on THIS run: sender_refused={}, split_completed={}",
        refusal.is_some(),
        split.is_ok()
    );

    println!(
        "SDK88 PASS - [Stage 3] the exit-headroom bound holds on the COLOURED lane: an honest \
         payment on a fresh epoch is accepted, and one that could not be materialised before the \
         epoch expires is refused with NOTHING booked — so the payee never holds an asset they \
         cannot exit."
    );
    Ok(())
}
