//! E2E (SDK_E2E=86): **INV-27 on a RECEIVED coin — there is NO calendar clock. Two hops and 300
//! idle blocks change nothing about when, or whether, the coin can exit.**
//!
//! This test used to measure the OTHER answer. While a laddered coin retained its flat backup
//! chain it carried two clocks: the CSV side, which never ticked, and an absolute-nLockTime rung
//! that every whole-coin hop decremented by `interval` and every mined block brought closer. The
//! test read that rung off the receiver's flat backup rows and asserted `L1 == L0 - interval`
//! (INV-5) and `left_after + 300 <= left_before`. That chain no longer exists: the ladder
//! `(T, X_0, S_0)` is co-signed at the FIRST MEMPOOL SIGHTING of `F` in place of the flat `tx1`
//! (`coin_status::check_deposit`, `UtexoWallet::claim`), a transfer conveys
//! `backup_transactions: []`, the receiver refuses any conveyed flat backup by name
//! (`verify_flat_backup_lane`) and books `coin.locktime = None`. So the honest form of INV-27 is
//! the unconditional one, and this test is its evidence on the one shape the deposit-only tests
//! (`sdk30` (a), `sdk48`) structurally cannot reach: a coin that has been RECEIVED — twice.
//!
//! # What this test measures
//!
//! One coin, three owners, and the ABSENCE of a calendar read at every step:
//!
//! * **k=0 — laddered at sight, and no calendar.** The ladder exists in the pass that BOOKS the
//!   deposit, before `F` confirms; the enclave count is exactly 3 (T + X + S, no `tx1`); there are
//!   ZERO flat backup rows and `coin.locktime == None`.
//! * **A — the CSV clock does not tick on a RECEIVED coin.** Bob's coin idles 300 blocks. Its
//!   ladder fingerprint (txids + relative CSVs) is byte-identical afterwards, `F` is unspent and the
//!   balance is whole. This is `sdk30` (a)'s property, re-measured after a hop.
//! * **B — there is no second clock to tick.** Same coin, same 300 blocks: still zero flat rows,
//!   still `locktime == None`; `estimate_exit_cost` reports no deadline, no blindness and
//!   `wait_blocks: 0` before AND after, pricing the same tier vbytes; and `deadline_safety_due` at
//!   a margin a hundred times the regtest `initlock` re-anchors nothing, severs nothing and leaves
//!   `F` unspent. The census `se_num_sigs == tiers + superseded` balances with the flat term 0.
//! * **C — a second hop changes nothing either.** Carol receives the same coin: zero flat rows,
//!   `locktime == None`, and the enclave count is exactly `tiers + superseded` — one superseded
//!   state per hop, no flat term. Hops cost signatures, not calendar.
//!
//! Each of these FAILS if the old shape comes back: a co-signed `tx1` or a per-hop backup makes
//! `num_sigs` exceed `tiers + superseded` (the census with flat term 0 refuses), a stored flat row
//! is counted, a booked locktime is read, and a matured rung is what the deadline pass would act on.
//!
//! Run: SDK_E2E=86 ML_NETWORK=regtest cargo run   (regtest stack up)

use std::time::Duration;

use anyhow::{anyhow, Result};
use electrum_client::ElectrumApi;
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};
use mercurylib::wallet::{Coin, CoinStatus};
use mercuryrustlib::client_config::ClientConfig;

use crate::bitcoin_core;
use crate::sdk40_tesr_consensus::is_outpoint_spent;

const ALICE: &str = "sdk86_alice";
const BOB: &str = "sdk86_bob";
const CAROL: &str = "sdk86_carol";
const DEPOSIT: u64 = 60_000;
/// Blocks of idling. Large enough that "the tip moved" is not a rounding artefact.
const IDLE: u32 = 300;
/// A margin far beyond any height a regtest coin could ever have been "due" at under the old
/// calendar (`initlock` is 1 000). The deadline pass takes the margin as a PARAMETER, so this drives
/// exactly the branch a real deadline would have driven — and it must select nothing.
const HUGE_MARGIN: u32 = 100_000;

async fn wallet(name: &str) -> Result<UtexoWallet> {
    let (w, _) = UtexoWallet::initialize(SdkConfig::regtest(name), None).await?;
    Ok(w)
}

fn tip(cc: &ClientConfig) -> Result<u32> {
    Ok(cc.electrum_client.block_headers_subscribe_raw()?.height as u32)
}

