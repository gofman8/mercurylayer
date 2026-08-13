//! E2E (SDK_E2E=45) — **TES-R keyless watchtower defends an offline owner** on the live SE + real
//! bitcoind.
//!
//! Answers the liveness question: an off-chain coin needs a defender awake when someone broadcasts
//! its trigger. A TES-R WatchBundle is KEYLESS — it holds only the pre-signed ladder (every tier
//! pays the owner) — so a delegated tower can drive the owner's exit without any key material.
//!
//! Flow:
//!   1. Owner deposits F, establishes + persists the ladder, then goes "offline".
//!   2. A tower loads ONLY the persisted bundle (no wallet, no coin, no keys).
//!   3. A griefer (or a previous owner) broadcasts the trigger. The tower's `watch_pass` reacts
//!      per block, broadcasting the extension then the state as each relative-timelock matures.
//!   4. Funds land at the owner — defended entirely by a keyless tower.
//!
//! Run with SDK_E2E=45 (needs the regtest + Mercury lockbox stack, Core 28+).

use std::{env, fs, process::Command};

use anyhow::{anyhow, Result};

use crate::bitcoin_core;
use crate::sdk40_tesr_consensus::{broadcast, deposit_coin, is_outpoint_spent, mine, tx_exists, wait_for_address};

const NETWORK: &str = "regtest";
/// **THIS FIXTURE'S rate, not the protocol's.** The shipped committed rate is 3.0 since [D44]
/// (`TesrParams::for_network`). This test builds its own tiers at 2.0 and checks them against 2.0,
/// which is self-consistent and fine — but anything that must agree with a ladder the SDK built
/// must read the rate from the preset instead (see `sdk58`/`sdk70`, which broke by mixing the two).
const FEE_RATE: f64 = 2.0;
const CSV_E: u16 = 3;
const CSV_D: u16 = 4;

pub async fn execute() -> Result<()> {
    let _ = Command::new("rm").arg("wallet.db").arg("wallet.db-shm").arg("wallet.db-wal").output();
    let _ = fs::remove_dir_all("./rgb-data-sdk45");
    env::set_var("ML_NETWORK", "regtest");
    let cc = mercuryrustlib::client_config::load().await;
    let wallet = "sdk45_owner";

    // ---- 1. Owner establishes + persists the ladder, then goes offline. ----
    let mut coin = deposit_coin(&cc, wallet).await?;
    let sid = coin.statechain_id.clone().ok_or(anyhow!("no statechain_id"))?;
    let f_txid = coin.utxo_txid.clone().ok_or(anyhow!("no F txid"))?;
    let f_vout = coin.utxo_vout.ok_or(anyhow!("no F vout"))?;
    let owner_exit = bitcoin_core::getnewaddress()?;
    let b = mercuryrustlib::tesr::establish(&cc, &mut coin, &owner_exit, CSV_E, CSV_D, FEE_RATE, NETWORK).await?;
    mercuryrustlib::tesr::persist(&cc, wallet, &b).await?;
    let trigger_tx = b.trigger.signed_tx.clone();
    let final_txid = b.current().state.txid.clone();
    let final_value = b.current().state.out_value;
    drop(b); // owner goes offline; keeps nothing in memory
    drop(coin);
    println!("SDK45 - owner established + persisted ladder, then went offline");

    // ---- 2. The tower loads ONLY the persisted bundle — keyless (no coin, no keys). ----
    let tower_bundle = mercuryrustlib::tesr::load(&cc, wallet, &sid).await?.ok_or(anyhow!("no bundle"))?;
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
    println!("SDK45 - hostile trigger broadcast; tower begins its defence");

    let mut total_acted = 0usize;
    for _ in 0..40 {
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
    assert!(wait_for_address(&cc, &owner_exit, final_value as u32).await.is_ok(), "owner funded by the keyless tower");
    println!("SDK45 - ✓ PASS: keyless watchtower defended an offline owner — {} sat exited with no owner keys", final_value);
    Ok(())
}
