//! E2E (SDK_E2E=81): **[P0-3]** an in-ladder split survives a HARD PROCESS KILL taken in the exact
//! window that used to destroy the coin.
//!
//! `in_ladder_split` terminalizes the parent at the SE (`set_spend_budget(..., 1)`), obtains ONE
//! co-signature over the un-broadcast split state `SP`, and then co-signs two tiers per child in a
//! loop. Every one of those signatures used to live only in process memory: the function persisted
//! nothing, and the SDK persisted only on `Ok`. A failure anywhere after the terminalization returned
//! `Err` with the parent PERMANENTLY terminal — the SE will co-sign nothing more over it — and zero
//! bundles on disk. The signatures can never be regenerated, so the coin's entire cooperative,
//! off-chain future was gone: exit-only, forever, from one transient error.
//!
//! This test does exactly that crash. A CHILD process runs the payment with
//! `UTEXO_CRASH_POINT=after_inladder_sp_sign` and dies by `abort()` (SIGABRT — no unwinding, no
//! `Drop`, no flush) the instant `SP`'s co-signature is journalled, BEFORE either child ladder
//! exists. The parent then restarts the wallet over the same database and proves:
//!
//!   1. the parent really is terminal at the SE (the destructive window was real, not hypothetical);
//!   2. `recover_in_ladder_splits` replays the journal — completing both children's ladders, which
//!      the crash never reached — and puts the CHANGE child back on disk as an exitable claim;
//!   3. recovery does NOT re-send the payment (the piece is reported, not conveyed);
//!   4. the ORIGINAL payment still completes: the recovered piece is conveyed and Bob ADOPTS it,
//!      which means the census over the replayed material balances at the receiver.
//!
//! Run: SDK_E2E=81 ML_NETWORK=regtest cargo run   (regtest stack up)

use std::time::Duration;

use anyhow::{anyhow, Result};
use mercury_utexo_sdk::transfer::InLadderSplitOutcome;
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};

use crate::bitcoin_core;

const ALICE: &str = "sdk81_alice";
const BOB: &str = "sdk81_bob";
const DEPOSIT: u64 = 100_000;
const PAY: u64 = 30_000;

/// Env contract between the parent and the crashing child.
const PHASE: &str = "SDK81_PHASE";
const RECIPIENT: &str = "SDK81_RECIPIENT";
const PARENT_SID: &str = "SDK81_PARENT_SID";

async fn prepaid_token(cc: &mercuryrustlib::client_config::ClientConfig) -> Result<String> {
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    crate::utils::handle_token_response(cc, &token).await
}

async fn wallet(name: &str) -> Result<UtexoWallet> {
    let (w, _) = UtexoWallet::initialize(SdkConfig::regtest(name), None).await?;
    Ok(w)
}

/// CHILD ROLE: reopen alice's existing wallet and start the very payment the parent wants, with the
/// fault injector armed. `in_ladder_pay` must NOT return — the process is expected to die inside it,
/// right after `SP`'s co-signature is journalled and before any child ladder exists.
async fn crashing_child() -> Result<()> {
    let recipient = std::env::var(RECIPIENT)?;
    let parent_sid = std::env::var(PARENT_SID)?;
    let alice = wallet(ALICE).await?;
    println!("SDK81/child - starting the in-ladder split that will be killed mid-flight");
    let r = alice
        .in_ladder_pay(
            &parent_sid,
            &recipient,
            PAY,
            mercury_utexo_sdk::transfer::InLadderLatch::None,
        )
        .await;
    println!("SDK81/child - UNEXPECTED: in_ladder_pay returned instead of crashing: {r:?}");
    Ok(())
}

