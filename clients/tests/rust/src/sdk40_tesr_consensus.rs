//! E2E (SDK_E2E=40) — **TES-R consensus core** on the live SE + real bitcoind.
//!
//! Validates the load-bearing claims of `docs/utexo/spec/PROTOCOL.md` against real Bitcoin consensus,
//! co-signed by the *unchanged* blind SE (proving the "enclave cryptographically unchanged" claim —
//! it blind-signs v3 + relative-timelock + P2A sighashes exactly as it signed anything else).
//!
//! The ladder under test is **the one the DEPOSIT establishes**: `coin_status::check_deposit`
//! (`LadderAtSight::Plain`, the `update_coins` default) co-signs T → X_0 → S_0 at the FIRST MEMPOOL
//! SIGHTING of the funding tx, before it confirms, on the regtest schedule (E0 = 12, D0 = 24). There
//! is no flat absolute-locktime backup: no `tx1` is signed at deposit and none exists beside the
//! ladder. This file's shared `deposit_coin` helper (used by sdk41-47/53-57/70) is where that shape is
//! MEASURED, on every deposit:
//!
//!   PART 0 — first-sight establishment (inside `deposit_coin_at_sight` / `confirm_deposit`):
//!     * one status pass while F is UNCONFIRMED books the coin `IN_MEMPOOL` and its `tesr-<sid>` row
//!       already exists; the SE's `num_sigs` is EXACTLY 3; the census balances with the flat term 0
//!       (`verify_bundle(b, 3, 0)`) and does NOT balance with the retired baseline of one deposit
//!       backup (`verify_bundle(b, 3, 1)` is refused); there is no `<sid>` flat backup row; the coin
//!       has `locktime: None`.
//!     * confirmation ADOPTS that ladder (same trigger txid, `num_sigs` still 3) — it never signs a
//!       second one.
//!
//!   PART 1 — un-broadcast immunity, CSV enforcement, full unilateral exit (coin A), through the
//!     deposit's own tiers:
//!     * Nothing broadcast; F stays unspent; nothing ages.
//!     * Broadcast T, confirm. Assert X_0 is REJECTED before E0 confirmations of T (BIP-68 not met).
//!     * Mine to E0; X_0 accepted. Assert S_0 REJECTED before D0 confirmations of X_0; mine; accepted.
//!     * The owner's funds land at the coin's backup address with NO operator cooperation.
//!
//!   PART 2 — cooperative de-trigger defeats a hostile trigger (coin B):
//!     * A griefer broadcasts the deposit's T'. The owner responds with a fresh no-timelock
//!       DE-TRIGGER spend of T'.out[0] — it confirms immediately, before X'_0 can ever mature. Assert
//!       X'_0 can NEVER confirm (its prevout is spent), even after E0 blocks.
//!
//!   PART 3 — off-chain renewal (Decker-Wattenhofer) through the production `renew_auto` (coin C):
//!     * X_1 at E0 − δE rivals X_0 over T.out[0]. Census after renewal: `num_sigs == 5 == 3 + 2
//!       superseded`, flat term 0. Once T confirms, X_1 matures FIRST; X_0 can never win the race.
//!
//! Run with SDK_E2E=40 (needs the regtest + Mercury lockbox stack; bitcoind must be Core 28+ for
//! v3/TRUC + P2A relay — the stack ships Core 30).

use std::{env, fs, process::Command, str::FromStr, thread, time::Duration};

use anyhow::{anyhow, Result};
use electrum_client::ElectrumApi;
use mercuryrustlib::{client_config::ClientConfig, Coin, CoinStatus};

use crate::{bitcoin_core, electrs};

const NETWORK: &str = "regtest";
const COIN_SAT: u32 = 100_000;

pub(crate) async fn wait_for_address(cc: &ClientConfig, address: &str, amount: u32) -> Result<()> {
    for _ in 0..60 {
        if electrs::check_address(cc, address, amount).await? {
            return Ok(());
        }
        thread::sleep(Duration::from_secs(1));
    }
    Err(anyhow!("address {address} not indexed in time"))
}

