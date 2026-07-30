//! E2E (SDK_E2E=72) — **the watchtower is never silently idle** (external review F2 + F3).
//!
//! Two independent defects, both about a deadline-critical loop that *looked* fine:
//!
//! * **[F3] "no carriers" was manufactured from a failure.** `auto_exit_due` enumerated this
//!   wallet's RGB token carriers with `unwrap_or_default()`. A storage / indexer / RGB-engine fault
//!   therefore yielded an EMPTY carrier set: every carrier failed `is_token_carrier`, the
//!   materialisation loop found nothing to protect, and the pass returned `Ok(vec![])` — the
//!   deadline-critical watcher looking idle while doing nothing. Same swallow in `get_balance`.
//!   PART A drives a REAL enumeration failure (the RGB data dir is a regular file, so the engine
//!   cannot open) and proves the wallet now says so: `Err`, a `WatchtowerBlind` event, and a
//!   RETAINED `WatchtowerFault` an app can poll — and that it clears itself once the fault is
//!   repaired, so this is not a one-way latch.
//!
//! * **[F2] the ladder defence was spawned by nothing.** `start_background` ran `claim()` and
//!   `auto_exit_due` but never `defend_ladders`, and every shipped wrapper exposes only background
//!   monitoring — so a hostile trigger on a laddered coin was raced by nobody unless the app
//!   scheduled the pass itself. PART B is sdk51's attack with the manual pass DELETED: an adversary
//!   broadcasts the trigger, the owner only ever calls `start_background()`, and the coin must still
//!   exit to the owner's own key.
//!
//! * **[C1] the deadline itself was still computed silently.** F3 was hardened at the SHELL (the
//!   carrier enumeration) but not at the load-bearing INPUT. `deposit_anchored_exit_deadline` was
//!   built entirely of `.ok()?`, so an unreachable SE config, an unreadable chain lookup or a
//!   missing deposit-history entry each produced the same `None` a flat coin produces — and `None`
//!   means "no deadline", so `auto_exit_due` SKIPPED the coin. The watcher could still conclude
//!   "nothing is due" from a total inability to tell. PART C drives a REAL uncomputable deadline (a
//!   branch whose root spends an outpoint that is not on-chain) and proves the coin is reported
//!   BLIND through the same `WatchtowerBlind` + retained-fault machinery — while a genuinely
//!   deadline-free flat coin (C1's control) stays quiet.
//!
//! Run: SDK_E2E=72 ML_NETWORK=regtest cargo run   (regtest + lockbox + RGB proxy up)

use std::time::Duration;

use anyhow::{anyhow, Result};
use mercury_utexo_sdk::{SdkConfig, UtexoWallet, WalletEvent, WatchtowerPass};
use mercuryrustlib::CoinStatus;

use crate::bitcoin_core;
use crate::sdk40_tesr_consensus::{broadcast, is_outpoint_spent, tx_exists, wait_for_address};