pub async fn execute() -> Result<()> {
    if std::env::var(PHASE).as_deref() == std::result::Result::Ok("kill") {
        return crashing_child().await;
    }

    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    let cc = mercuryrustlib::client_config::load().await;

    let alice = wallet(ALICE).await?;
    let bob = wallet(BOB).await?;
    let bob_address = bob.get_utexo_address().await?;

    // --- Alice deposits one coin; claim() auto-establishes its TES-R ladder. --------------------
    let t = prepaid_token(&cc).await?;
    alice.add_prepaid_token(&t).await;
    let addr = alice.get_deposit_address(DEPOSIT).await?;
    bitcoin_core::sendtoaddress(u32::try_from(DEPOSIT)?, &addr)?;
    let core = bitcoin_core::getnewaddress()?;
    bitcoin_core::generatetoaddress(3, &core)?;
    let mut waited = 0;
    loop {
        alice.claim().await?;
        if alice.get_balance().await?.available_sats == DEPOSIT {
            break;
        }
        waited += 1;
        if waited > 60 {
            return Err(anyhow!("alice deposit did not confirm"));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let parent_sid = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, ALICE)
        .await?
        .coins
        .iter()
        .find(|c| c.status == mercurylib::wallet::CoinStatus::CONFIRMED && c.duplicate_index == 0)
        .and_then(|c| c.statechain_id.clone())
        .ok_or(anyhow!("alice has no confirmed coin"))?;
    assert!(
        mercuryrustlib::tesr::load(&cc, ALICE, &parent_sid).await?.is_some(),
        "the parent must be laddered — this test is about the IN-LADDER split"
    );
    // The parent is a live, co-signable coin right now. That is what the crash destroys.
    let (_, _, terminal_before) =
        mercuryrustlib::lightning_latch::get_spend_budget(&cc, &parent_sid).await?;
    assert!(!terminal_before, "the parent must start non-terminal");
    println!("SDK81 - alice deposited {DEPOSIT} and laddered it (sid {parent_sid}); parent is live");

    // Release the sqlite pool so the child process can open the same database.
    drop(alice);
    tokio::time::sleep(Duration::from_secs(1)).await;

    // --- THE CRASH: a child process dies inside the split, right after SP's co-signature --------
    let exe = std::env::current_exe().map_err(|e| anyhow!("current_exe: {e}"))?;
    let child = std::process::Command::new(&exe)
        .env(PHASE, "kill")
        .env(RECIPIENT, &bob_address)
        .env(PARENT_SID, &parent_sid)
        .env("UTEXO_CRASH_POINT", "after_inladder_sp_sign")
        .env("SDK_E2E", "81")
        .env("ML_NETWORK", "regtest")
        .output()
        .map_err(|e| anyhow!("spawn crashing child: {e}"))?;
    let signal = {
        use std::os::unix::process::ExitStatusExt;
        child.status.signal()
    };
    println!(
        "SDK81 - child died: exit code {:?}, signal {signal:?} (6 == SIGABRT); last stdout: {}",
        child.status.code(),
        String::from_utf8_lossy(&child.stdout).lines().last().unwrap_or("")
    );
    assert_eq!(signal, Some(6), "the child must have been KILLED by abort(), not exited");
    assert!(
        String::from_utf8_lossy(&child.stderr).contains("after_inladder_sp_sign"),
        "the child must have died at the armed crash point: {}",
        String::from_utf8_lossy(&child.stderr)
    );

    // --- THE DAMAGE, MEASURED. The parent is terminal at the SE: its cooperative path is spent and
    // no further co-signature over it will ever be issued. Before this fix that was the end of the
    // coin — the one transaction that spends it existed only in the dead process's memory. --------
    let (_, _, terminal_after) =
        mercuryrustlib::lightning_latch::get_spend_budget(&cc, &parent_sid).await?;
    assert!(
        terminal_after,
        "the crash must have left the parent TERMINAL — otherwise this test is not exercising the \
         destructive window at all"
    );
    println!("SDK81 - the parent is now TERMINAL at the SE: SP can never be co-signed again");

    // --- RESTART: a fresh wallet handle over the same database, exactly like relaunching the app.
    let alice = wallet(ALICE).await?;
    let report = alice.recover_in_ladder_splits().await?;
    assert_eq!(report.len(), 1, "the interrupted split must be found: {report:?}");
    let entry = &report[0];
    assert_eq!(entry.lane, "in_ladder_split");
    assert_eq!(entry.terminalized_statechain_id, parent_sid);
    let (change_sid, piece_sid) = match &entry.outcome {
        InLadderSplitOutcome::Replayed { change_statechain_id, unconveyed_pieces } => {
            let change = change_statechain_id
                .clone()
                .ok_or_else(|| anyhow!("the replay must give alice her change child back"))?;
            assert_eq!(
                unconveyed_pieces.len(),
                1,
                "the recipient's piece must be reported as NOT conveyed — recovery never re-sends a \
                 payment on its own"
            );
            (change, unconveyed_pieces[0].clone())
        }
        other => return Err(anyhow!("expected the split to be replayed, got {other:?}")),
    };
    println!(
        "SDK81 - RECOVERED: the killed split was replayed from the journal (change {change_sid}, \
         piece {piece_sid} awaiting an explicit hand-over)"
    );

    // The change is a real, persisted, exitable claim again — the crash landed BEFORE any leg's
    // material existed, so the replay co-signed all of it.
    //
    // [CATS change 2] The change is a SPINE TIP now, under `spinetip-`, not a `ctesr-` child. The
    // row it is looked up through is part of the claim: a replay that wrote it under the child key
    // would file the sender's own change as a conveyable leaf, and `load_spine_tip` — which is what
    // `parent_shape`, `unilateral_exit` and the tower all read — would find nothing.
    let change_tip = mercuryrustlib::tesr::load_spine_tip(&cc, ALICE, &change_sid)
        .await?
        .ok_or_else(|| anyhow!("alice's change spine tip must be on disk after recovery"))?;
    assert_eq!(change_tip.statechain_id, change_sid);
    assert!(
        !change_tip.cap.signed_tx.is_empty(),
        "the replayed spine tip must carry its co-signed cap"
    );
    // ONE cap and no extension, and the record must satisfy its own persist-door precondition after
    // a REPLAY just as it does after a first run.
    change_tip
        .validate()
        .map_err(|e| anyhow!("the replayed spine tip must still validate: {e}"))?;
    assert_eq!(
        mercuryrustlib::tesr::spine_tip_exit_chain(&change_tip).len(),
        change_tip.parent.exit_tiers().len() + 1,
        "T, X_m, SP, then ONE cap"
    );
    assert!(
        mercuryrustlib::tesr::load_child(&cc, ALICE, &change_sid).await?.is_none(),
        "the change must NOT also be filed as a conveyable `ctesr-` child"
    );
    let alice_change = alice.get_balance().await?.available_sats;
    assert!(
        alice_change > 0 && alice_change < DEPOSIT,
        "alice's change must be booked as spendable value ({alice_change} sat)"
    );
    println!("SDK81 - alice's change child is exitable again ({alice_change} sat booked)");

    // Idempotent: a second pass has nothing left to do.
    assert!(
        alice.recover_in_ladder_splits().await?.is_empty(),
        "recovery must be idempotent"
    );

    // --- THE PROOF: the ORIGINAL payment still completes off-chain. -----------------------------
    let op_id = format!("in_ladder_split:{parent_sid}:{}", change_tip.parent.current().state.txid);
    alice
        .convey_recovered_piece(&op_id, &piece_sid, &bob_address)
        .await?;
    println!("SDK81 - handed the recovered piece over to bob");

    let mut waited = 0;
    loop {
        bob.claim().await?;
        if bob.get_balance().await?.available_sats == PAY {
            break;
        }
        waited += 1;
        if waited > 30 {
            return Err(anyhow!(
                "bob did not adopt the recovered piece; balance {:?}",
                bob.get_balance().await?
            ));
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    let bob_child_sid = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, BOB)
        .await?
        .coins
        .iter()
        .find(|c| {
            c.status == mercurylib::wallet::CoinStatus::CONFIRMED && c.amount == Some(PAY as u32)
        })
        .and_then(|c| c.statechain_id.clone())
        .ok_or(anyhow!("bob has no confirmed child coin"))?;
    assert_eq!(bob_child_sid, piece_sid, "bob adopted the very piece the crash interrupted");
    assert!(
        mercuryrustlib::tesr::load_child(&cc, BOB, &bob_child_sid).await?.is_some(),
        "bob must have ADOPTED (censused + persisted) the replayed child bundle"
    );

    println!(
        "SDK81 - SUCCESS [P0-3]: an in-ladder split killed by SIGABRT in the window between \
         terminalizing the parent and persisting the co-signed children is fully recovered from the \
         write-ahead journal after a restart. Both child ladders — neither of which existed when the \
         process died — were replayed, alice's change is an exitable claim again ({alice_change} \
         sat), recovery is idempotent and never re-sent the payment by itself, and the ORIGINAL \
         {PAY}-sat payment completed: bob censused and ADOPTED the replayed child. Before the fix \
         the parent was terminal with nothing on disk and the coin was exit-only forever."
    );
    Ok(())
}
