//! E2E (SDK_E2E=86): **[D36 / Stage 3] INV-27's REAL scope — a received coin DOES have a calendar
//! deadline, and each hop moves it closer.**
//!
//! INV-27 is published as *"idle coins never age"*, with `sdk30` (a) as its evidence. `sdk30` (a)
//! idles a **k=0 deposit** and asserts that after 300 blocks its exit chain is byte-identical and
//! `F` is unspent. Both facts are true. Neither is INV-27.
//!
//! What `sdk30` (a) actually witnesses is that the **CSV side** does not age: a BIP-112 relative
//! lock does not tick until its parent confirms, and no tier is on chain, so nothing matures. That
//! half is real and this test re-confirms it. But a laddered coin also carries a **flat backup
//! chain** with ABSOLUTE locktimes, retained under TES-R, and [D36] recorded that this — not the CSV
//! hop budget — is what actually sets the maintenance cadence. A coin that has never been received
//! is precisely the shape in which that second clock is furthest away and least visible, so a
//! deposit-only test structurally cannot witness the case where INV-27, as written, is false.
//!
//! # What this test measures
//!
//! One coin, three owners, and both clocks read at every step:
//!
//! * **A — the CSV clock does not tick.** Bob's RECEIVED laddered coin is idled over 300 blocks. Its
//!   ladder fingerprint (txids + relative CSVs) is byte-identical afterwards and `F` is still
//!   unspent. This is `sdk30` (a)'s property, re-measured on the shape it was missing.
//!
//! * **B — the CALENDAR clock does tick, and mining moves it.** The same coin's flat backup carries
//!   an absolute locktime `L`. `L` is finite, and after 300 blocks the tip is 300 blocks nearer to
//!   it. The coin has a deadline; it simply is not the one INV-27 talks about.
//!
//! * **C — and each HOP moves it too, by `interval`.** `L` decrements by exactly `interval` per
//!   whole-coin hop (INV-5), so a coin received twice has `2·interval` less calendar than the
//!   deposit it came from. On regtest `interval` is 10 and `initlock` is 1000; on mainnet 100 and
//!   10 000. That is the "100 blocks per whole-coin hop" [D36]/T-4 recorded as unpublished and
//!   invisible to every client — measured here rather than asserted.
//!
//! The three together are the honest form of the invariant: **idle coins never age on the CSV side;
//! the flat ladder is what expires, and it is consumed by hops as well as by blocks.**
//!
//! Run: SDK_E2E=86 ML_NETWORK=regtest cargo run   (regtest stack up)

use std::time::Duration;

use anyhow::{anyhow, Result};
use electrum_client::ElectrumApi;
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};
use mercurylib::wallet::CoinStatus;

use crate::bitcoin_core;
use crate::sdk40_tesr_consensus::is_outpoint_spent;

const ALICE: &str = "sdk86_alice";
const BOB: &str = "sdk86_bob";
const CAROL: &str = "sdk86_carol";
const DEPOSIT: u64 = 60_000;
/// Blocks of idling. Large enough that "the tip moved" is not a rounding artefact, and far short of
/// the regtest `initlock` (1000) so the coin is never actually at risk during the run.
const IDLE: u32 = 300;

async fn wallet(name: &str) -> Result<UtexoWallet> {
    let (w, _) = UtexoWallet::initialize(SdkConfig::regtest(name), None).await?;
    Ok(w)
}

fn tip(cc: &mercuryrustlib::client_config::ClientConfig) -> Result<u32> {
    Ok(cc.electrum_client.block_headers_subscribe_raw()?.height as u32)
}

/// **THE CALENDAR CLOCK.** The height at which this coin's own flat backup may spend `F`: the LOWEST
/// locktime in its backup chain, which is the CURRENT owner's rung (INV-5). This is the quantity
/// INV-27 claims does not exist.
async fn calendar_deadline(
    cc: &mercuryrustlib::client_config::ClientConfig,
    wallet_name: &str,
    sid: &str,
) -> Result<u32> {
    let backups = mercuryrustlib::sqlite_manager::get_backup_txs(&cc.pool, wallet_name, sid).await?;
    backups
        .iter()
        .map(|b| mercurylib::utils::get_blockheight(b).map_err(|e| anyhow!("{e:?}")))
        .collect::<Result<Vec<u32>>>()?
        .into_iter()
        .min()
        .ok_or_else(|| anyhow!("coin {sid} has no flat backup — it has no calendar clock to read"))
}

/// **THE CSV CLOCK.** The ladder's shape: every tier's txid and its RELATIVE lock. This is the
/// quantity INV-27 is actually about, and the one that must not move.
fn ladder_fingerprint(b: &mercuryrustlib::tesr::TesrBundle) -> Vec<(String, Option<u16>)> {
    b.exit_tiers().iter().map(|t| (t.txid.clone(), t.csv)).collect()
}