/// True iff `txid:vout` is no longer in the UTXO set (i.e. it has been spent / never existed unspent).
///
/// An electrum failure PANICS rather than returning a guess. Both callers assert in BOTH directions
/// ("still unspent" early, "finally spent" late), so any fixed fallback makes one of those directions
/// fail OPEN — silently passing a test whose chain state was never actually observed. (sdk17 used to
/// keep a private copy returning `true` here and sdk40 `false`, so the two disagreed and each leaked a
/// different fail-open; there is now one copy and it refuses to guess.)
pub(crate) fn is_outpoint_spent(cc: &ClientConfig, txid: &str, vout: u32) -> bool {
    use electrum_client::bitcoin::Txid;
    let raw = match cc.electrum_client.transaction_get_raw(&Txid::from_str(txid).unwrap()) {
        std::result::Result::Ok(r) => r,
        Err(e) => panic!("electrum could not fetch {txid} — cannot assert whether {txid}:{vout} is spent: {e}"),
    };
    let tx: electrum_client::bitcoin::Transaction =
        electrum_client::bitcoin::consensus::deserialize(&raw).unwrap();
    let spk = &tx.output[vout as usize].script_pubkey;
    let listed = cc.electrum_client.script_list_unspent(spk).unwrap_or_default();
    !listed.iter().any(|u| u.tx_hash.to_string() == txid && u.tx_pos as u32 == vout)
}

pub(crate) fn tx_exists(cc: &ClientConfig, txid: &str) -> bool {
    use electrum_client::bitcoin::Txid;
    cc.electrum_client.transaction_get_raw(&Txid::from_str(txid).unwrap()).is_ok()
}

pub(crate) fn broadcast(cc: &ClientConfig, tx_hex: &str) -> Result<String> {
    let raw = hex::decode(tx_hex)?;
    Ok(cc.electrum_client.transaction_broadcast_raw(&raw)?.to_string())
}

pub(crate) fn mine(n: u32) -> Result<()> {
    let core = bitcoin_core::getnewaddress()?;
    let _ = bitcoin_core::generatetoaddress(n, &core)?;
    thread::sleep(Duration::from_millis(500));
    Ok(())
}

/// The SE's cumulative co-sign counter for `sid` — the attested `num_sigs` that is the right-hand
/// side of every receiver census (`se_num_sigs == tiers + superseded`, flat term 0).
pub(crate) async fn se_num_sigs(cc: &ClientConfig, sid: &str) -> Result<u32> {
    Ok(mercuryrustlib::utils::get_statechain_info(sid, cc)
        .await?
        .ok_or_else(|| anyhow!("no /info/statechain record for {sid}"))?
        .num_sigs)
}

/// Create `wallet_name`, buy a token, hand out a deposit address for `COIN_SAT`, pay it, and wait
/// until electrs has the funding tx in its MEMPOOL index. Nothing is mined and no status pass has
/// run yet: the coin is still `INITIALISED` on disk.
async fn fund_fresh_deposit(cc: &ClientConfig, wallet_name: &str) -> Result<String> {
    let wallet = mercuryrustlib::wallet::create_wallet(wallet_name, cc).await?;
    mercuryrustlib::sqlite_manager::insert_wallet(&cc.pool, &wallet).await?;
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    let token_id = crate::utils::handle_token_response(cc, &token).await?;
    let sc_address =
        mercuryrustlib::deposit::get_deposit_bitcoin_address(cc, &wallet.name, &token_id, COIN_SAT)
            .await?;
    let _ = bitcoin_core::sendtoaddress(COIN_SAT, &sc_address)?;
    wait_for_address(cc, &sc_address, COIN_SAT).await?;
    Ok(sc_address)
}

/// The index-0 coin of `wallet_name` sitting at `sc_address`, re-read from the wallet DB.
async fn coin_at(cc: &ClientConfig, wallet_name: &str, sc_address: &str) -> Result<Coin> {
    mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, wallet_name)
        .await?
        .coins
        .iter()
        .find(|c| c.aggregated_address.as_deref() == Some(sc_address) && c.duplicate_index == 0)
        .cloned()
        .ok_or_else(|| anyhow!("coin not found for {sc_address}"))
}