/// **THE FLAT ROWS, counted.** `get_backup_txs` is `fetch_one`, so an absent row is an error there;
/// `try_get_backup_txs` separates "no row" (`None`) from a failed read (`Err`). Both `None` and an
/// empty vector count as zero — either way there is no absolute-locktime spend of `F` on disk.
async fn flat_rows(cc: &ClientConfig, wallet_name: &str, sid: &str) -> Result<usize> {
    Ok(mercuryrustlib::sqlite_manager::try_get_backup_txs(&cc.pool, wallet_name, sid)
        .await?
        .map_or(0, |rows| rows.len()))
}

/// The enclave's attested co-signature count for `sid`.
async fn num_sigs(cc: &ClientConfig, sid: &str) -> Result<u32> {
    Ok(mercuryrustlib::utils::get_statechain_info(sid, cc)
        .await?
        .ok_or_else(|| anyhow!("no statechain_info for {sid}"))?
        .num_sigs)
}

/// **THE CSV CLOCK.** The ladder's shape: every tier's txid and its RELATIVE lock. This is the
/// quantity INV-27 is about, and the one that must not move.
fn ladder_fingerprint(b: &mercuryrustlib::tesr::TesrBundle) -> Vec<(String, Option<u16>)> {
    b.exit_tiers().iter().map(|t| (t.txid.clone(), t.csv)).collect()
}

async fn coin_by_sid(cc: &ClientConfig, name: &str, sid: &str) -> Result<Coin> {
    mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, name)
        .await?
        .coins
        .into_iter()
        .find(|c| c.statechain_id.as_deref() == Some(sid) && c.duplicate_index == 0)
        .ok_or_else(|| anyhow!("{name} has no coin {sid}"))
}

async fn confirmed_coin(cc: &ClientConfig, name: &str, sats: u64) -> Result<Coin> {
    mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, name)
        .await?
        .coins
        .iter()
        .find(|c| {
            c.status == CoinStatus::CONFIRMED
                && c.duplicate_index == 0
                && c.amount == Some(sats as u32)
        })
        .cloned()
        .ok_or_else(|| anyhow!("{name} has no confirmed {sats}-sat coin"))
}

/// **THE ABSENT CALENDAR, read every way a calendar could be read.** Zero flat rows; `locktime`
/// `None`; an exit estimate with no deadline, no blindness and no wait, priced on the ladder's own
/// tiers; and the census balancing with the flat term 0 against the LIVE enclave count. Returns the
/// estimate's total vbytes so a caller can assert before/after equality.
async fn assert_no_calendar(
    w: &UtexoWallet,
    cc: &ClientConfig,
    name: &str,
    sid: &str,
    hops: u32,
    when: &str,
) -> Result<u64> {
    let rows = flat_rows(cc, name, sid).await?;
    assert_eq!(
        rows, 0,
        "{when}: k={hops} — a laddered coin must hold ZERO flat backup rows in {name}'s wallet, \
         got {rows}. A flat rung is an absolute-locktime spend of F, i.e. a calendar."
    );
    let coin = coin_by_sid(cc, name, sid).await?;
    assert_eq!(
        coin.locktime, None,
        "{when}: k={hops} — coin.locktime must be None for life; a laddered coin has no absolute \
         calendar and a receiver books None"
    );
    let est = w.estimate_exit_cost(sid).await?;
    assert_eq!(est.branch_txs, 0, "{when}: k={hops} — an on-chain-rooted coin has no exit branch");
    assert_eq!(
        est.exit_deadline_block, None,
        "{when}: k={hops} — no exit-race deadline exists for a laddered coin"
    );
    assert!(
        !est.deadline_is_unknown(),
        "{when}: k={hops} — the absent deadline is SAFE, not blind: {:?}",
        est.exit_deadline_blind
    );
    assert_eq!(
        est.wait_blocks, 0,
        "{when}: k={hops} — nothing on a laddered coin matures on its own, so there is nothing to \
         wait for (a non-zero wait is a flat backup's locktime minus the tip)"
    );
    assert!(
        est.backup_vbytes > 0,
        "{when}: k={hops} — the estimate must price the ladder's own tiers, not a missing backup"
    );
    let bundle = mercuryrustlib::tesr::load(cc, name, sid)
        .await?
        .ok_or_else(|| anyhow!("{when}: k={hops} — {name}'s coin {sid} has no ladder"))?;
    let sigs = num_sigs(cc, sid).await?;
    let tiers = bundle.exit_tiers().len();
    let superseded = bundle.superseded_states.len() + bundle.superseded_extensions.len();
    assert_eq!(
        sigs as usize,
        tiers + superseded,
        "{when}: k={hops} — the enclave count must be EXACTLY tiers + superseded ({tiers} + \
         {superseded}) with NO flat term: every extra co-sign is a flat rung (a deposit tx1 or a \
         per-hop backup) that the census with flat term 0 cannot account for"
    );
    mercuryrustlib::tesr::verify_bundle(&bundle, sigs, 0).map_err(|e| {
        anyhow!(
            "{when}: k={hops} — the census `num_sigs == tiers + superseded` must balance with the \
             FLAT TERM 0 (num_sigs={sigs}, tiers={tiers}, superseded={superseded}): {e}"
        )
    })?;
    println!(
        "SDK86 - {when}: k={hops} — 0 flat rows, locktime=None, deadline=None (not blind), \
         wait_blocks=0, num_sigs={sigs} = {tiers} tiers + {superseded} superseded, flat term 0"
    );
    Ok(est.total_vbytes)
}

