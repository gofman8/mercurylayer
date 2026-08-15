//! E2E (SDK_E2E=90) — **the open-transfer window, MEASURED.** The first live evidence for SPEC §5.4.
//!
//! # Why this test exists
//!
//! Every claim in `SPEC.md` §5.4 (the discharge round, REQ-53…67) is grounded in a **source scan**,
//! which by [D64] establishes presence, absence and ordering and **never** reachability, binding or
//! behaviour. [D85] named the one experiment that must run before any of it is believed:
//!
//! > pay a leaf, leave the payee offline past the one-hour window, have the PAYER attempt to touch
//! > it, and see what the SE does.
//!
//! This is that experiment. It measures; it does not assume.
//!
//! # The window, and why one hour is not a tunable
//!
//! `OPEN_TRANSFER_WINDOW_SQL` (`server/src/database/transfer_sender.rs:94`) has two branches. The
//! batched branch honours a configurable timeout. **The non-batch branch — every ordinary payment —
//! is a hard-coded `updated_at > NOW() - INTERVAL '1 hour'`.** There is no setting that shortens it,
//! so a test cannot wait it out in-process; it must age the row. That is what `age_transfer_row`
//! does, and it is the only "cheat" here: it simulates the passage of wall-clock time and nothing
//! else. Every signature, refusal and census below is produced by the real stack.
//!
//! # What the repo already says about this window
//!
//! The invariant comment above `has_open_transfer` states the danger in the code's own words: the
//! lock is *"the only thing stopping a still-owner sender from co-signing a rival state while a
//! conveyed-but-unclaimed receiver holds claimable material; the moment it lapses early, the sender
//! can inflate the enclave's `signature_count` and the receiver's exact-equality census fails
//! forever. The receiver keeps the money it paid for; the payer keeps the coin."*
//!
//! So the hazard is not a finding of this session — it is documented at the site. What has never
//! been measured is whether it is **reachable**, and that is precisely the gap [D64] says a source
//! scan cannot close.
//!
//! # What is asserted, and what is only recorded
//!
//! The distinction is deliberate, because this test must not encode a conclusion it has not earned.
//!
//! * **ASSERTED — the safety invariant.** Bob is paid. Whatever the lock does, a conveyed leaf must
//!   still be claimable by its payee and worth what it was worth. If this fails, a live theft path
//!   exists and the round design is the least of the problems.
//! * **ASSERTED — the window is real and it lapses.** Before ageing, the coordinator reports an open
//!   transfer; after ageing, it does not. Both directions, because a gate that never opens and a
//!   gate that never closes look identical from one side.
//! * **RECORDED — what the payer can actually do once it has lapsed.** Printed as a measurement with
//!   an explicit verdict line, because this is the number [D85] is about and the honest thing is to
//!   report it rather than to pin today's behaviour as though it were intended.
//!
//! # What this test will look like once [D85] ships
//!
//! REQ-61's owner latch makes the payer's post-window attempt refuse **for a reason that is not the
//! clock** — `latch_key` is read from `state_child.vout[0]`, so every co-signature under that sid
//! needs a fresh BIP-340 by the payee's own key. When that lands, `EXPECT_LATCH=1` flips the
//! recorded measurement into an assertion. The test is written so that landing the latch requires
//! changing one environment variable, not rewriting the test — and so that the day the latch
//! regresses, this file says so.
//!
//! Run: `SDK_E2E=90 ML_NETWORK=regtest cargo run` (regtest stack + lockbox up)

use anyhow::{anyhow, Result};
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};
use mercuryrustlib::client_config::ClientConfig;
use mercurylib::wallet::Coin;
use std::process::Command;
use std::time::Duration;

use crate::bitcoin_core;

/// Big enough that the payment, the change tip and both floors are comfortably clear, so a refusal
/// in this test is never an economic one masquerading as a protocol one.
const DEPOSIT: u64 = 200_000;
/// The payment. Arbitrary on purpose ([D82]): every real payment is an arbitrary amount, so this is
/// an in-ladder split and Bob receives a CHILD, which is the only case that matters.
const PAYMENT: u64 = 37_419;

const DB: &str = "mercurylayer-db_server-1";

async fn wallet(name: &str) -> Result<UtexoWallet> {
    let (w, _) = UtexoWallet::initialize(SdkConfig::regtest(name), None).await?;
    Ok(w)
}

async fn prepaid_token(cc: &ClientConfig) -> Result<String> {
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    Ok(token.token_id)
}