/// **PART 0a — the deposit as FIRST SEEN.** Deposit a fresh coin of `COIN_SAT` to a new wallet and
/// return it while its funding tx is still UNCONFIRMED: status `IN_MEMPOOL`, and ALREADY LADDERED.
///
/// One `update_coins` pass (`LadderAtSight::Plain`) runs before any block is mined. That pass is
/// where the flat `tx1` used to be co-signed; under the rule it books the deposit and establishes
/// T → X_0 → S_0 in the same call. Every property of that shape is asserted here, so a regression to
/// "ladder only at CONFIRMED" (no row while IN_MEMPOOL), to "tx1 + ladder" (`num_sigs == 4`, a
/// `<sid>` row, a locktime), or to "no ladder at all" fails on the first deposit of every caller.
pub(crate) async fn deposit_coin_at_sight(cc: &ClientConfig, wallet_name: &str) -> Result<Coin> {
    let sc_address = fund_fresh_deposit(cc, wallet_name).await?;

    mercuryrustlib::coin_status::update_coins(cc, wallet_name).await?;

    let coin = coin_at(cc, wallet_name, &sc_address).await?;
    assert!(
        coin.status == CoinStatus::IN_MEMPOOL,
        "the funding tx is unconfirmed, so the first sighting must book the coin IN_MEMPOOL (got {:?})",
        coin.status
    );
    let sid = coin.statechain_id.clone().ok_or_else(|| anyhow!("booked coin has no statechain_id"))?;
    let f_txid = coin.utxo_txid.clone().ok_or_else(|| anyhow!("booked coin has no utxo_txid"))?;
    let f_vout = coin.utxo_vout.ok_or_else(|| anyhow!("booked coin has no utxo_vout"))?;
    assert!(
        coin.locktime.is_none(),
        "a laddered coin carries NO absolute calendar: coin.locktime must be None at sight, got {:?}",
        coin.locktime
    );

    // The ladder row exists WHILE THE COIN IS STILL IN_MEMPOOL.
    let ladder = mercuryrustlib::tesr::load(cc, wallet_name, &sid).await?.ok_or_else(|| {
        anyhow!(
            "{sid} was booked IN_MEMPOOL but has no `tesr-{sid}` row — a deposit with no ladder has \
             no exit material at all; the ladder must be established at first sight, not at confirmation"
        )
    })?;
    assert_eq!(ladder.f_txid, f_txid, "the ladder's trigger spends the coin's own funding outpoint");
    assert_eq!(ladder.f_vout, f_vout, "the ladder's trigger spends the coin's own funding outpoint");
    assert_eq!(ladder.exit_tiers().len(), 3, "a fresh deposit ladder is exactly T → X_0 → S_0");
    assert_eq!(
        ladder.owner_exit_address, coin.backup_address,
        "the deposit ladder exits to the coin's own backup address"
    );
    let p = mercurylib::tesr::TesrParams::for_network(NETWORK);
    assert_eq!(ladder.current().extension.csv, Some(p.ext_csv(0)), "X_0 is at the schedule's E0");
    assert_eq!(ladder.current().state.csv, Some(p.state_csv(0)), "S_0 is at the schedule's D0");

    // The enclave count is exactly the three tiers: no tx1 was signed at first sight.
    let n = se_num_sigs(cc, &sid).await?;
    assert_eq!(n, 3, "the SE's num_sigs after deposit must be exactly T + X_0 + S_0 (no tx1)");
    mercuryrustlib::tesr::verify_bundle(&ladder, n, 0)
        .map_err(|e| anyhow!("the fresh deposit ladder must pass the census with flat term 0: {e}"))?;
    assert!(
        mercuryrustlib::tesr::verify_bundle(&ladder, n, 1).is_err(),
        "the retired baseline (one deposit backup) must NOT balance: 3 != 1 + 3"
    );
    // No flat backup row: `create_tx1` is gone and nothing writes a `<sid>` row for a deposit.
    assert!(
        mercuryrustlib::sqlite_manager::try_get_backup_txs(&cc.pool, wallet_name, &sid).await?.is_none(),
        "a deposit must have NO `<sid>` flat backup row"
    );
    println!(
        "SDK40::deposit_coin - {sid} booked IN_MEMPOOL with its ladder already signed: T={} X_0={}(csv {:?}) S_0={}(csv {:?}), num_sigs=3, no flat backup",
        ladder.trigger.txid,
        ladder.current().extension.txid,
        ladder.current().extension.csv,
        ladder.current().state.txid,
        ladder.current().state.csv
    );
    Ok(coin)
}

