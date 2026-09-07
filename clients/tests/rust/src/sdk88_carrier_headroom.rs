//! E2E (SDK_E2E=88): **[Stage 3] THE CARRIER VARIANT — a coloured child has NO EPOCH to fit inside.**
//!
//! `sdk82` proves on a PLAIN child that a conveyed child has no funding epoch to run out of: the
//! flat backup that used to mature at `H_deposit + initlock`, spend `F` and void the tree does not
//! exist, so a payment from a coin aged past `initlock` is admitted exactly like a fresh one. This
//! is the same property on the COLOURED lane, and the plan asked for it in those words — *the
//! coloured lane is where three separate bounds turned out to be sat-denominated descriptions of a
//! victim who loses an ASSET.*
//!
//! # Why the coloured lane is not covered by the plain test
//!
//! The rule is colour-blind, and that is the design: nothing on any coin carries an absolute
//! calendar, so there is no gate to consult. But "colour-blind" is a claim about the code, and the
//! lanes differ in what a calendar would have cost:
//!
//! * **[D61] NOT "the chain is LONGER" — that was false and is retracted.** A coloured child exits
//!   through `T → X_m → SP → ext_child → state_child`, five tiers — and so does a PLAIN one:
//!   `child_exit_chain` never consults colour;
//! * every tier is DEARER (168 vB with the opret against 125) and the thing at stake is an
//!   ALLOCATION: under the old rule a coloured carrier's flat backup was an RGB-unaware spend of `F`
//!   that BURNED the allocation the moment it matured. A calendar surviving on this lane — a
//!   coloured `tx1`, a coloured deadline pass, a sender refusing an "old" carrier — would lose an
//!   asset where the plain lane loses sats.
//!
//! # What is asserted
//!
//! * **(a) THE CONTROL.** On a fresh carrier, an honest coloured payment is accepted and the payee
//!   books the allocation. Without this the admission below could be a lane that accepts anything.
//! * **(b) NO EPOCH.** A second carrier is issued and then aged `initlock + 60` blocks — past the
//!   height at which its old flat backup would have matured and burned the allocation. Its `F` is
//!   still UNSPENT after the mining (nothing on the carrier can mature), the SAME payment is made
//!   and BOOKED, the payee adopts a coloured child carrying `parent_flat_backups: []` whose signed
//!   exit chain is the five-tier schedule, and `F` is still unspent after the payment (the split is
//!   off-chain).
//! * **(c) AND NOTHING WAS LOST.** The payee's balance is exactly the payment; the adopted child's
//!   coin has no calendar (`locktime == None`).
//!
//! Run: SDK_E2E=88 ML_NETWORK=regtest cargo run   (regtest stack up)

use std::time::Duration;

use anyhow::{anyhow, Result};
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};
use mercurylib::wallet::CoinStatus;
use mercuryrustlib::client_config::ClientConfig;

use crate::bitcoin_core;
use crate::sdk40_tesr_consensus::is_outpoint_spent;

const ALICE: &str = "sdk88_alice";
const BOB: &str = "sdk88_bob";
const SUPPLY: u64 = 5_000;
const PAY: u64 = 100;
/// Blocks mined PAST `initlock` before the aged payment. Under the old rule the carrier's flat backup
/// matured AT `initlock` and burned the allocation; anything past it is a carrier whose every child
/// the old gate refused outright.
const PAST_EPOCH: u32 = 60;

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

fn tip(cc: &ClientConfig) -> Result<u32> {
    use electrum_client::ElectrumApi;
    Ok(cc.electrum_client.block_headers_subscribe_raw()?.height as u32)
}

