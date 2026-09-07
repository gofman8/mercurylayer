//! E2E (SDK_E2E=42) — **TES-R wallet-level lifecycle + persistence** on the live SE + real bitcoind.
//!
//! Proves a wallet can HOLD a V2 coin across sessions and manage its whole off-chain life through
//! library calls (not test glue): the ladder the DEPOSIT established, renewed off-chain, persisted
//! to the wallet DB, RELOADED as a fresh session would, and unilaterally exited from the reloaded
//! bundle.
//!
//! Flow:
//!   1. Deposit F. The deposit itself (`coin_status::check_deposit`, at first mempool sight) builds,
//!      blind-co-signs and persists T → X_0 → S_0 under `tesr-<id>` — the test LOADS that bundle
//!      (`tesr::load`) and establishes nothing: a second establishment would be a rival trigger
//!      over F and a census no receiver could balance.
//!   2. `tesr::renew_auto` co-signs a lower-CSV X_1 (E0 − δE) + fresh state OFF-CHAIN (zero bytes);
//!      persist again. The enclave count is now 5 = 3 live tiers + 2 superseded, with NO flat term.
//!   3. `tesr::load` re-reads the bundle from disk (simulating a fresh wallet load) — asserts it
//!      round-trips and reflects the renewal (m=1, the schedule's lower extension CSV), and that the
//!      coin still has no flat backup row and no absolute calendar (`locktime: None`).
//!   4. Exit purely from the RELOADED bundle: broadcast trigger → extension → state honoring each
//!      tier's CSV; funds land at the coin's own backup address (the ladder's `owner_exit_address`).
//!      Nothing aged while the coin sat un-broadcast.
//!
//! Run with SDK_E2E=42 (needs the regtest + Mercury lockbox stack, Core 28+).

use std::{env, fs, process::Command};

use anyhow::{anyhow, Result};

use crate::sdk40_tesr_consensus::{
    broadcast, deposit_coin, is_outpoint_spent, mine, se_num_sigs, tx_exists, wait_for_address,
};

const NETWORK: &str = "regtest";

