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

    println!(
        "SDK72 - ✓ PASS: [F3] a blind watcher reports Err + WatchtowerBlind + a retained fault \
         (never a manufactured empty carrier set); [F2] start_background() alone defended a hostile \
         trigger and raced {exit_value} sat to the owner's key"
    );
    Ok(())
}