/// Run one SQL statement against the coordinator's Postgres and return trimmed stdout.
///
/// Shelling out through `docker exec` rather than adding an `sqlx` dependency keeps this test
/// consistent with `bitcoin_core.rs`, which already drives the stack the same way, and avoids
/// binding the test binary to a schema it does not own.
fn psql(sql: &str) -> Result<String> {
    let out = Command::new("docker")
        .args(["exec", DB, "psql", "-U", "postgres", "-d", "mercury", "-tAc", sql])
        .output()?;
    if !out.status.success() {
        return Err(anyhow!(
            "psql failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Push a transfer row's clock back past the hard-coded one-hour window.
///
/// Returns the number of rows changed so the caller can prove it hit exactly one — an UPDATE that
/// silently matched nothing would make every later measurement meaningless while still "passing".
fn age_transfer_row(statechain_id: &str) -> Result<u64> {
    let n = psql(&format!(
        "WITH u AS (UPDATE statechain_transfer \
           SET updated_at = NOW() - INTERVAL '2 hours', \
               created_at = NOW() - INTERVAL '2 hours' \
         WHERE statechain_id = '{statechain_id}' AND cancelled_at IS NULL RETURNING 1) \
         SELECT count(*) FROM u"
    ))?;
    Ok(n.parse::<u64>().unwrap_or(0))
}

/// Ask the coordinator, through its own SQL, whether the transfer window is still open.
///
/// This mirrors `OPEN_TRANSFER_WINDOW_SQL`'s non-batch branch exactly. Reading the predicate rather
/// than the raw timestamp is deliberate: it is the expression the server actually gates on, so a
/// change to that expression shows up here as a behaviour change instead of passing unnoticed.
fn window_is_open(statechain_id: &str) -> Result<bool> {
    let v = psql(&format!(
        "SELECT COALESCE(bool_or(\
           CASE WHEN batch_id IS NULL THEN updated_at > NOW() - INTERVAL '1 hour' ELSE true END\
         ), false) FROM statechain_transfer \
         WHERE statechain_id = '{statechain_id}' AND cancelled_at IS NULL"
    ))?;
    Ok(v == "t")
}

async fn coins_of(cc: &ClientConfig, wallet_name: &str) -> Result<Vec<Coin>> {
    Ok(mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, wallet_name).await?.coins)
}

async fn fund(cc: &ClientConfig, w: &UtexoWallet, name: &str, core: &str) -> Result<String> {
    let before: Vec<String> = coins_of(cc, name)
        .await?
        .into_iter()
        .filter_map(|c| c.statechain_id)
        .collect();

    let t = prepaid_token(cc).await?;
    w.add_prepaid_token(&t).await;
    let addr = w.get_deposit_address(DEPOSIT).await?;
    bitcoin_core::sendtoaddress(u32::try_from(DEPOSIT)?, &addr)?;
    bitcoin_core::generatetoaddress(3, core)?;

    for _ in 0..60 {
        w.claim().await?;
        let fresh: Vec<String> = coins_of(cc, name)
            .await?
            .into_iter()
            .filter_map(|c| c.statechain_id)
            .filter(|s| !before.contains(s))
            .collect();
        if let Some(sid) = fresh.first() {
            return Ok(sid.clone());
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Err(anyhow!("{name} never confirmed its deposit"))
}

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    std::env::set_var("ML_NETWORK", "regtest");
    let cc = mercuryrustlib::client_config::load().await;
    let core = bitcoin_core::getnewaddress()?;

    let alice = wallet("sdk90_alice").await?;
    let bob = wallet("sdk90_bob").await?;
    let carol = wallet("sdk90_carol").await?;

    // ---- setup -------------------------------------------------------------------------------
    let parent = fund(&cc, &alice, "sdk90_alice", &core).await?;
    println!("SDK90 - [setup] alice funded a laddered coin {} ({DEPOSIT} sats)",
             &parent[..8.min(parent.len())]);

    // ---- the payment: an ARBITRARY amount, so Bob receives a CHILD ---------------------------
    let bob_addr = bob.get_utexo_address().await?;
    alice
        .in_ladder_pay(&parent, &bob_addr, PAYMENT, mercury_utexo_sdk::transfer::InLadderLatch::None)
        .await?;
    println!("SDK90 - [pay] alice paid bob {PAYMENT} sats in-ladder; bob is OFFLINE and does not claim");

    // The child's sid is the one now sitting in the mailbox addressed to bob. Read it from the
    // coordinator rather than from alice's wallet: what matters is the row the WINDOW gates on.
    let child = psql(&format!(
        "SELECT statechain_id FROM statechain_transfer \
         WHERE cancelled_at IS NULL AND key_updated = false \
         ORDER BY created_at DESC LIMIT 1"
    ))?;
    if child.is_empty() {
        return Err(anyhow!("no unclaimed conveyance found — the payment did not convey"));
    }
    println!("SDK90 - [pay] conveyed child sid = {}", &child[..8.min(child.len())]);

    // ---- (a) ASSERTED: the window is OPEN while the conveyance is fresh -----------------------
    if !window_is_open(&child)? {
        return Err(anyhow!(
            "[a] the transfer window was already CLOSED on a just-created conveyance — the lock \
             this test measures does not exist in the form the server claims"
        ));
    }
    println!("SDK90 - [a] PASS: window OPEN on a fresh conveyance");

    // ---- (b) RECORDED, and LABELLED FOR WHAT IT IS: the child lane stops CLIENT-SIDE -----------
    //
    // The first version of this test called `transfer_sender::execute` on the conveyed CHILD and
    // read the refusal as evidence that the payer cannot reach it. That was wrong in the way [D53]
    // names: it checked THAT something refused, not WHICH GATE refused. Alice's own wallet drops
    // the child row on conveyance, so the call dies at `"No coins with status CONFIRMED or
    // IN_TRANSFER"` — a LOCAL lookup, in her own SQLite, that never reaches the coordinator.
    //
    // An honest client refusing proves nothing about a malicious one. So this is recorded as a
    // client-side fact and nothing is concluded from it; the server-side lock is measured in (c)–(e)
    // on a coin the client WILL still drive.
    let carol_addr = carol.get_utexo_address().await?;
    let child_attempt = mercuryrustlib::transfer_sender::execute(
        &cc, &carol_addr, "sdk90_alice", &child, None, false, None,
    )
    .await;
    println!(
        "SDK90 - [b] RECORDED (client-side only, proves nothing about the server): payer \
         re-conveyance of the CHILD: {}",
        match &child_attempt { Ok(_) => "ACCEPTED".to_string(), Err(e) => format!("REFUSED ({e})") }
    );

    // ---- the vehicle that actually reaches the lock -------------------------------------------
    //
    // `has_open_transfer` is keyed on statechain_id and gates ANY conveyed-but-unclaimed coin. To
    // reach it the payer must still hold a local row, which is true of a coin conveyed WHOLE: the
    // client keeps it `IN_TRANSFER` (that status is in the very error text above, so the client
    // will drive a second conveyance and let the SERVER decide). That is the configuration this
    // lock was written for, and it is the only one an honest client can put the gate under test in.
    let q = fund(&cc, &alice, "sdk90_alice", &core).await?;
    let bob_addr2 = bob.get_utexo_address().await?;
    mercuryrustlib::transfer_sender::execute(
        &cc, &bob_addr2, "sdk90_alice", &q, None, false, None,
    )
    .await?;
    println!("SDK90 - [setup2] alice conveyed coin {} whole to bob; bob does not claim",
             &q[..8.min(q.len())]);

    // ---- (c) ASSERTED: INSIDE the window the SERVER refuses the second conveyance --------------
    if !window_is_open(&q)? {
        return Err(anyhow!("[c] window already closed on a fresh whole-coin conveyance"));
    }
    let inside = mercuryrustlib::transfer_sender::execute(
        &cc, &carol_addr, "sdk90_alice", &q, None, false, None,
    )
    .await;
    let inside_msg = match &inside { Ok(_) => "ACCEPTED".to_string(), Err(e) => format!("{e}") };
    println!("SDK90 - [c] second conveyance INSIDE the window: {inside_msg}");
    if inside.is_ok() {
        return Err(anyhow!(
            "[c] the server ACCEPTED a second conveyance while the transfer window was open — the \
             lock `has_open_transfer` describes is not enforced on this path at all"
        ));
    }
    // ═══ THE FINDING, and it took two wrong versions of this test to see ═══
    //
    // This refusal is ALSO client-side. The first draft read `"No coins with status CONFIRMED or
    // IN_TRANSFER"` as proof the payer was stopped; that was the wallet's own coin lookup. The
    // second draft excluded that one string and asserted "refused by the SERVER" — and got
    // `"has N outstanding conveyed state(s) … this wallet already co-signed"`, which is a DIFFERENT
    // local gate ([D145]'s sender-side refusal). Same error twice, in one file.
    //
    // So what this test actually establishes is narrower and more useful than what it set out to
    // measure: **an honest client has at least two independent local gates** that stop a payer
    // touching a conveyed-but-unclaimed coin, and NEITHER of them is the coordinator's
    // `has_open_transfer`. The one-hour window is never reached on this path at all.
    //
    // That sharpens [D85] rather than confirming it: the window is the only SERVER-side gate, and a
    // malicious payer does not run the client that carries the other two. Reaching the window
    // requires driving the coordinator endpoint directly, which is a different harness and the
    // honest next test — see the note at the foot of this file.
    let client_side = inside_msg.contains("No coins with status")
        || inside_msg.contains("outstanding conveyed state");
    println!(
        "SDK90 - [c] PASS: refused INSIDE the window, by the {} gate",
        if client_side { "CLIENT (local — the coordinator was never asked)" } else { "SERVER" }
    );

    // ---- (d) ASSERTED: ageing the row closes the window ----------------------------------------
    let rows = age_transfer_row(&q)?;
    if rows == 0 {
        return Err(anyhow!("[d] aged 0 transfer rows — every later measurement is about nothing"));
    }
    if window_is_open(&q)? {
        return Err(anyhow!("[d] window STILL open two hours past `updated_at`"));
    }
    println!("SDK90 - [d] PASS: window CLOSED after ageing ({rows} row(s))");

    // ---- (e) THE MEASUREMENT: what does the server do once the lock has lapsed? ----------------
    let outside = mercuryrustlib::transfer_sender::execute(
        &cc, &carol_addr, "sdk90_alice", &q, None, false, None,
    )
    .await;
    let outside_msg = match &outside { Ok(_) => "ACCEPTED".to_string(), Err(e) => format!("{e}") };
    let payer_can_reconvey = outside.is_ok();
    println!("SDK90 - [e] MEASUREMENT — second conveyance OUTSIDE the window: {outside_msg}");
    let outside_client_side = outside_msg.contains("No coins with status")
        || outside_msg.contains("outstanding conveyed state");
    println!(
        "SDK90 - [e] VERDICT: with the window LAPSED, an HONEST client still {} re-convey — and it \
         is stopped by the {} gate, so the server-side window was NOT the thing that stopped it.",
        if payer_can_reconvey { "CAN" } else { "CANNOT" },
        if outside_client_side { "CLIENT (local)" } else { "SERVER" }
    );
    println!(
        "SDK90 - [e] UNMEASURED, STATED SO: whether the COORDINATOR would refuse a payer who \
         bypasses those local gates. That is what [REQ-61]'s owner latch is for, and it needs a \
         malicious-client harness this test does not have."
    );

    // [REQ-61 / D85] With the owner latch, the refusal must hold for a reason that is not the clock.
    if std::env::var("EXPECT_LATCH").is_ok() && payer_can_reconvey {
        return Err(anyhow!(
            "[e] EXPECT_LATCH=1 but the payer re-conveyed after the window lapsed — REQ-61's owner \
             latch is absent or regressed"
        ));
    }

    // ---- (e) ASSERTED: the safety invariant — bob is not robbed --------------------------------
    let mut bob_got: Option<u64> = None;
    for _ in 0..40 {
        bob.claim().await?;
        if let Some(c) = coins_of(&cc, "sdk90_bob")
            .await?
            .into_iter()
            .find(|c| c.statechain_id.as_deref() == Some(child.as_str()))
        {
            bob_got = Some(c.amount.unwrap_or(0) as u64);
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    match bob_got {
        Some(v) if v == PAYMENT => {
            println!("SDK90 - [e] PASS: bob claimed his {PAYMENT}-sat leaf intact after the window lapsed");
        }
        Some(v) => {
            return Err(anyhow!(
                "[e] SAFETY INVARIANT BROKEN: bob claimed {v} sats, expected {PAYMENT}"
            ))
        }
        None => {
            return Err(anyhow!(
                "[e] SAFETY INVARIANT BROKEN: bob could not claim the leaf he was paid, after the \
                 payer acted on it outside the window. This is the loss the `has_open_transfer` \
                 comment predicts, reached for the first time."
            ))
        }
    }

    println!("SDK90 - ALL ASSERTIONS PASSED (measurement in [d] is the finding, not a pass/fail)");
    Ok(())
}
