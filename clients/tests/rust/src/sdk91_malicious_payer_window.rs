//! E2E (SDK_E2E=91) — **the malicious payer, and the only gate that actually stands between them
//! and a conveyed-but-unclaimed coin.** The test `sdk90` said it could not do.
//!
//! # Why this exists
//!
//! `sdk90` set out to measure the coordinator's one-hour transfer window and measured the CLIENT
//! twice instead: first the wallet's own coin lookup, then [D145]'s sender-side outstanding-conveyance
//! refusal. Both are local. Both are the payer's own software. **A payer who wants to cheat does not
//! run them** — they POST to the coordinator directly, which is what this test does.
//!
//! So `sdk90`'s honest verdict was "an honest client is stopped locally, and the server-side window
//! was never reached." This is the other half: reach it.
//!
//! # Why the probe is legitimate rather than a fabricated attack
//!
//! Nothing here forges anything. `/sign/first` authenticates with `signed_statechain_id`, a signature
//! by the coin's owner auth key — and until the receiver completes the handover **the payer still
//! legitimately holds it**. It is sitting in their own wallet row (`Coin::signed_statechain_id`,
//! written at deposit). The "attack" is therefore just: use your own credential, skip your own
//! client. That is precisely the adversary the window exists for, and precisely the one no test in
//! this repo had ever put in front of it.
//!
//! Auth runs BEFORE the gate in `sign_first` (`server/src/endpoints/sign.rs`), so a garbage signature
//! would be turned away at `401` and never reach the thing under test. The real credential is what
//! makes this a measurement rather than a smoke test.
//!
//! # What is measured
//!
//! The gate returns a distinctive `409 Conflict` — *"coin has an open transfer (SE refuses
//! co-signatures until it completes or expires)"*. So the status code alone discriminates cleanly:
//!
//! * **inside** the window → `409` is the gate holding. ASSERTED: if it is anything else, the lock
//!   the whole conveyance model rests on does not exist on this path, and that is a Tier-1 finding.
//! * **outside** the window (the row aged past one hour) → RECORDED. `409` means something beyond
//!   the clock protects the payee; anything else means the one-hour window is the ONLY thing that
//!   ever did, and it has lapsed.
//!
//! That second line is the number [D85] and SPEC REQ-61 are about, and it has never been observed.
//!
//! # Note on what a lapse would and would not mean
//!
//! A lapse is **not** an immediate theft. Obtaining a `sign/first` session is the first move of one:
//! the payer must still complete `sign/second` and win a broadcast race against a state the receiver
//! holds at a lower CSV. The `has_open_transfer` comment in `transfer_sender.rs` describes exactly
//! that chain and calls its endpoint *"the receiver keeps the money it paid for; the payer keeps the
//! coin."* This test measures the first link only, and says so rather than implying the rest.
//!
//! Run: `SDK_E2E=91 ML_NETWORK=regtest cargo run` (regtest stack + lockbox up)

use anyhow::{anyhow, Result};
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};
use mercuryrustlib::client_config::ClientConfig;
use mercurylib::wallet::Coin;
use std::process::Command;
use std::time::Duration;

use crate::bitcoin_core;

const DEPOSIT: u64 = 120_000;
const DB: &str = "mercurylayer-db_server-1";

async fn wallet(name: &str) -> Result<UtexoWallet> {
    let (w, _) = UtexoWallet::initialize(SdkConfig::regtest(name), None).await?;
    Ok(w)
}