/// Drain everything currently queued on an event stream (lag tolerated — we only assert PRESENCE).
fn drain(rx: &mut tokio::sync::broadcast::Receiver<WalletEvent>) -> Vec<WalletEvent> {
    let mut out = Vec::new();
    loop {
        match rx.try_recv() {
            Ok(ev) => out.push(ev),
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => continue,
            Err(_) => break,
        }
    }
    out
}

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    let _ = std::fs::remove_dir_all("./rgb-data-sdk72_blind");
    let _ = std::fs::remove_file("./rgb-data-sdk72_blind");
    let _ = std::fs::remove_dir_all("./rgb-data-sdk72_alice");
    let _ = std::fs::remove_dir_all("./rgb-data-sdk72_carol");
    std::env::set_var("ML_NETWORK", "regtest");

    let cc = mercuryrustlib::client_config::load().await;

    // ================================================================================ PART A [F3]
    // A wallet whose RGB carrier enumeration CANNOT succeed. The fault is real, not mocked: the
    // configured `rgb_data_dir` is an ordinary FILE, so the engine's `create_dir_all` fails and
    // `unspendable_as_btc_outpoints()` returns Err — exactly the shape of the storage/indexer fault
    // the reviewer described, reproduced without touching the SDK's internals.
    std::fs::write("./rgb-data-sdk72_blind", b"not a directory")?;
    let (blind, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk72_blind"), None).await?;
    let mut rx = blind.subscribe();

    // A1. Nothing has run yet, so nothing is blind. (Guards against a fault map that is non-empty by
    //     construction, which would make every later assertion vacuous.)
    assert!(
        !blind.is_watchtower_blind().await,
        "A1: a wallet whose passes have not run yet must not claim to be blind"
    );

    // A2. `get_balance` fails CLOSED instead of reporting a carrier's sats as spendable BTC.
    let bal = blind.get_balance().await;
    assert!(
        bal.is_err(),
        "A2 [F3]: get_balance must FAIL when the carrier set is unknown — it used to \
         unwrap_or_default() and report carrier sats as spendable"
    );
    println!("SDK72 - A2 get_balance refused: {}", bal.unwrap_err());

    // A3. THE FINDING. The near-deadline pass must NOT return "nothing to do".
    let pass = blind.auto_exit_due(288).await;
    let err = match pass {
        Ok(acted) => {
            return Err(anyhow!(
                "A3 [F3] REGRESSION: auto_exit_due returned Ok({acted:?}) while it could not \
                 enumerate carriers — this is precisely the silent-idle watcher the review found"
            ))
        }
        Err(e) => e.to_string(),
    };
    assert!(
        err.contains("could not enumerate RGB token carriers"),
        "A3: the error must name the carrier enumeration, got: {err}"
    );
    println!("SDK72 - A3 auto_exit_due refused instead of reporting an empty carrier set");

    // A4. It is LOUD: an event was emitted for the pass that went blind.
    let evs = drain(&mut rx);
    let blind_ev = evs.iter().any(|e| {
        matches!(e, WalletEvent::WatchtowerBlind { pass, .. } if *pass == WatchtowerPass::AutoExit)
    });
    assert!(
        blind_ev,
        "A4 [F3]: a blind deadline-critical pass must emit WatchtowerBlind{{AutoExit}}; got {evs:?}"
    );

    // A5. ...and it is RETAINED, so an app that never subscribed (or subscribed late) can still tell
    //     "nothing to do" from "I could not see".
    let faults = blind.watchtower_faults().await;
    assert_eq!(faults.len(), 1, "A5: exactly one blind pass, got {faults:?}");
    assert_eq!(faults[0].pass, WatchtowerPass::AutoExit, "A5: the AutoExit pass is the blind one");
    assert_eq!(faults[0].consecutive_failures, 1, "A5: one failed pass so far");
    assert!(blind.is_watchtower_blind().await, "A5: the wallet reports itself blind");
    println!("SDK72 - A5 retained fault: {:?}", faults[0]);

    // A6. A second failing pass counts up rather than resetting — a persistent blindness is
    //     distinguishable from a one-off blip.
    assert!(blind.auto_exit_due(288).await.is_err(), "A6: still blind");
    let faults = blind.watchtower_faults().await;
    assert_eq!(faults[0].consecutive_failures, 2, "A6: consecutive failures accumulate");

    // A7. The fault is PER-PASS, not global: `defend_ladders` does not read RGB state, so it still
    //     runs and reports clean. (A blanket "wallet is broken" flag would hide which defence died.)
    let defended = blind.defend_ladders().await?;
    assert!(defended.is_empty(), "A7: no laddered coins in this wallet");
    let faults = blind.watchtower_faults().await;
    assert_eq!(faults.len(), 1, "A7: only AutoExit is blind, DefendLadders is healthy: {faults:?}");

    // A8. RECOVERY. Repair the storage fault; the next successful pass must CLEAR the retained state
    //     (fail-loud must not be a one-way latch that alerts forever after one blip).
    std::fs::remove_file("./rgb-data-sdk72_blind")?;
    std::fs::create_dir_all("./rgb-data-sdk72_blind")?;
    let acted = blind
        .auto_exit_due(288)
        .await
        .map_err(|e| anyhow!("A8: the pass should succeed once the RGB dir is real: {e}"))?;
    assert!(acted.is_empty(), "A8: no coins in this wallet, so nothing to exit");
    assert!(
        blind.watchtower_faults().await.is_empty(),
        "A8: a successful pass clears the retained fault"
    );
    assert!(!blind.is_watchtower_blind().await, "A8: the wallet is no longer blind");
    // And NOW an empty result is a real answer, not a manufactured one.
    let _ = blind.get_balance().await.map_err(|e| anyhow!("A8: get_balance should work now: {e}"))?;
    println!("SDK72 - ✓ PART A: blind ⇒ Err + WatchtowerBlind + retained fault; repaired ⇒ cleared");

    // ================================================================================ PART B [F2]
    // sdk51's attack with the manual `defend_ladders()` call REMOVED. The owner's only action is
    // `start_background()` — the default background pass. If the defence is not wired in there, the
    // ladder never advances and this phase times out.
    let (alice, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk72_alice"), None).await?;

    let amount = 100_000u32;
    let token = mercuryrustlib::deposit::get_token(&cc).await?;
    let t = crate::utils::handle_token_response(&cc, &token).await?;
    alice.add_prepaid_token(&t).await;
    let addr = alice.get_deposit_address(amount as u64).await?;
    bitcoin_core::sendtoaddress(amount, &addr)?;
    let core = bitcoin_core::getnewaddress()?;
    bitcoin_core::generatetoaddress(3, &core)?;
    let mut confirmed = false;
    for _ in 0..60 {
        alice.claim().await?;
        if alice.get_balance().await?.available_sats >= amount as u64 {
            confirmed = true;
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    assert!(confirmed, "B: deposit did not confirm");

    let coin = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk72_alice")
        .await?
        .coins
        .iter()
        .find(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0)
        .cloned()
        .ok_or(anyhow!("B: no confirmed coin"))?;
    let sid = coin.statechain_id.clone().ok_or(anyhow!("B: no statechain_id"))?;
    let f_txid = coin.utxo_txid.clone().ok_or(anyhow!("B: no F txid"))?;
    let f_vout = coin.utxo_vout.ok_or(anyhow!("B: no F vout"))?;
    let bundle = mercuryrustlib::tesr::load(&cc, "sdk72_alice", &sid)
        .await?
        .ok_or(anyhow!("B: claim() did not ladder the deposit"))?;
    let exit_addr = bundle.owner_exit_address.clone();
    let exit_value = bundle.current().state.out_value;

    // B1. The adversary triggers: F is spent, the CSV clock is running, the owner must race.
    assert!(!is_outpoint_spent(&cc, &f_txid, f_vout), "B1: F unspent while idle");
    broadcast(&cc, &bundle.trigger.signed_tx)?;
    bitcoin_core::generatetoaddress(1, &core)?;
    assert!(is_outpoint_spent(&cc, &f_txid, f_vout), "B1: the adversary's trigger spent F");
    println!("SDK72 - B1 adversary broadcast the trigger; from here the owner runs NOTHING by hand");

    // B2. The ONLY owner action in this phase. No defend_ladders() call appears below.
    let mut arx = alice.subscribe();
    let bg = alice.start_background();

    let mut saw_defended = false;
    let mut guard = 0;
    loop {
        guard += 1;
        assert!(
            guard < 40,
            "B2 [F2] REGRESSION: the background pass never advanced the ladder — defend_ladders is \
             not running from start_background()"
        );
        // Give the background loop (poll_interval_secs = 5) at least one pass at this tip.
        tokio::time::sleep(Duration::from_secs(7)).await;
        for ev in drain(&mut arx) {
            if let WalletEvent::LadderDefended { statechain_id, tiers_broadcast } = ev {
                saw_defended = true;
                println!(
                    "SDK72 - B2 background pass defended {statechain_id} ({tiers_broadcast} tier(s))"
                );
            }
        }
        // F4: `?` — a blind chain read is an error here, never a fabricated wait time.
        match mercuryrustlib::tesr::next_exit_tier(&cc.electrum_client, &bundle)? {
            None => break, // every tier is on-chain / in the mempool
            Some(csv) => {
                bitcoin_core::generatetoaddress(csv.max(1) as u32, &core)?;
            }
        }
    }
    bg.abort();
    bitcoin_core::generatetoaddress(2, &core)?; // confirm the final exit state

    // B3. The defence must have been OBSERVABLE, not merely effective — the wrappers forward this
    //     event, and an owner needs to know a contested exit is being fought on their behalf.
    assert!(
        saw_defended,
        "B3 [F2]: the background pass must emit LadderDefended so wrappers can forward it"
    );

    // B4. The funds landed at the OWNER's key, driven only by start_background().
    assert!(tx_exists(&cc, &bundle.current().state.txid), "B4: the owner's exit state confirmed");
    assert!(
        wait_for_address(&cc, &exit_addr, exit_value as u32).await.is_ok(),
        "B4: {exit_value} sat must land at the owner's own key despite a hostile trigger"
    );
    // B5. A pass that saw everything leaves no fault behind.
    assert!(
        !alice.is_watchtower_blind().await,
        "B5: the defence ran with full visibility, so nothing is retained: {:?}",
        alice.watchtower_faults().await
    );

    // ================================================================================ PART C [C1]
    // The load-bearing INPUT, not the shell. PART A proved the pass is loud when the CARRIER
    // ENUMERATION fails. This proves it is loud when the DEADLINE COMPUTATION fails — the step the
    // previous round left silent.
    //
    // `deposit_anchored_exit_deadline` was built entirely of `.ok()?`: an unreachable SE config, an
    // unreadable chain lookup, or a missing deposit-history entry each yielded `None` — the SAME
    // `None` a flat coin with genuinely no deadline yields. `auto_exit_due` then read that as
    // "nothing is due" and SKIPPED the coin, i.e. concluded the wallet was safe from a total
    // inability to tell. This is precisely the protection a received RGB token depends on.
    //
    // The fault is real, not mocked: the coin is given an exit BRANCH whose root spends a funding
    // outpoint that does not exist on-chain, so the chain lookup inside the deadline computation
    // genuinely fails. Nothing in the SDK is stubbed.
    let (carol, _) = UtexoWallet::initialize(SdkConfig::regtest("sdk72_carol"), None).await?;
    let mut crx = carol.subscribe();

    let token = mercuryrustlib::deposit::get_token(&cc).await?;
    let t = crate::utils::handle_token_response(&cc, &token).await?;
    carol.add_prepaid_token(&t).await;
    let caddr = carol.get_deposit_address(amount as u64).await?;
    bitcoin_core::sendtoaddress(amount, &caddr)?;
    bitcoin_core::generatetoaddress(3, &core)?;
    let mut cconfirmed = false;
    for _ in 0..60 {
        carol.claim().await?;
        if carol.get_balance().await?.available_sats >= amount as u64 {
            cconfirmed = true;
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    assert!(cconfirmed, "C: deposit did not confirm");
    let csid = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk72_carol")
        .await?
        .coins
        .iter()
        .find(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0)
        .and_then(|c| c.statechain_id.clone())
        .ok_or(anyhow!("C: no confirmed coin"))?;

    // C1. CONTROL — the "genuinely no deadline" shape. A flat on-chain coin has no exit branch, so
    //     no ancestor can race it. `None` here is a real answer and the pass must stay QUIET; if
    //     this half regressed into an alert, the fix would be worthless (every wallet would cry
    //     wolf and the signal would be ignored).
    let est = carol.estimate_exit_cost(&csid).await?;
    assert_eq!(est.branch_txs, 0, "C1: a fresh deposit is flat — no exit branch");
    assert!(est.exit_deadline_block.is_none(), "C1: a flat coin has no exit deadline");
    assert!(
        !est.deadline_is_unknown(),
        "C1: a flat coin's absent deadline is SAFE, not blind — it must not be reported as blindness"
    );
    let quiet = carol
        .auto_exit_due(288)
        .await
        .map_err(|e| anyhow!("C1: a wallet of flat coins is genuinely idle, not blind: {e}"))?;
    assert!(quiet.is_empty(), "C1: nothing is due on a fresh flat deposit");
    assert!(
        !carol.is_watchtower_blind().await,
        "C1: a verified-idle pass retains no fault: {:?}",
        carol.watchtower_faults().await
    );
    let _ = drain(&mut crx);
    println!("SDK72 - C1 control: a flat coin's absent deadline stays quiet (no false alarm)");

    // C2. THE FAULT. Give the coin an exit branch whose ROOT spends a funding outpoint that is not
    //     on-chain — the shape of every real cause (SE config unreachable, electrum down, deposit
    //     not yet in the address history): the coin definitely HAS a deadline and definitely CAN be
    //     raced, and the deadline definitely cannot be computed.
    {
        use electrum_client::bitcoin::{consensus, Transaction, Txid};
        use std::str::FromStr;
        let real = mercuryrustlib::sqlite_manager::get_backup_txs(&cc.pool, "sdk72_carol", &csid)
            .await?;
        let latest = real
            .iter()
            .max_by_key(|b| b.tx_n)
            .ok_or(anyhow!("C2: no backup tx to build a branch from"))?;
        let mut tx: Transaction = consensus::deserialize(&hex::decode(&latest.tx)?)?;
        // A valid, well-formed transaction (so vsize accounting still works and the failure is
        // ISOLATED to the chain lookup) that spends a funding outpoint nobody has ever seen.
        tx.input[0].previous_output.txid = Txid::from_str(
            "dead00000000000000000000000000000000000000000000000000000000beef",
        )?;
        let fake = mercurylib::wallet::BackupTx {
            tx_n: 1,
            tx: hex::encode(consensus::serialize(&tx)),
            client_public_nonce: latest.client_public_nonce.clone(),
            server_public_nonce: latest.server_public_nonce.clone(),
            client_public_key: latest.client_public_key.clone(),
            server_public_key: latest.server_public_key.clone(),
            blinding_factor: latest.blinding_factor.clone(),
            rgb_consignment: None,
            rgb_blinding: None,
        };
        mercuryrustlib::sqlite_manager::insert_or_update_backup_txs(
            &cc.pool,
            "sdk72_carol",
            &format!("branch-{csid}"),
            &vec![fake],
        )
        .await?;
    }

    // C3. The two shapes are INDISTINGUISHABLE by `exit_deadline_block` alone — that is the whole
    //     defect — so the estimate must carry the distinction explicitly.
    let est = carol.estimate_exit_cost(&csid).await?;
    assert_eq!(est.branch_txs, 1, "C3: the coin now has an exit branch, so a deadline exists");
    assert_eq!(
        est.exit_deadline_block, None,
        "C3: same Option value as the flat coin in C1 — proving the old encoding could not tell \
         'no deadline' from 'I could not compute one'"
    );
    assert!(
        est.deadline_is_unknown(),
        "C3 [C1] REGRESSION: an uncomputable deadline on a branch-bearing coin must be reported as \
         BLIND; it is reported as a plain absent deadline, which every consumer reads as 'safe'"
    );
    println!(
        "SDK72 - C3 uncomputable deadline surfaced: {}",
        est.exit_deadline_blind.clone().unwrap_or_default()
    );

    // C4. THE FINDING. The near-deadline pass must NOT conclude "nothing is due".
    let pass = carol.auto_exit_due(288).await;
    let cerr = match pass {
        Ok(acted) => {
            return Err(anyhow!(
                "C4 [C1] REGRESSION: auto_exit_due returned Ok({acted:?}) while it could not \
                 compute {csid}'s exit-race deadline — the watcher is reporting 'nothing is due' \
                 from an inability to tell, which is exactly what a received RGB token's clawback \
                 protection hangs on"
            ))
        }
        Err(e) => e.to_string(),
    };
    assert!(
        cerr.contains("BLIND") && cerr.contains(&csid),
        "C4: the error must name the blind coin, got: {cerr}"
    );
    println!("SDK72 - C4 auto_exit_due refused: {cerr}");

    // C5. It is LOUD and RETAINED — routed into the SAME WatchtowerBlind / WatchtowerFault
    //     machinery PART A exercises, not into a bespoke channel an app would have to learn about.
    let cevs = drain(&mut crx);
    assert!(
        cevs.iter().any(|e| matches!(
            e,
            WalletEvent::WatchtowerBlind { pass, .. } if *pass == WatchtowerPass::AutoExit
        )),
        "C5 [C1]: an uncomputable deadline must emit WatchtowerBlind{{AutoExit}}; got {cevs:?}"
    );
    let cfaults = carol.watchtower_faults().await;
    assert_eq!(cfaults.len(), 1, "C5: exactly one blind pass, got {cfaults:?}");
    assert_eq!(cfaults[0].pass, WatchtowerPass::AutoExit, "C5: AutoExit is the blind pass");
    assert!(carol.is_watchtower_blind().await, "C5: the wallet reports itself blind");
    println!("SDK72 - C5 retained fault: {:?}", cfaults[0]);

    // C6. The OTHER consumer of the same deadline fails closed too: a keyless watch bundle that
    //     silently omitted this coin would hand a watchtower a bundle that protects nothing, and
    //     the export would still report success.
    let bundle_out = carol.export_watch_bundle().await;
    assert!(
        bundle_out.is_err(),
        "C6 [C1]: export_watch_bundle must refuse to omit a coin whose deadline is unknown"
    );
    println!("SDK72 - C6 export_watch_bundle refused: {}", bundle_out.unwrap_err());

    // C7. RECOVERY — not a one-way latch. Restore the coin to its genuinely branch-free shape; the
    //     next pass must go quiet again and CLEAR the retained fault.
    mercuryrustlib::sqlite_manager::insert_or_update_backup_txs(
        &cc.pool,
        "sdk72_carol",
        &format!("branch-{csid}"),
        &Vec::new(),
    )
    .await?;
    let quiet = carol
        .auto_exit_due(288)
        .await
        .map_err(|e| anyhow!("C7: the pass must succeed once the coin is flat again: {e}"))?;
    assert!(quiet.is_empty(), "C7: nothing due");
    assert!(
        !carol.is_watchtower_blind().await,
        "C7: a pass that saw everything clears the retained fault: {:?}",
        carol.watchtower_faults().await
    );
    println!("SDK72 - ✓ PART C: uncomputable deadline ⇒ BLIND (never 'nothing due'); repaired ⇒ cleared");

    println!(
        "SDK72 - ✓ PASS: [F3] a blind watcher reports Err + WatchtowerBlind + a retained fault \
         (never a manufactured empty carrier set); [F2] start_background() alone defended a hostile \
         trigger and raced {exit_value} sat to the owner's key; [C1] an UNCOMPUTABLE exit-race \
         deadline is reported as BLIND while a genuinely absent one stays quiet"
    );
    Ok(())
}
