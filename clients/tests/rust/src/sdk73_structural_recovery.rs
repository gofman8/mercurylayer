//! E2E (SDK_E2E=73): F7 — a colored split survives a HARD PROCESS KILL taken in the exact window
//! that used to destroy it.
//!
//! A colored split terminalizes the carrier at the SE (`set_spend_budget`) and then obtains ONE
//! co-signature over the un-broadcast child transaction. Everything produced by that co-signature —
//! the signed tx, the consignment, the blinding, the child vouts — used to live only in process
//! memory until `register_split_subcoins` persisted the children. A crash in between left the
//! carrier terminal at the SE (never co-signable again) with its one spending transaction gone: the
//! cooperative off-chain path was destroyed, and only a unilateral exit of the carrier's own backup
//! could recover the BTC.
//!
//! This test does exactly that crash. A CHILD process runs the transfer with
//! `UTEXO_CRASH_POINT=after_structural_sign` and dies by `abort()` (SIGABRT — no unwinding, no
//! `Drop`, no flush) the instant the signed material is journalled. The parent then restarts the
//! wallet from the same database, runs the recovery reader, and completes the ORIGINAL payment from
//! the replayed material: bob claims the 250 TKN the killed process was sending him.
//!
//! Run: SDK_E2E=73 ML_NETWORK=regtest cargo run   (regtest + lockbox + RGB proxy up)

use anyhow::{anyhow, Result};
use mercury_utexo_sdk::tokens::StructuralSpendOutcome;
use mercury_utexo_sdk::{SdkConfig, UtexoWallet, WalletEvent};
use std::time::Duration;

use crate::bitcoin_core;

const ALICE: &str = "sdk73_alice";
const BOB: &str = "sdk73_bob";
const ALICE_RGB: &str = "./rgb-data-sdk73_alice";
const BOB_RGB: &str = "./rgb-data-sdk73_bob";

/// Env contract between the parent and the crashing child.
const PHASE: &str = "SDK73_PHASE";
const ASSET: &str = "SDK73_ASSET";
const RECIPIENT: &str = "SDK73_RECIPIENT";

fn alice_cfg() -> SdkConfig {
    let mut c = SdkConfig::regtest(ALICE);
    c.rgb_data_dir = Some(ALICE_RGB.to_string());
    c
}

fn bob_cfg() -> SdkConfig {
    let mut c = SdkConfig::regtest(BOB);
    c.rgb_data_dir = Some(BOB_RGB.to_string());
    c
}

async fn prepaid_token(cc: &mercuryrustlib::client_config::ClientConfig) -> Result<String> {
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    crate::utils::handle_token_response(cc, &token).await
}

/// CHILD ROLE: reopen alice's existing wallet and start the very transfer the parent wants, with the
/// SDK's fault injector armed. `transfer_tokens` must NOT return — the process is expected to die
/// inside it, right after the signed child material is journalled.
async fn crashing_child() -> Result<()> {
    let asset_id = std::env::var(ASSET)?;
    let recipient = std::env::var(RECIPIENT)?;
    let cc = mercuryrustlib::client_config::load().await;
    let (alice, _) = UtexoWallet::initialize(alice_cfg(), None).await?;
    for _ in 0..2 {
        let t = prepaid_token(&cc).await?;
        alice.add_prepaid_token(&t).await;
    }
    println!("SDK73/child - starting the colored split that will be killed mid-flight");
    let r = alice.transfer_tokens(&asset_id, &recipient, 250).await;
    // Reaching here means the crash point did not fire; exit 0 so the parent's assertion catches it.
    println!("SDK73/child - UNEXPECTED: transfer returned instead of crashing: {r:?}");
    Ok(())
}