async fn confirmed_coin(
    cc: &mercuryrustlib::client_config::ClientConfig,
    name: &str,
    sats: u64,
) -> Result<mercurylib::wallet::Coin> {
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

/// Deposit + ladder one coin, returning it.
async fn deposit_laddered(
    w: &UtexoWallet,
    cc: &mercuryrustlib::client_config::ClientConfig,
    name: &str,
) -> Result<mercurylib::wallet::Coin> {
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    let t = crate::utils::handle_token_response(cc, &token).await?;
    w.add_prepaid_token(&t).await;
    let addr = w.get_deposit_address(DEPOSIT).await?;
    bitcoin_core::sendtoaddress(u32::try_from(DEPOSIT)?, &addr)?;
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
    let sid = coin.statechain_id.clone().ok_or_else(|| anyhow!("deposit has no id"))?;
    assert!(
        mercuryrustlib::tesr::load(cc, name, &sid).await?.is_some(),
        "the deposit must be laddered — both clocks only exist together"
    );
    Ok(coin)
}

/// Hand the WHOLE coin over (exact amount ⇒ the whole-coin `S'` handover, no split) and let the
/// recipient claim it. Returns the recipient's statechain id, which is the same id: a whole-coin hop
/// keeps the coin and advances its flat rung.
async fn hand_over(
    from: &UtexoWallet,
    to: &UtexoWallet,
    cc: &mercuryrustlib::client_config::ClientConfig,
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
    let alice = wallet(ALICE).await?;
    let bob = wallet(BOB).await?;
    let carol = wallet(CAROL).await?;
    let cc = alice.client_config().clone();
    let core = bitcoin_core::getnewaddress()?;

    // The per-network flat-ladder parameters, read from the same place the client compiles in —
    // NOT hard-coded here, so this test states the RULE rather than the regtest numbers.
    let (initlock, interval) =
        mercurylib::tesr::TesrParams::flat_ladder_params(&cc.network.to_string()).ok_or_else(
            || anyhow!("no flat-ladder parameters for network {:?}", cc.network),
        )?;
    println!("SDK86 - flat ladder for {:?}: initlock={initlock} interval={interval}", cc.network);

    // ===== SETUP: one coin, k = 0 ================================================================
    let coin0 = deposit_laddered(&alice, &cc, ALICE).await?;
    let sid = coin0.statechain_id.clone().ok_or_else(|| anyhow!("no id"))?;
    let f_txid = coin0.utxo_txid.clone().ok_or_else(|| anyhow!("no funding txid"))?;
    let f_vout = coin0.utxo_vout.ok_or_else(|| anyhow!("no funding vout"))?;
    let deadline_k0 = calendar_deadline(&cc, ALICE, &sid).await?;
    let tip_at_deposit = tip(&cc)?;
    println!(
        "SDK86 - k=0 (deposit): coin {sid}, calendar deadline L0={deadline_k0}, tip={tip_at_deposit} \
         => {} blocks of calendar",
        deadline_k0.saturating_sub(tip_at_deposit)
    );

    // ===== C (first half): THE HOP CONSUMES A RUNG ===============================================
    let sid_bob = hand_over(&alice, &bob, &cc, BOB).await?;
    assert_eq!(sid_bob, sid, "a whole-coin hop keeps the same statechain id");
    let deadline_k1 = calendar_deadline(&cc, BOB, &sid).await?;
    assert_eq!(
        deadline_k1,
        deadline_k0 - interval,
        "INV-5: one whole-coin hop must decrement the flat locktime by EXACTLY interval \
         ({interval}). L0={deadline_k0} -> L1={deadline_k1}"
    );
    println!(
        "SDK86 - k=1 (received): L1={deadline_k1} — the hop alone cost {interval} blocks of \
         calendar, with no block mined"
    );

    // ===== A: THE CSV CLOCK DOES NOT TICK, on a RECEIVED coin ====================================
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

    // ===== B: THE CALENDAR CLOCK DOES TICK =======================================================
    //
    // Same coin, same moment, the other clock. `L` did not move — it cannot, it is absolute — but
    // the tip moved 300 blocks toward it, which is what "having a deadline" means.
    let deadline_still = calendar_deadline(&cc, BOB, &sid).await?;
    assert_eq!(
        deadline_still, deadline_k1,
        "B: the flat locktime is ABSOLUTE — idling does not change the number, it changes how far \
         away it is"
    );
    let left_before = deadline_k1.saturating_sub(tip_before);
    let left_after = deadline_k1.saturating_sub(tip_after);
    assert!(
        left_after + IDLE <= left_before,
        "B: the coin must have LOST at least {IDLE} blocks of calendar while idle ({left_before} -> \
         {left_after}). If this holds constant, the coin really has no calendar deadline and INV-27 \
         is true as written — re-derive this test rather than deleting it."
    );
    assert!(
        left_after > 0,
        "B (test hygiene): the coin must still be inside its epoch at the end of the run — this \
         test measures a deadline approaching, not a coin expiring"
    );
    println!(
        "SDK86 - B: the SAME idle coin lost {} blocks of calendar ({left_before} -> {left_after} \
         remaining against L={deadline_k1}). INV-27 as written is FALSE of this coin; it is true \
         only of its CSV side.",
        left_before - left_after
    );

    // ===== C (second half): A SECOND HOP, A SECOND RUNG ==========================================
    let sid_carol = hand_over(&bob, &carol, &cc, CAROL).await?;
    assert_eq!(sid_carol, sid, "a whole-coin hop keeps the same statechain id");
    let deadline_k2 = calendar_deadline(&cc, CAROL, &sid).await?;
    assert_eq!(
        deadline_k2,
        deadline_k0 - 2 * interval,
        "C: k hops cost k*interval of calendar. L0={deadline_k0}, L2={deadline_k2}, interval={interval}"
    );
    // The headline number D36/T-4 says is unpublished and invisible: the whole flat ladder is
    // `initlock` blocks long and each hop spends `interval` of it, so the coin has a FINITE hop
    // budget that no client surfaces.
    let hops_available = initlock / interval;
    println!(
        "SDK86 - C: L0={deadline_k0} -> L1={deadline_k1} -> L2={deadline_k2}. Each whole-coin hop \
         spends {interval} blocks of an {initlock}-block ladder, so this shape affords {hops_available} \
         hops before the calendar is gone — consumed by HOPS as well as by blocks, and surfaced by \
         nothing."
    );

    println!(
        "SDK86 PASS - INV-27 holds on the CSV side and is FALSE as a statement about the coin: a \
         received coin has a finite calendar deadline that both mining and hopping move closer."
    );
    Ok(())
}