/// **PART 0b — confirmation ADOPTS the ladder signed at sight.** Mine the deposit to `CONFIRMED`,
/// run another status pass, and return the re-read coin. The `tesr-<sid>` row is the SAME ladder
/// (same trigger txid) and the enclave count is still 3 — a second establishment at confirmation
/// would be three more irreversible co-signs and a census no receiver could balance.
pub(crate) async fn confirm_deposit(cc: &ClientConfig, wallet_name: &str, coin: &Coin) -> Result<Coin> {
    let sid = coin.statechain_id.clone().ok_or_else(|| anyhow!("coin has no statechain_id"))?;
    let sc_address = coin.aggregated_address.clone().ok_or_else(|| anyhow!("coin has no aggregated_address"))?;
    let at_sight = mercuryrustlib::tesr::load(cc, wallet_name, &sid)
        .await?
        .ok_or_else(|| anyhow!("{sid} has no ladder before confirmation"))?;

    mine(cc.confirmation_target.max(1))?;
    mercuryrustlib::coin_status::update_coins(cc, wallet_name).await?;

    let coin = coin_at(cc, wallet_name, &sc_address).await?;
    assert!(coin.status == CoinStatus::CONFIRMED, "funding coin F must confirm (got {:?})", coin.status);
    let confirmed = mercuryrustlib::tesr::load(cc, wallet_name, &sid)
        .await?
        .ok_or_else(|| anyhow!("{sid} lost its ladder row at confirmation"))?;
    assert_eq!(
        confirmed.trigger.txid, at_sight.trigger.txid,
        "confirmation must ADOPT the ladder signed at sight, not sign a second one"
    );
    assert_eq!(
        se_num_sigs(cc, &sid).await?,
        3,
        "the enclave count must still be exactly 3 after confirmation (no re-establishment, no tx1)"
    );
    assert!(coin.locktime.is_none(), "coin.locktime stays None for life");
    Ok(coin)
}

/// Deposit a fresh statechain coin of `COIN_SAT` to a new wallet; return its CONFIRMED `Coin` (F),
/// **already laddered by the deposit** (T → X_0 → S_0 on the network schedule, exiting to the
/// coin's backup address). Callers LOAD that ladder with `tesr::load`; establishing another would
/// co-sign a second rival trigger over F and break the census.
pub(crate) async fn deposit_coin(cc: &ClientConfig, wallet_name: &str) -> Result<Coin> {
    let coin = deposit_coin_at_sight(cc, wallet_name).await?;
    confirm_deposit(cc, wallet_name, &coin).await
}