fn psql(sql: &str) -> Result<String> {
    let out = Command::new("docker")
        .args(["exec", DB, "psql", "-U", "postgres", "-d", "mercury", "-tAc", sql])
        .output()?;
    if !out.status.success() {
        return Err(anyhow!("psql failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn age_transfer_row(sid: &str) -> Result<u64> {
    let n = psql(&format!(
        "WITH u AS (UPDATE statechain_transfer \
           SET updated_at = NOW() - INTERVAL '2 hours', created_at = NOW() - INTERVAL '2 hours' \
         WHERE statechain_id = '{sid}' AND cancelled_at IS NULL RETURNING 1) SELECT count(*) FROM u"
    ))?;
    Ok(n.parse::<u64>().unwrap_or(0))
}

async fn coins_of(cc: &ClientConfig, wallet_name: &str) -> Result<Vec<Coin>> {
    Ok(mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, wallet_name).await?.coins)
}

/// THE PROBE. POST `/sign/first` with the payer's own, genuine credential — no client, no local
/// gates. Returns the HTTP status so the caller can discriminate `409` (the gate) from everything
/// else without pattern-matching prose that may be reworded.
async fn raw_sign_first(cc: &ClientConfig, sid: &str, signed_sid: &str) -> Result<(u16, String)> {
    let resp = reqwest::Client::new()
        .post(format!("{}/sign/first", cc.statechain_entity))
        .json(&serde_json::json!({
            "statechain_id": sid,
            "signed_statechain_id": signed_sid,
        }))
        .send()
        .await?;
    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap_or_default();
    Ok((status, body.chars().take(160).collect()))
}

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    std::env::set_var("ML_NETWORK", "regtest");
    let cc = mercuryrustlib::client_config::load().await;
    let core = bitcoin_core::getnewaddress()?;

    let alice = wallet("sdk91_alice").await?;
    let bob = wallet("sdk91_bob").await?;

    // ---- setup: alice funds a coin and conveys it WHOLE to bob, who never claims ---------------
    let before: Vec<String> = coins_of(&cc, "sdk91_alice").await?
        .into_iter().filter_map(|c| c.statechain_id).collect();
    let t = mercuryrustlib::deposit::get_token(&cc).await?;
    alice.add_prepaid_token(&t.token_id).await;
    let addr = alice.get_deposit_address(DEPOSIT).await?;
    bitcoin_core::sendtoaddress(u32::try_from(DEPOSIT)?, &addr)?;
    bitcoin_core::generatetoaddress(3, &core)?;

    let mut sid = String::new();
    for _ in 0..60 {
        alice.claim().await?;
        if let Some(s) = coins_of(&cc, "sdk91_alice").await?
            .into_iter().filter_map(|c| c.statechain_id).find(|s| !before.contains(s))
        {
            sid = s;
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    if sid.is_empty() {
        return Err(anyhow!("alice never confirmed her deposit"));
    }

    let bob_addr = bob.get_utexo_address().await?;
    mercuryrustlib::transfer_sender::execute(&cc, &bob_addr, "sdk91_alice", &sid, None, false, None)
        .await?;
    println!("SDK91 - [setup] alice conveyed {} to bob; bob is OFFLINE and never claims",
             &sid[..8.min(sid.len())]);

    // ---- the payer's own credential, read from their own wallet --------------------------------
    let signed_sid = coins_of(&cc, "sdk91_alice").await?
        .into_iter()
        .find(|c| c.statechain_id.as_deref() == Some(sid.as_str()))
        .and_then(|c| c.signed_statechain_id)
        .ok_or_else(|| anyhow!(
            "alice holds no signed_statechain_id for her own conveyed coin — the probe cannot \
             authenticate, and this test would silently measure a 401 instead of the gate"
        ))?;
    println!("SDK91 - [setup] payer credential in hand (their own, still valid pre-handover)");

    // ---- (a) ASSERTED: inside the window, the coordinator refuses with 409 ---------------------
    let (s_in, b_in) = raw_sign_first(&cc, &sid, &signed_sid).await?;
    println!("SDK91 - [a] raw /sign/first INSIDE the window -> HTTP {s_in}  {b_in}");
    if s_in == 401 {
        return Err(anyhow!(
            "[a] 401 — the probe failed to authenticate, so it never reached the gate. The test is \
             invalid as written; fix the credential before reading anything below."
        ));
    }
    if s_in != 409 {
        return Err(anyhow!(
            "[a] expected 409 (the open-transfer gate) and got {s_in}. A payer can obtain a co-sign \
             session on a conveyed-but-unclaimed coin WHILE the window is open — the lock the \
             conveyance model rests on is not enforced on this path at all."
        ));
    }
    println!("SDK91 - [a] PASS: the coordinator's open-transfer gate fired (409)");

    // ---- (b) ASSERTED: the window can be aged past its hard-coded hour --------------------------
    let rows = age_transfer_row(&sid)?;
    if rows == 0 {
        return Err(anyhow!("[b] aged 0 rows — the measurement below would be about nothing"));
    }
    println!("SDK91 - [b] PASS: transfer row aged 2h past `updated_at` ({rows} row(s))");

    // ---- (c) THE MEASUREMENT — never observed before this test ---------------------------------
    let (s_out, b_out) = raw_sign_first(&cc, &sid, &signed_sid).await?;
    println!("SDK91 - [c] raw /sign/first OUTSIDE the window -> HTTP {s_out}  {b_out}");

    let gate_still_holds = s_out == 409;
    println!(
        "SDK91 - [c] VERDICT: with the one-hour window LAPSED and the client bypassed, a payer {} \
         obtain a co-sign session on a coin they already conveyed and which the payee has not claimed.",
        if gate_still_holds { "still CANNOT" } else { "CAN" }
    );
    if !gate_still_holds {
        println!(
            "SDK91 - [c] SCOPE: this is the FIRST LINK of the chain `has_open_transfer`'s own \
             comment describes, not a completed theft — sign/second and a broadcast race against \
             the payee's lower-CSV state remain. Do not report it as more than it is."
        );
    }

    // [REQ-61 / D85] The owner latch must make this refuse for a reason that is not the clock.
    if std::env::var("EXPECT_LATCH").is_ok() && !gate_still_holds {
        return Err(anyhow!(
            "[c] EXPECT_LATCH=1 but a bypassing payer obtained a session after the window lapsed — \
             REQ-61's owner latch is absent or regressed"
        ));
    }

    // ---- (d) ASSERTED: the safety invariant — bob is still paid --------------------------------
    let mut got = None;
    for _ in 0..40 {
        bob.claim().await?;
        if let Some(c) = coins_of(&cc, "sdk91_bob").await?
            .into_iter().find(|c| c.statechain_id.as_deref() == Some(sid.as_str()))
        {
            got = Some(c.amount.unwrap_or(0) as u64);
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    match got {
        Some(v) if v == DEPOSIT => println!("SDK91 - [d] PASS: bob claimed his {DEPOSIT}-sat coin intact"),
        Some(v) => return Err(anyhow!("[d] SAFETY INVARIANT BROKEN: bob got {v}, expected {DEPOSIT}")),
        None => return Err(anyhow!(
            "[d] SAFETY INVARIANT BROKEN: bob could not claim the coin he was conveyed, after the \
             payer probed the lapsed window. This is the loss `has_open_transfer`'s comment predicts."
        )),
    }

    println!("SDK91 - ALL ASSERTIONS PASSED (the finding is [c], not the pass/fail)");
    Ok(())
}