pub async fn execute() -> Result<()> {
    if std::env::var(PHASE).as_deref() == std::result::Result::Ok("kill") {
        return crashing_child().await;
    }

    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    for d in [ALICE_RGB, BOB_RGB] {
        let _ = std::fs::remove_dir_all(d);
    }

    let cc = mercuryrustlib::client_config::load().await;
    let (alice, _) = UtexoWallet::initialize(alice_cfg(), None).await?;
    let (bob, _) = UtexoWallet::initialize(bob_cfg(), None).await?;
    let bob_address = bob.get_utexo_address().await?;
    println!("SDK73 - wallets up; bob address: {bob_address}");

    // --- alice issues 1000 TKN onto a statechain carrier ---------------------------------------
    let rgb_fund_addr = alice.get_token_funding_address().await?;
    bitcoin_core::sendtoaddress(100_000, &rgb_fund_addr)?;
    let core = bitcoin_core::getnewaddress()?;
    bitcoin_core::generatetoaddress(3, &core)?;
    tokio::time::sleep(Duration::from_secs(3)).await;

    let t = prepaid_token(&cc).await?;
    alice.add_prepaid_token(&t).await;
    let asset_id = alice.issue_token("TKN", "F7 Recovery Token", 0, 1000).await?;
    println!("SDK73 - issued 1000 TKN: {asset_id}");

    bitcoin_core::generatetoaddress(3, &core)?;
    let mut waited = 0;
    loop {
        alice.claim().await?;
        let b = alice.get_balance().await?;
        if !b.tokens.is_empty() && b.tokens[0].balance == 1000 {
            break;
        }
        waited += 1;
        if waited > 60 {
            return Err(anyhow!("token carrier did not confirm: {b:?}"));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    println!("SDK73 - alice's carrier confirmed with 1000 TKN settled");

    // Release the sqlite pool and the RGB stash so the child process can open them.
    drop(alice);
    tokio::time::sleep(Duration::from_secs(1)).await;

    // --- THE CRASH: a child process dies inside the colored split, right after the co-signature --
    let exe = std::env::current_exe().map_err(|e| anyhow!("current_exe: {e}"))?;
    let child = std::process::Command::new(&exe)
        .env(PHASE, "kill")
        .env(ASSET, &asset_id)
        .env(RECIPIENT, &bob_address)
        .env("UTEXO_CRASH_POINT", "after_structural_sign")
        .env("SDK_E2E", "73")
        .env("ML_NETWORK", "regtest")
        .output()
        .map_err(|e| anyhow!("spawn crashing child: {e}"))?;
    // On unix a process killed by a signal has no exit code; SIGABRT (6) is what `abort()` raises.
    let signal = {
        use std::os::unix::process::ExitStatusExt;
        child.status.signal()
    };
    println!(
        "SDK73 - child died: exit code {:?}, signal {signal:?} (6 == SIGABRT); last stdout: {}",
        child.status.code(),
        String::from_utf8_lossy(&child.stdout).lines().last().unwrap_or("")
    );
    assert_eq!(signal, Some(6), "the child must have been killed by abort(), not exited");
    assert!(
        !child.status.success(),
        "the child was supposed to be KILLED inside the colored split, not to finish it"
    );
    assert!(
        String::from_utf8_lossy(&child.stderr).contains("after_structural_sign"),
        "the child must have died at the armed crash point, not somewhere else: {}",
        String::from_utf8_lossy(&child.stderr)
    );

    // --- RESTART: a fresh wallet handle over the same database, exactly like relaunching the app --
    let (alice, _) = UtexoWallet::initialize(alice_cfg(), None).await?;
    let report = alice.recover_structural_spends().await?;
    assert_eq!(report.len(), 1, "the interrupted split must be found: {report:?}");
    let entry = &report[0];
    assert_eq!(entry.lane, "colored_split");
    assert_eq!(entry.receiver_address, bob_address, "the intended payee survived the crash");
    let piece_id = match &entry.outcome {
        StructuralSpendOutcome::Replayed { piece_id, handed_over } => {
            assert!(!handed_over, "a replay must never re-send a payment on its own");
            assert!(!piece_id.is_empty());
            piece_id.clone()
        }
        other => panic!("expected the signed material to be replayed, got {other:?}"),
    };
    println!("SDK73 - RECOVERED: the killed split was rebuilt from the journal; piece = {piece_id}");

    // Idempotent: a second pass has nothing left to do.
    assert!(
        alice.recover_structural_spends().await?.is_empty(),
        "recovery must be idempotent"
    );

    // The change came back to alice as a booked allocation (the piece is earmarked for bob).
    let alice_tokens = alice.get_token_balances().await?;
    assert_eq!(alice_tokens.len(), 1);
    assert_eq!(
        alice_tokens[0].balance, 750,
        "alice's 750 TKN change was re-registered by the replay"
    );

    // The piece is a real coin of this wallet, carrying the consignment envelope the receiver needs.
    let piece_backups = mercuryrustlib::sqlite_manager::get_backup_txs(&cc.pool, ALICE, &piece_id).await?;
    assert!(
        piece_backups
            .first()
            .and_then(|b| b.rgb_consignment.as_ref())
            .is_some(),
        "the replayed piece must carry its consignment envelope"
    );
    println!("SDK73 - the replayed piece carries its consignment envelope and its exit backup");

    // --- THE PROOF: the ORIGINAL payment still completes off-chain ------------------------------
    // The crash did not destroy the cooperative path — the recovered piece is handed to bob and he
    // claims exactly what the killed process was sending him.
    let mut bob_events = bob.subscribe();
    let bob_bg = bob.start_background();
    mercuryrustlib::transfer_sender::execute(&cc, &bob_address, ALICE, &piece_id, None, false, None)
        .await?;
    println!("SDK73 - handed the recovered piece over to bob");

    let (recv_asset, recv_amount) = tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            match bob_events.recv().await {
                Ok(WalletEvent::TokenTransferClaimed { asset_id, amount, .. }) => {
                    break (asset_id, amount)
                }
                Ok(_) => continue,
                Err(e) => panic!("event stream closed: {e}"),
            }
        }
    })
    .await
    .map_err(|_| anyhow!("bob did not claim the recovered token piece in time"))?;
    bob_bg.abort();
    assert_eq!(recv_asset, asset_id);
    assert_eq!(recv_amount, 250, "bob booked the amount the killed transfer was paying him");
    let bob_tokens = bob.get_token_balances().await?;
    assert_eq!(bob_tokens[0].balance, 250);
    let alice_tokens = alice.get_token_balances().await?;
    assert_eq!(alice_tokens[0].balance, 750);

    println!(
        "SDK73 - SUCCESS: a colored split killed by SIGABRT in the window between terminalizing the \
         carrier and persisting the co-signed child (the F7 gap) is fully recovered from the \
         write-ahead journal after a restart — the sub-coins, the RGB change booking and the \
         consignment envelope are rebuilt, recovery is idempotent, and the original 250 TKN payment \
         completes off-chain (alice 750 / bob 250)."
    );
    Ok(())
}
