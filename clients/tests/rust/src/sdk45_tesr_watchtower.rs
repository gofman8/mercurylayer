//! E2E (SDK_E2E=45) — **TES-R keyless watchtower defends an offline owner** on the live SE + real
//! bitcoind.
//!
//! Answers the liveness question: an off-chain coin needs a defender awake when someone broadcasts
//! its trigger. A TES-R WatchBundle is KEYLESS — it holds only the pre-signed ladder (every tier
//! pays the owner) — so a delegated tower can drive the owner's exit without any key material.
//!
//! The ladder the tower defends is **the one the DEPOSIT signed** at the first mempool sighting of
//! `F` (`coin_status::check_deposit` under `LadderAtSight::Plain`: T → X_0 → S_0 on the regtest
//! schedule, E0 = 12 / D0 = 24, exiting to the coin's own backup address). There is no flat
//! absolute-locktime backup beside it, so the tower's material is COMPLETE from the mempool onwards
//! and nothing in it ever matures on its own — the tower's only clock starts when somebody spends `F`.
//!
//! Flow:
//!   1. Owner deposits F. While F is STILL UNCONFIRMED (coin `IN_MEMPOOL`) the persisted `tesr-<sid>`
//!      row already exists and a keyless tower loading ONLY that row runs an `Idle` pass over it: the
//!      coin is defensible from its first moment, not from its confirmation (the SDK's liveness
//!      allowlist L1 admits `IN_MEMPOOL` for exactly this reason — this is the material it relies
//!      on). Then F confirms; confirmation ADOPTS the ladder signed at sight. The owner goes offline.
//!   2. A tower loads ONLY the persisted bundle (no wallet, no coin, no keys). It is custody-free, it
//!      is `Idle` while F is unspent, and a second independent tower over the same bundle is
//!      harmlessly idempotent.
//!   3. A griefer (or a previous owner) broadcasts the trigger. The tower's `watch_pass` reacts
//!      per block, broadcasting the extension then the state as each relative-timelock matures.
//!   4. Funds land at the owner's backup address — defended entirely by a keyless tower, with ZERO
//!      new enclave co-signs (`num_sigs` stays at the deposit's 3).
//!
//! Run with SDK_E2E=45 (needs the regtest + Mercury lockbox stack, Core 28+).

use std::{env, fs, process::Command};

use anyhow::{anyhow, Result};
use mercuryrustlib::CoinStatus;

use crate::sdk40_tesr_consensus::{
    broadcast, confirm_deposit, deposit_coin_at_sight, is_outpoint_spent, mine, se_num_sigs, tx_exists,
    wait_for_address,
};

const NETWORK: &str = "regtest";