/// The DEFERRED fixture: the deposit is booked under `LadderAtSight::Defer`, so it reaches
/// `CONFIRMED` with NO exit material and NO co-sign on the enclave (`num_sigs == 0`) — the shape the
/// SDK's `claim()` sees for one instant before its own establish pass. For a test that must build a
/// ladder ITSELF (hand-picked CSVs, adversarial shapes) over a coin the deposit has not laddered.
#[allow(dead_code)]
pub(crate) async fn deposit_coin_unladdered(cc: &ClientConfig, wallet_name: &str) -> Result<Coin> {
    use mercuryrustlib::coin_status::{update_coins_ex, LadderAtSight};
    let sc_address = fund_fresh_deposit(cc, wallet_name).await?;

    update_coins_ex(cc, wallet_name, LadderAtSight::Defer).await?;
    let coin = coin_at(cc, wallet_name, &sc_address).await?;
    assert!(coin.status == CoinStatus::IN_MEMPOOL, "Defer still BOOKS the deposit at sight (got {:?})", coin.status);
    let sid = coin.statechain_id.clone().ok_or_else(|| anyhow!("booked coin has no statechain_id"))?;
    assert!(
        mercuryrustlib::tesr::load(cc, wallet_name, &sid).await?.is_none(),
        "LadderAtSight::Defer must establish nothing"
    );
    assert_eq!(se_num_sigs(cc, &sid).await?, 0, "no tx1 and no ladder: the enclave has co-signed nothing for {sid}");

    mine(cc.confirmation_target.max(1))?;
    update_coins_ex(cc, wallet_name, LadderAtSight::Defer).await?;
    let coin = coin_at(cc, wallet_name, &sc_address).await?;
    assert!(coin.status == CoinStatus::CONFIRMED, "funding coin F must confirm (got {:?})", coin.status);
    assert!(
        mercuryrustlib::tesr::load(cc, wallet_name, &sid).await?.is_none(),
        "confirmation under Defer establishes nothing either"
    );
    assert!(
        mercuryrustlib::sqlite_manager::try_get_backup_txs(&cc.pool, wallet_name, &sid).await?.is_none(),
        "no `<sid>` flat backup row under Defer"
    );
    assert!(coin.locktime.is_none(), "coin.locktime is None on every coin");
    Ok(coin)
}