pub async fn execute() -> Result<()> {
    let _ = Command::new("rm").arg("wallet.db").arg("wallet.db-shm").arg("wallet.db-wal").output();
    let _ = fs::remove_dir_all("./rgb-data-sdk42");
    env::set_var("ML_NETWORK", "regtest");
    let cc = mercuryrustlib::client_config::load().await;
    let wallet = "sdk42_alice";
    let p = mercurylib::tesr::TesrParams::for_network(NETWORK);

    // ---- 1. Deposit F; the deposit established + persisted the ladder at first sight. LOAD it. ----
    let mut coin = deposit_coin(&cc, wallet).await?;
    let sid = coin.statechain_id.clone().ok_or(anyhow!("no statechain_id"))?;
    let f_txid = coin.utxo_txid.clone().ok_or(anyhow!("no F txid"))?;
    let f_vout = coin.utxo_vout.ok_or(anyhow!("no F vout"))?;

    let mut bundle = mercuryrustlib::tesr::load(&cc, wallet, &sid)
        .await?
        .ok_or(anyhow!("the deposit did not persist a ladder for {sid}"))?;
    let owner_exit = bundle.owner_exit_address.clone();
    let initial_ext_csv = bundle.current().extension.csv.ok_or(anyhow!("X_0 has no CSV"))?;
    assert_eq!(bundle.m, 0, "a fresh deposit ladder has no renewals yet");
    assert_eq!(initial_ext_csv, p.ext_csv(0), "the deposit ladder's extension is at E0");
    println!("SDK42 - loaded the deposit's ladder for {sid}: T={} X_0={}(csv {initial_ext_csv}) -> {owner_exit}", bundle.trigger.txid, bundle.current().extension.txid);

    // ---- 2. Off-chain renewal (lower-CSV extension), persist the updated bundle. ----
    let _rollover_due = mercuryrustlib::tesr::renew_auto(&cc, &mut coin, &mut bundle).await?;
    mercuryrustlib::tesr::persist(&cc, wallet, &bundle).await?;
    let renewed_ext_txid = bundle.current().extension.txid.clone();
    let renewed_csv = bundle.current().extension.csv.ok_or(anyhow!("X_1 has no CSV"))?;
    assert!(renewed_csv < initial_ext_csv, "the renewed extension's CSV is strictly lower");
    let n = se_num_sigs(&cc, &sid).await?;
    assert_eq!(n, 5, "num_sigs after one renewal is 3 live tiers + 2 superseded — there is no flat term");
    mercuryrustlib::tesr::verify_bundle(&bundle, n, 0)
        .map_err(|e| anyhow!("the renewed bundle must pass the census with flat term 0: {e}"))?;
    println!("SDK42 - renewed off-chain (0 on-chain bytes): m={}, new X={renewed_ext_txid}(csv {renewed_csv}); census 5 == 3 + 2, flat term 0", bundle.m);

    // ---- 3. RELOAD from disk (as a fresh wallet session) and verify it round-trips. ----
    drop(bundle);
    let reloaded = mercuryrustlib::tesr::load(&cc, wallet, &sid)
        .await?
        .ok_or(anyhow!("bundle did not persist"))?;
    assert_eq!(reloaded.m, 1, "reloaded bundle reflects the renewal");
    assert_eq!(reloaded.current().extension.csv, Some(p.ext_csv(1)), "reloaded extension has the schedule's renewed CSV");
    assert_eq!(reloaded.current().extension.txid, renewed_ext_txid, "reloaded extension is the renewed one");
    assert_eq!(reloaded.statechain_id, sid, "bundle keyed to the coin");
    assert_eq!(reloaded.owner_exit_address, owner_exit, "the exit address survives the round-trip");
    // The persisted shape is ladder-only: no flat backup row was ever written for this coin, and the
    // wallet record carries no absolute calendar to age it.
    assert!(
        mercuryrustlib::sqlite_manager::try_get_backup_txs(&cc.pool, wallet, &sid).await?.is_none(),
        "a laddered coin has NO `<sid>` flat backup row on disk"
    );
    let on_disk = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, wallet)
        .await?
        .coins
        .into_iter()
        .find(|c| c.statechain_id.as_deref() == Some(&sid))
        .ok_or(anyhow!("coin vanished from the wallet"))?;
    assert!(on_disk.locktime.is_none(), "coin.locktime must be None for life, got {:?}", on_disk.locktime);
    println!("SDK42 - ✓ bundle reloaded from DB intact (m={}, extension csv {:?}); no flat row, no calendar", reloaded.m, reloaded.current().extension.csv);

    // Un-broadcast immunity: the coin sat with a full renewed ladder, F never touched.
    assert!(!is_outpoint_spent(&cc, &f_txid, f_vout), "F still UNSPENT before exit — nothing aged");

    // ---- 4. Unilateral exit purely from the RELOADED bundle (trigger → extension → state). ----
    let exit_state = reloaded.current().state.clone();
    let ext_csv = reloaded.current().extension.csv.unwrap();
    let state_csv = reloaded.current().state.csv.unwrap();
    let _ = broadcast(&cc, &reloaded.trigger.signed_tx)?;
    let _ = mine(ext_csv as u32); // trigger confirmed + ext_csv confs
    let _ = broadcast(&cc, &reloaded.current().extension.signed_tx)?;
    let _ = mine(state_csv as u32); // extension confirmed + state_csv confs
    let _ = broadcast(&cc, &exit_state.signed_tx)?;
    let _ = mine(1)?;
    assert!(tx_exists(&cc, &exit_state.txid), "state confirms");
    assert!(is_outpoint_spent(&cc, &f_txid, f_vout), "F consumed by the exit");
    assert!(wait_for_address(&cc, &owner_exit, exit_state.out_value as u32).await.is_ok(), "owner funded on exit");
    println!("SDK42 - ✓ PASS: the deposit-established V2 coin renewed off-chain, reloaded from disk, and exited ({} sat to the owner's backup address)", exit_state.out_value);
    Ok(())
}