pub async fn execute() -> Result<()> {
    let _ = Command::new("rm").arg("wallet.db").arg("wallet.db-shm").arg("wallet.db-wal").output();
    let _ = fs::remove_dir_all("./rgb-data-sdk45");
    env::set_var("ML_NETWORK", "regtest");
    let cc = mercuryrustlib::client_config::load().await;
    let wallet = "sdk45_owner";
    let p = mercurylib::tesr::TesrParams::for_network(NETWORK);

    // ---- 1. Deposit. The ladder exists — and is defensible — while F is still in the mempool. ----
    let at_sight = deposit_coin_at_sight(&cc, wallet).await?;
    let sid = at_sight.statechain_id.clone().ok_or(anyhow!("no statechain_id"))?;
    let f_txid = at_sight.utxo_txid.clone().ok_or(anyhow!("no F txid"))?;
    let f_vout = at_sight.utxo_vout.ok_or(anyhow!("no F vout"))?;
    assert_eq!(at_sight.status, CoinStatus::IN_MEMPOOL, "F is unconfirmed at this point by construction");

    // A keyless tower that loads ONLY the persisted row — before any block has been mined over F —
    // holds the full T → X_0 → S_0 chain and can already SEE that F is unspent. A deposit that had
    // to wait for confirmation to get its ladder would leave this window undefended.
    let tower_at_sight =
        mercuryrustlib::tesr::load(&cc, wallet, &sid).await?.ok_or(anyhow!("no bundle while IN_MEMPOOL"))?;
    assert_eq!(tower_at_sight.exit_tiers().len(), 3, "the at-sight bundle is the complete 3-tier exit chain");
    assert_eq!(
        mercuryrustlib::tesr::watch_pass(&cc.electrum_client, &tower_at_sight),
        mercuryrustlib::tesr::WatchState::Idle,
        "a keyless tower over an UNCONFIRMED F must be able to read the chain and report Idle — the coin is defensible from its first mempool sighting"
    );
    let trigger_tx_at_sight = tower_at_sight.trigger.signed_tx.clone();
    println!("SDK45 - {sid} is IN_MEMPOOL and already laddered; a keyless tower over it is Idle before F has a single confirmation ✓");

    // Confirmation adopts that same ladder (asserted inside confirm_deposit: same trigger, num_sigs 3).
    let coin = confirm_deposit(&cc, wallet, &at_sight).await?;
    drop(tower_at_sight);
    drop(coin); // owner goes offline; keeps nothing in memory
    println!("SDK45 - F confirmed; the owner went offline holding nothing but the persisted `tesr-{sid}` row");

    // ---- 2. The tower loads ONLY the persisted bundle — keyless (no coin, no keys). ----
    let tower_bundle = mercuryrustlib::tesr::load(&cc, wallet, &sid).await?.ok_or(anyhow!("no bundle"))?;
    assert_eq!(
        tower_bundle.trigger.signed_tx, trigger_tx_at_sight,
        "the tower defends the ladder the deposit signed at sight — confirmation did not swap it"
    );
    let owner_exit = tower_bundle.owner_exit_address.clone();
    let csv_e = tower_bundle.current().extension.csv.ok_or(anyhow!("X_0 has no CSV"))?;
    let csv_d = tower_bundle.current().state.csv.ok_or(anyhow!("S_0 has no CSV"))?;
    assert_eq!(csv_e, p.ext_csv(0), "the deposit ladder's extension is at the schedule's E0");
    assert_eq!(csv_d, p.state_csv(0), "the deposit ladder's state is at the schedule's D0");
    let trigger_tx = tower_bundle.trigger.signed_tx.clone();
    let final_txid = tower_bundle.current().state.txid.clone();
    let final_value = tower_bundle.current().state.out_value;
    assert!(!is_outpoint_spent(&cc, &f_txid, f_vout), "F still UNSPENT — nothing to defend yet");

    // CUSTODY-FREE, literally: the serialized bundle a user hands a third-party tower carries the
    // pre-signed ladder and NOTHING that could move funds on its own. (Back-filled from the retired
    // sdk35 trust-boundaries test, which held the only assertion of this property.)
    let serialized = serde_json::to_string(&tower_bundle)?;
    for secret in ["mnemonic", "seckey", "secret", "private", "privkey", "xpriv"] {
        assert!(
            !serialized.to_lowercase().contains(secret),
            "the watch bundle must carry NO key material, but it contains {secret:?} — delegating to a tower would hand over custody"
        );
    }
    println!("SDK45 - the persisted bundle carries zero key material (custody-free delegation) ✓");

    // Before any trigger, the tower is idle — and STRICTLY idle (F4): the pass must report that it
    // READ the chain and found F unspent, not merely that it happened to broadcast nothing. An
    // empty result used to be the only signal, so a dead backend was indistinguishable from this.
    assert_eq!(
        mercuryrustlib::tesr::watch_pass(&cc.electrum_client, &tower_bundle),
        mercuryrustlib::tesr::WatchState::Idle,
        "idle: no trigger, no action — and the tower could SEE that"
    );
    // A SECOND, INDEPENDENT tower over the SAME bundle is harmlessly idempotent — towers need no
    // coordination, so a user may delegate to several. (Also back-filled from sdk35.)
    let tower_bundle_b = mercuryrustlib::tesr::load(&cc, wallet, &sid).await?.ok_or(anyhow!("no bundle"))?;
    assert_eq!(
        mercuryrustlib::tesr::watch_pass(&cc.electrum_client, &tower_bundle_b),
        mercuryrustlib::tesr::WatchState::Idle,
        "a second independent tower must also be idle (no trigger) — watch_pass is idempotent, so redundant towers never conflict"
    );
    println!("SDK45 - keyless tower loaded the bundle; idle while the coin sits un-broadcast (a 2nd independent tower is idempotent) ✓");

    // ---- 3. A griefer broadcasts the trigger; the tower reacts per block. ----
    let _ = broadcast(&cc, &trigger_tx)?;
    let _ = mine(1)?;
    assert!(is_outpoint_spent(&cc, &f_txid, f_vout), "trigger spent F — the clock is running");
    println!("SDK45 - hostile trigger broadcast; tower begins its defence (E0 = {csv_e}, D0 = {csv_d})");

    // The walk is E0 blocks to the extension plus D0 to the state; a generous bound above that
    // catches a tower that never gets there without hiding a stall behind a huge loop.
    let max_passes = csv_e as usize + csv_d as usize + 10;
    let mut total_acted = 0usize;
    for _ in 0..max_passes {
        let state = mercuryrustlib::tesr::watch_pass(&cc.electrum_client, &tower_bundle);
        assert!(!state.is_blind(), "the tower must be able to SEE throughout the defence: {state:?}");
        assert!(!state.is_idle(), "F is spent — a pass that reports Idle here is reading the chain wrong");
        total_acted += state.ids().len();
        if tx_exists(&cc, &final_txid) {
            break;
        }
        let _ = mine(1)?;
    }
    assert!(total_acted >= 2, "tower broadcast the extension and the state (>=2 tiers)");
    assert!(tx_exists(&cc, &final_txid), "the owner's exit state confirmed via the tower");
    assert!(wait_for_address(&cc, &owner_exit, final_value as u32).await.is_ok(), "owner funded by the keyless tower at the coin's backup address");
    // Keyless means keyless: the whole defence cost the enclave nothing. The count is still the
    // deposit's three tiers — no tx1 before them, no co-sign during the walk.
    assert_eq!(
        se_num_sigs(&cc, &sid).await?,
        3,
        "a keyless defence issues no enclave co-sign: num_sigs must still be exactly the deposit's T + X_0 + S_0"
    );
    println!("SDK45 - ✓ PASS: keyless watchtower defended an offline owner through the DEPOSIT's own ladder — {} sat exited to {owner_exit} with no owner keys and no new co-sign", final_value);
    Ok(())
}