pub async fn execute() -> Result<()> {
    let _ = Command::new("rm").arg("wallet.db").arg("wallet.db-shm").arg("wallet.db-wal").output();
    let _ = fs::remove_dir_all("./rgb-data-sdk40");
    env::set_var("ML_NETWORK", "regtest");
    let cc = mercuryrustlib::client_config::load().await;
    let p = mercurylib::tesr::TesrParams::for_network(NETWORK);

    // ============================ PART 1: coin A — full lifecycle ============================
    // PART 0 (first-sight establishment + adoption at confirmation) is asserted inside deposit_coin.
    let coin_a = deposit_coin(&cc, "sdk40_alice").await?;
    let sid_a = coin_a.statechain_id.clone().ok_or(anyhow!("no statechain_id"))?;
    let f_txid = coin_a.utxo_txid.clone().ok_or(anyhow!("no F txid"))?;
    let f_vout = coin_a.utxo_vout.ok_or(anyhow!("no F vout"))?;
    let f_value = coin_a.amount.ok_or(anyhow!("no F value"))? as u64;
    let la = mercuryrustlib::tesr::load(&cc, "sdk40_alice", &sid_a).await?.ok_or(anyhow!("coin A has no ladder"))?;
    let t = la.trigger.clone();
    let x = la.current().extension.clone();
    let s = la.current().state.clone();
    let csv_e = x.csv.ok_or(anyhow!("X_0 has no CSV"))?;
    let csv_d = s.csv.ok_or(anyhow!("S_0 has no CSV"))?;
    assert_eq!(csv_e, p.ext_csv(0), "the deposit ladder's extension is at E0");
    assert_eq!(csv_d, p.state_csv(0), "the deposit ladder's state is at D0");
    println!("SDK40 - coin A: F = {f_txid}:{f_vout} ({f_value} sat); deposit ladder T={} X_0={}(csv {csv_e}) S_0={}(csv {csv_d}) -> {}", t.txid, x.txid, s.txid, la.owner_exit_address);

    // Un-broadcast immunity: nothing is on chain, F is still unspent, no clock is running.
    assert!(!is_outpoint_spent(&cc, &f_txid, f_vout), "un-broadcast: F still UNSPENT, nothing ages");

    // Broadcast the trigger and confirm it (1 conf). The clock starts now.
    let _ = broadcast(&cc, &t.signed_tx)?;
    mine(1)?;
    assert!(tx_exists(&cc, &t.txid), "T must confirm");
    assert!(is_outpoint_spent(&cc, &f_txid, f_vout), "F consumed by T");
    println!("SDK40 - T broadcast + confirmed (1 conf); clock started");

    // CSV enforcement #1: X_0 needs E0 confirmations of T. With 1, it MUST be rejected.
    assert!(broadcast(&cc, &x.signed_tx).is_err(), "X_0 must be REJECTED before {csv_e} confs of T (BIP-68)");
    println!("SDK40 - ✓ X_0 rejected at 1 conf (relative-timelock enforced)");
    let _ = mine((csv_e - 1) as u32); // T now has E0 confs
    let _ = broadcast(&cc, &x.signed_tx)?;
    mine(1)?;
    assert!(tx_exists(&cc, &x.txid), "X_0 must confirm once T has {csv_e} confs");
    println!("SDK40 - ✓ X_0 accepted after {csv_e} confs of T");

    // CSV enforcement #2: S_0 needs D0 confirmations of X_0.
    assert!(broadcast(&cc, &s.signed_tx).is_err(), "S_0 must be REJECTED before {csv_d} confs of X_0");
    println!("SDK40 - ✓ S_0 rejected at 1 conf");
    let _ = mine((csv_d - 1) as u32);
    let _ = broadcast(&cc, &s.signed_tx)?;
    mine(1)?;
    assert!(tx_exists(&cc, &s.txid), "S_0 must confirm once X_0 has {csv_d} confs");
    assert!(is_outpoint_spent(&cc, &x.txid, x.payload_vout), "X_0's payload output consumed by S_0");
    // Funds landed at the owner's own backup address — a complete unilateral exit, no operator help.
    assert!(
        wait_for_address(&cc, &la.owner_exit_address, s.out_value as u32).await.is_ok(),
        "owner exit funds landed at the coin's backup address"
    );
    println!("SDK40 - ✓ PART 1: full unilateral exit T→X_0→S_0 through the DEPOSIT ladder reached the owner ({} sat) with no SE cooperation", s.out_value);

    // ============================ PART 2: coin B — cooperative de-trigger ============================
    let mut coin_b = deposit_coin(&cc, "sdk40_bob").await?;
    let sid_b = coin_b.statechain_id.clone().ok_or(anyhow!("no statechain_id B"))?;
    let lb = mercuryrustlib::tesr::load(&cc, "sdk40_bob", &sid_b).await?.ok_or(anyhow!("coin B has no ladder"))?;
    let tb = lb.trigger.clone();
    let xb = lb.current().extension.clone();
    let csv_eb = xb.csv.ok_or(anyhow!("X'_0 has no CSV"))?;
    let detrigger_dest = bitcoin_core::getnewaddress()?;

    // A griefer broadcasts the deposit's own trigger to force a cost.
    let _ = broadcast(&cc, &tb.signed_tx)?;
    mine(1)?;
    println!("SDK40 - coin B: hostile trigger T' broadcast");

    // Owner responds with a fresh no-timelock DE-TRIGGER spend of T'.out[0] — the coin's own aggregate
    // key (co-signed fresh by the SE), confirming immediately, before X'_0 can ever mature.
    let de = mercurylib::tesr::build_detrigger(&tb.txid, tb.out_value, &detrigger_dest, NETWORK, lb.fee_rate)?;
    let de_signed = mercuryrustlib::tesr::cosign_tier(&cc, &mut coin_b, de.tx_hex.clone(), tb.out_value, NETWORK).await?;
    let _ = broadcast(&cc, &de_signed)?;
    mine(1)?;
    assert!(tx_exists(&cc, &de.txid), "de-trigger confirms immediately (no CSV wait)");
    assert!(is_outpoint_spent(&cc, &tb.txid, tb.payload_vout), "T'.out0 consumed by the de-trigger");
    // The de-trigger is one more counted co-sign on top of the three tiers — and nothing else is.
    assert_eq!(se_num_sigs(&cc, &sid_b).await?, 4, "3 tiers + 1 de-trigger: every co-sign is counted, and there is no tx1 term");
    println!("SDK40 - ✓ de-trigger confirmed, spending T'.out0 with no timelock");

    // The stale extension can now NEVER confirm — its prevout is gone — even after the full E0 window.
    let _ = mine(csv_eb as u32);
    assert!(broadcast(&cc, &xb.signed_tx).is_err(), "stale X'_0 can never confirm: its prevout T'.out0 is spent");
    println!("SDK40 - ✓ PART 2: stale ladder defeated — griefing collapses to a priced nuisance");

    // ============================ PART 3: off-chain renewal (Decker-Wattenhofer) ============================
    // Renewal = co-sign a NEW extension X_1 with a LOWER extension-CSV than X_0, both spending
    // T.out[0]. It replaces horizontally (no new depth, no on-chain byte). Consensus-level claim:
    // once T confirms, the renewed X_1 (lower CSV) matures FIRST, so the pre-renewal X_0 can never win
    // the race for T.out[0] — old state dies at the consensus level, not by an enclave promise. This
    // is what lets an active coin renew unlimited times off-chain (footprint scales with activity).
    let mut coin_c = deposit_coin(&cc, "sdk40_carol").await?;
    let sid_c = coin_c.statechain_id.clone().ok_or(anyhow!("no statechain_id C"))?;
    let mut lc = mercuryrustlib::tesr::load(&cc, "sdk40_carol", &sid_c).await?.ok_or(anyhow!("coin C has no ladder"))?;
    let tc = lc.trigger.clone();
    let x0 = lc.current().extension.clone();
    let csv_e0 = x0.csv.ok_or(anyhow!("X_0 has no CSV"))?;

    // The production renewal: X_1 at E0 − δE (strictly lower) + a fresh state, zero on-chain bytes.
    let _rollover_due = mercuryrustlib::tesr::renew_auto(&cc, &mut coin_c, &mut lc).await?;
    mercuryrustlib::tesr::persist(&cc, "sdk40_carol", &lc).await?;
    let x1 = lc.current().extension.clone();
    let csv_e1 = x1.csv.ok_or(anyhow!("X_1 has no CSV"))?;
    assert_eq!(lc.m, 1, "one renewal");
    assert_eq!(csv_e1, p.ext_csv(1), "X_1 follows the schedule");
    assert!(csv_e1 < csv_e0, "the renewed extension's CSV is strictly lower");
    // Census after renewal: 3 live tiers + 2 superseded (X_0, S_0), flat term 0.
    let n_c = se_num_sigs(&cc, &sid_c).await?;
    assert_eq!(n_c, 5, "num_sigs after one renewal is 3 + 2 (no tx1 term)");
    mercuryrustlib::tesr::verify_bundle(&lc, n_c, 0)
        .map_err(|e| anyhow!("the renewed bundle must pass the census with flat term 0: {e}"))?;

    let _ = broadcast(&cc, &tc.signed_tx)?;
    let _ = mine(csv_e1 as u32); // T now has E1 confs: X_1 final, X_0 (needs E0 > E1) not yet
    // At the renewal moment the OLD extension cannot confirm, but the RENEWED one can:
    assert!(broadcast(&cc, &x0.signed_tx).is_err(), "pre-renewal X_0 not final at {csv_e1} confs");
    let _ = broadcast(&cc, &x1.signed_tx)?;
    let _ = mine(1)?;
    assert!(tx_exists(&cc, &x1.txid), "renewed X_1 (lower CSV) confirms first");
    assert!(is_outpoint_spent(&cc, &tc.txid, tc.payload_vout), "T.out0 consumed by the renewed extension");
    // The superseded extension is now permanently dead, even past its own CSV window.
    let _ = mine(csv_e0 as u32);
    assert!(broadcast(&cc, &x0.signed_tx).is_err(), "superseded X_0 can never confirm (prevout spent)");
    println!("SDK40 - ✓ PART 3: off-chain renewal — X_1 (csv {csv_e1}) superseded X_0 (csv {csv_e0}) at consensus level (zero on-chain bytes)");

    println!("SDK40 - PASS: TES-R consensus core validated on live SE + real bitcoind, over the ladder the deposit signs at first sight");
    Ok(())
}