/// Mine the chain to `target` in chunks and wait until electrs has indexed it. A pending chunk is
/// waited out, never re-issued, so a slow index cannot double-mine the stretch.
async fn mine_to(cc: &ClientConfig, core: &str, target: u32) -> Result<()> {
    let mut mined_to = tip(cc)?;
    while mined_to < target {
        let step = (target - mined_to).min(200);
        bitcoin_core::generatetoaddress(step, core)?;
        mined_to += step;
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    let mut waited = 0;
    while tip(cc)? < target {
        waited += 1;
        if waited > 900 {
            return Err(anyhow!("electrs did not catch up to {target}"));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    Ok(())
}

/// The coloured children of `wallet_name` carrying an allocation of `asset`, with the coin each
/// one is booked as.
async fn colored_children_of(
    cc: &ClientConfig,
    wallet_name: &str,
    asset: &str,
) -> Result<Vec<(mercurylib::wallet::Coin, mercuryrustlib::tesr::ChildTesrBundle)>> {
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
                out.push((c.clone(), cb));
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

/// Issue an asset and wait for it to land on a COLOURED carrier. Returns the asset id, the
/// carrier's statechain id and its funding outpoint.
async fn colored_carrier(
    alice: &UtexoWallet,
    cc: &ClientConfig,
    core: &str,
    ticker: &str,
) -> Result<(String, String, String, u32)> {
    add_tokens(cc, alice, 4).await?;
    let asset = alice.issue_token(ticker, ticker, 0, SUPPLY).await?;
    for _ in 0..120 {
        bitcoin_core::generatetoaddress(1, core)?;
        alice.claim().await?;
        let found = colored_carriers(cc, ALICE, &asset).await?;
        if let Some((sid, b)) = found.into_iter().find(|(_, b)| b.is_colored()) {
            return Ok((asset, sid, b.f_txid.clone(), b.f_vout));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Err(anyhow!("alice never got a COLOURED carrier of {asset}"))
}

/// Pay `PAY` units of `asset` to bob and poll until he has booked exactly `want`.
async fn pay_and_book(
    alice: &UtexoWallet,
    bob: &UtexoWallet,
    asset: &str,
    want: u64,
    what: &str,
) -> Result<()> {
    let bob_addr = bob.get_utexo_address().await?;
    alice
        .transfer_tokens(asset, &bob_addr, PAY)
        .await
        .map_err(|e| anyhow!("{what}: the SENDER refused the coloured payment: {e:#}"))?;
    let mut booked = 0;
    for _ in 0..60 {
        bob.claim().await?;
        booked = token_balance(bob, asset).await?;
        if booked == want {
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    assert_eq!(
        booked, want,
        "{what}: bob must BOOK the {PAY}-unit payment of {asset} (wanted {want}, booked {booked})"
    );
    Ok(())
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

    // ===== (a) THE CONTROL: a fresh carrier accepts an honest coloured payment ====================
    //
    // Without this half, (b)'s admission could be a lane that accepts anything — which would be a
    // worse defect than the one this file used to catch, and invisible.
    let (asset_ok, carrier_ok, _, _) = colored_carrier(&alice, &cc, &core, "CHA").await?;
    println!("SDK88 - control carrier {carrier_ok} of {asset_ok} at tip {}", tip(&cc)?);
    pay_and_book(&alice, &bob, &asset_ok, PAY, "(a) CONTROL").await?;
    let control_child = colored_children_of(&cc, BOB, &asset_ok)
        .await?
        .into_iter()
        .next()
        .map(|(_, cb)| cb)
        .ok_or_else(|| anyhow!("(a) CONTROL: bob booked {PAY} units but adopted no coloured child of {asset_ok}"))?;
    assert!(control_child.parent_flat_backups.is_empty(), "a conveyed coloured child carries NO flat backup beside its ladder");
    println!("SDK88 - (a) CONTROL: bob booked {PAY} units on a fresh carrier and adopted a coloured child");

    // ===== (b) NO EPOCH: the same payment from a carrier aged past `initlock` ======================
    let initlock = mercuryrustlib::utils::info_config(&cc).await?.initlock;
    let (asset_aged, carrier_aged, f_txid, f_vout) = colored_carrier(&alice, &cc, &core, "CHB").await?;
    let born = tip(&cc)?;
    let target = born + initlock + PAST_EPOCH;
    println!(
        "SDK88 - aged carrier {carrier_aged} of {asset_aged} laddered at tip {born}; mining to {target} \
         (initlock {initlock} + {PAST_EPOCH})"
    );
    mine_to(&cc, &core, target).await?;
    let now = tip(&cc)?;
    assert!(now >= born + initlock, "the chain must be past the old epoch: tip {now}, born {born}, initlock {initlock}");
    // NOTHING MATURED. Under the old rule the carrier's flat backup was spendable from `initlock`
    // on, an RGB-unaware spend of F that burned the allocation. There is no such backup.
    assert!(
        !is_outpoint_spent(&cc, &f_txid, f_vout),
        "[Stage 3] the aged carrier's F was SPENT after {initlock}+ blocks — something on a coloured carrier matured \
         on its own, and an RGB-unaware spend of F burns the allocation"
    );
    // The carrier is still the SAME ladder: no calendar pass re-anchored, renewed or exited it for
    // having merely aged.
    let aged_bundle = mercuryrustlib::tesr::load(&cc, ALICE, &carrier_aged)
        .await?
        .ok_or_else(|| anyhow!("the aged carrier lost its ladder row"))?;
    assert!(aged_bundle.is_colored(), "the aged carrier is still a COLOURED ladder");
    assert_eq!(aged_bundle.f_txid, f_txid, "the aged carrier is still rooted at the same F — no calendar pass re-anchored it");

    // The SPLIT is the payee's protection here as it always was; what has changed is that the age
    // of the carrier is not an input to anything.
    let before_bal = token_balance(&bob, &asset_aged).await?;
    pay_and_book(&alice, &bob, &asset_aged, before_bal + PAY, "(b) NO EPOCH").await?;

    // ===== (c) AND NOTHING WAS LOST — the assertion that makes this the ALLOCATION property ========
    let after_bal = token_balance(&bob, &asset_aged).await?;
    assert_eq!(
        after_bal, before_bal + PAY,
        "[Stage 3] bob must hold EXACTLY the {PAY} units paid out of the aged carrier"
    );
    let (aged_coin, aged_child) = colored_children_of(&cc, BOB, &asset_aged)
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!(
            "[Stage 3] bob booked {PAY} units of {asset_aged} but ADOPTED no coloured child of it — a balance with no exit \
             material is an allocation he can never materialise"
        ))?;
    assert!(
        aged_child.parent_flat_backups.is_empty(),
        "[Stage 3] the aged child conveyed a flat backup beside its ladder — the calendar is back on the coloured lane"
    );
    assert!(
        aged_coin.locktime.is_none(),
        "[Stage 3] bob's coloured child coin carries locktime {:?}; nothing on this lane has an absolute calendar",
        aged_coin.locktime
    );
    let chain = mercuryrustlib::tesr::child_exit_chain_bound(&aged_child)
        .map_err(|e| anyhow!("the adopted child's declared timelocks must match its signatures: {e:#}"))?;
    // [D61] Five tiers — `T, X_m, SP, ext_child, state_child` — on EITHER lane; `child_exit_chain`
    // does not consult colour. The count is pinned because a shape change would move it, not as
    // evidence of a lane difference.
    assert_eq!(
        chain.len(),
        5,
        "a depth-1 child exits through five tiers (T, X_m, SP, ext_child, state_child) on EITHER lane"
    );
    assert!(
        !is_outpoint_spent(&cc, &f_txid, f_vout),
        "the coloured in-ladder split is off-chain: the aged carrier's F is still unspent after the payment"
    );
    println!(
        "SDK88 - (b)/(c) NO EPOCH: a child of a carrier {} blocks old ({initlock}+{PAST_EPOCH} past its deposit) was \
         ADMITTED and booked — {PAY} units, five signed tiers, no flat backup, no calendar; F unspent throughout",
        now - born
    );

    println!(
        "SDK88 PASS - [Stage 3] the coloured lane has no funding epoch: an honest payment on a fresh \
         carrier is accepted, the SAME payment from a carrier aged past initlock is accepted and \
         booked with a complete coloured exit, and the carrier's F is never spent by anything that \
         merely aged — so the payee never holds an asset they cannot exit, and the sender never \
         loses one to a matured flat backup."
    );
    Ok(())
}