/// Deposit one coin and prove it is laddered — and calendar-free — from the moment it is SIGHTED,
/// before `F` confirms. Then confirm it and return it.
async fn deposit_laddered(w: &UtexoWallet, cc: &ClientConfig, name: &str) -> Result<Coin> {
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    let t = crate::utils::handle_token_response(cc, &token).await?;
    w.add_prepaid_token(&t).await;
    let addr = w.get_deposit_address(DEPOSIT).await?;
    bitcoin_core::sendtoaddress(u32::try_from(DEPOSIT)?, &addr)?;

    // ---- at sight: the pass that BOOKS the deposit also ladders it. No block has been mined. ----
    let mut sighted: Option<Coin> = None;
    for _ in 0..30 {
        w.claim().await?;
        let coins = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, name).await?.coins;
        if let Some(c) = coins.iter().find(|c| {
            c.aggregated_address.as_deref() == Some(addr.as_str())
                && c.duplicate_index == 0
                && c.status != CoinStatus::INITIALISED
        }) {
            sighted = Some(c.clone());
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let sighted = sighted.ok_or_else(|| {
        anyhow!("{name}'s un-mined deposit to {addr} was never sighted: claim() left it INITIALISED")
    })?;
    let sid = sighted.statechain_id.clone().ok_or_else(|| anyhow!("sighted coin has no id"))?;
    assert!(
        mercuryrustlib::tesr::load(cc, name, &sid).await?.is_some(),
        "{name}'s deposit {sid} was booked as {:?} but has NO ladder: the ladder must be established \
         at first sight, in the same pass, before any confirmation",
        sighted.status
    );
    assert_eq!(
        num_sigs(cc, &sid).await?,
        3,
        "at sight the enclave count must be exactly the 3 tiers: a 4 means a flat tx1 was co-signed \
         at deposit"
    );
    assert_eq!(flat_rows(cc, name, &sid).await?, 0, "at sight: zero flat backup rows");
    assert_eq!(sighted.locktime, None, "at sight: coin.locktime must be None");
    println!(
        "SDK86 - {name}'s deposit {sid} sighted as {:?}: laddered in the same pass, num_sigs=3, 0 \
         flat rows, locktime=None",
        sighted.status
    );

    // ---- confirm; nothing about the exit material may change. ---------------------------------
    let core = bitcoin_core::getnewaddress()?;
    bitcoin_core::generatetoaddress(3, &core)?;
    for i in 0..60 {
        w.claim().await?;
        if w.get_balance().await?.available_sats >= DEPOSIT {
            break;
        }
        if i == 59 {
            return Err(anyhow!("{name}'s deposit did not confirm"));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let coin = confirmed_coin(cc, name, DEPOSIT).await?;
    assert_eq!(coin.statechain_id.as_deref(), Some(sid.as_str()), "confirmation keeps the id");
    Ok(coin)
}

/// Hand the WHOLE coin over (exact amount ⇒ the whole-coin `S'` handover, no split) and let the
/// recipient claim it. Returns the recipient's statechain id, which is the same id: a whole-coin hop
/// keeps the coin and supersedes its state — it conveys `backup_transactions: []`.
async fn hand_over(
    from: &UtexoWallet,
    to: &UtexoWallet,
    cc: &ClientConfig,
    to_name: &str,
) -> Result<String> {
    let slot = mercuryrustlib::transfer_receiver::new_transfer_address(cc, to_name).await?;
    from.transfer(&slot, DEPOSIT).await?;
    for i in 0..60 {
        to.claim().await?;
        if to.get_balance().await?.available_sats >= DEPOSIT {
            break;
        }
        if i == 59 {
            return Err(anyhow!("{to_name} never claimed the coin"));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    confirmed_coin(cc, to_name, DEPOSIT)
        .await?
        .statechain_id
        .ok_or_else(|| anyhow!("{to_name}'s claimed coin has no statechain id"))
}

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    for d in ["./rgb-data-sdk86_alice", "./rgb-data-sdk86_bob", "./rgb-data-sdk86_carol"] {
        let _ = std::fs::remove_dir_all(d);
    }
    let alice = wallet(ALICE).await?;
    let bob = wallet(BOB).await?;
    let carol = wallet(CAROL).await?;
    let cc = alice.client_config().clone();
    let core = bitcoin_core::getnewaddress()?;

    // The coordinator's `initlock` is the OLD calendar's epoch length. It is read only to show that
    // the margin below dwarfs it: under the old shape every coin in this test would have been "due".
    let initlock = mercuryrustlib::utils::info_config(&cc).await?.initlock;
    assert!(
        HUGE_MARGIN > initlock + IDLE,
        "test hygiene: the deadline margin ({HUGE_MARGIN}) must exceed initlock + IDLE \
         ({initlock} + {IDLE}), or 'the pass selected nothing' would prove nothing"
    );
    println!("SDK86 - coordinator initlock={initlock}; deadline margin under test={HUGE_MARGIN}");

    // ===== SETUP: one coin, k = 0 — laddered at sight, no calendar =================================
    let coin0 = deposit_laddered(&alice, &cc, ALICE).await?;
    let sid = coin0.statechain_id.clone().ok_or_else(|| anyhow!("no id"))?;
    let f_txid = coin0.utxo_txid.clone().ok_or_else(|| anyhow!("no funding txid"))?;
    let f_vout = coin0.utxo_vout.ok_or_else(|| anyhow!("no funding vout"))?;
    let vb0 = assert_no_calendar(&alice, &cc, ALICE, &sid, 0, "k=0 confirmed").await?;

    // ===== HOP 1: alice -> bob ====================================================================
    let sid_bob = hand_over(&alice, &bob, &cc, BOB).await?;
    assert_eq!(sid_bob, sid, "a whole-coin hop keeps the same statechain id");
    let vb1 = assert_no_calendar(&bob, &cc, BOB, &sid, 1, "k=1 received").await?;
    assert_eq!(
        vb1, vb0,
        "a hop supersedes a state; it does not add exit material (same tier vbytes before and after)"
    );

    // ===== A: THE CSV CLOCK DOES NOT TICK, on a RECEIVED coin ======================================
    //
    // This is `sdk30` (a)'s property, measured on the shape it could not reach. If the tiers
    // themselves aged, the invariant would be false in the direction that MATTERS (the exit would
    // become unbuildable), rather than merely mis-scoped.
    let bundle_before = mercuryrustlib::tesr::load(&cc, BOB, &sid)
        .await?
        .ok_or_else(|| anyhow!("bob's received coin has no ladder"))?;
    let fp_before = ladder_fingerprint(&bundle_before);
    assert!(
        !is_outpoint_spent(&cc, &f_txid, f_vout),
        "a resting laddered coin publishes NOTHING — F must be unspent"
    );
    let tip_before = tip(&cc)?;

    bitcoin_core::generatetoaddress(IDLE, &core)?;
    let mut tip_after = tip(&cc)?;
    for _ in 0..40 {
        if tip_after >= tip_before + IDLE {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
        tip_after = tip(&cc)?;
    }
    assert!(
        tip_after >= tip_before + IDLE,
        "{IDLE} blocks must have been mined (tip {tip_before} -> {tip_after})"
    );
    bob.claim().await?;

    let bundle_after = mercuryrustlib::tesr::load(&cc, BOB, &sid)
        .await?
        .ok_or_else(|| anyhow!("the received coin lost its ladder while idle"))?;
    assert_eq!(
        ladder_fingerprint(&bundle_after),
        fp_before,
        "A: the CSV clock does not tick — after {IDLE} blocks a RECEIVED coin's exit chain (txids + \
         relative locks) must be byte-identical"
    );
    assert!(
        !is_outpoint_spent(&cc, &f_txid, f_vout),
        "A: nothing was broadcast while idle — F is still unspent, so the ladder cost 0 vB of rent"
    );
    assert_eq!(
        bob.get_balance().await?.available_sats,
        DEPOSIT,
        "A: the idle received coin is still whole"
    );
    println!(
        "SDK86 - A: +{IDLE} blocks (tip {tip_before} -> {tip_after}) and the RECEIVED coin's ladder \
         is UNCHANGED, F unspent — the CSV clock genuinely does not tick"
    );

    // ===== B: THERE IS NO SECOND CLOCK ============================================================
    //
    // Same coin, same moment, every calendar reader: none of them has anything to read, and the
    // exit estimate is the same number it was before the idle. Then the pass that USED to act on the
    // calendar — at a margin under which every coin would have been due — must select nothing.
    let vb1_after = assert_no_calendar(&bob, &cc, BOB, &sid, 1, "k=1 after the idle").await?;
    assert_eq!(
        vb1_after, vb1,
        "B: the exit estimate must be the SAME number of vbytes before and after {IDLE} idle blocks \
         — nothing matured, nothing was added"
    );
    let (re_anchored, severed) = bob.deadline_safety_due(HUGE_MARGIN).await.map_err(|e| {
        anyhow!(
            "B: the deadline pass must not go blind, and must not report an UNDEFENDED coin: a \
             laddered coin is never near a floor it does not have: {e:#}"
        )
    })?;
    assert!(
        re_anchored.is_empty(),
        "B: at margin {HUGE_MARGIN} the pass RE-ANCHORED {re_anchored:?} — it found a calendar floor \
         on a coin that has none"
    );
    assert!(
        severed.is_empty(),
        "B: at margin {HUGE_MARGIN} the pass SEVERED {severed:?} — it found a calendar deadline on a \
         coin that has none, and converted a resting coin into a walking exit"
    );
    assert!(
        !is_outpoint_spent(&cc, &f_txid, f_vout),
        "B: the deadline pass must leave F unspent"
    );
    assert_eq!(
        ladder_fingerprint(
            &mercuryrustlib::tesr::load(&cc, BOB, &sid)
                .await?
                .ok_or_else(|| anyhow!("bob's ladder vanished during the deadline pass"))?
        ),
        fp_before,
        "B: the deadline pass must leave the ladder byte-identical"
    );
    println!(
        "SDK86 - B: the SAME idle coin has no calendar to lose: deadline_safety_due({HUGE_MARGIN}) \
         re-anchored nothing, severed nothing, F unspent, ladder unchanged"
    );

    // ===== C: A SECOND HOP, STILL NO CALENDAR =====================================================
    let sid_carol = hand_over(&bob, &carol, &cc, CAROL).await?;
    assert_eq!(sid_carol, sid, "a whole-coin hop keeps the same statechain id");
    let vb2 = assert_no_calendar(&carol, &cc, CAROL, &sid, 2, "k=2 received").await?;
    assert_eq!(vb2, vb0, "C: two hops later the exit material is still the same three tiers");
    let bundle_carol = mercuryrustlib::tesr::load(&cc, CAROL, &sid)
        .await?
        .ok_or_else(|| anyhow!("carol's received coin has no ladder"))?;
    let superseded = bundle_carol.superseded_states.len();
    assert_eq!(
        superseded, 2,
        "C: two whole-coin hops supersede exactly two states — that, and nothing else, is what a \
         hop costs"
    );
    assert!(
        !is_outpoint_spent(&cc, &f_txid, f_vout),
        "C: after two hops and {IDLE} idle blocks F is still unspent"
    );
    assert_eq!(
        carol.get_balance().await?.available_sats,
        DEPOSIT,
        "C: the twice-received coin is whole"
    );
    println!(
        "SDK86 - C: k=2 — {superseded} superseded states, 0 flat rows, locktime=None. Hops cost \
         signatures, not calendar."
    );

    println!(
        "SDK86 PASS - INV-27 holds UNCONDITIONALLY on a received coin: across two hops and {IDLE} \
         idle blocks there is no flat backup row, no locktime, no deadline and no wait; the enclave \
         count is tiers + superseded with the flat term 0; the deadline pass at margin \
         {HUGE_MARGIN} selects nothing; the ladder is byte-identical and F unspent."
    );
    Ok(())
}
