//! E2E (SDK_E2E=71) — **unconditional laddering + a flat lane only carriers can reach**.
//!
//! One coin type: `claim()` ladders EVERY confirmed coin it can, with no opt-in and no flag, and the
//! conveyance path refuses to put a coin on the flat lane unless that coin is legitimately flat
//! (RGB carrier / [B0] un-broadcast funding / legacy no-aggregate). Proven here on the live stack:
//!
//!   1. a plain deposit is laddered by claim() alone — no `establish` call anywhere in this test;
//!   2. an RGB carrier is NOT laddered (terminal freeze), and the skip is now SURFACED as
//!      `WalletEvent::LadderSkipped { reason: RgbCarrier }` instead of being silent;
//!   3. the carrier still transfers off-chain (colored split), i.e. the flat lane still works for
//!      the coins that need it;
//!   4. a laddered coin can NEVER be conveyed at `protocol_version = 0`:
//!        4a. an unreadable ladder record is refused, not silently degraded to flat (the old code
//!            folded a DB read error into "no ladder" and the receiver then rejected the message
//!            with the sender's coin stuck IN_TRANSFER) — and [M2] the claim() pass now RECORDS and
//!            SURFACES that state (`LadderUnreadable`) instead of skipping the coin in silence;
//!        4b. a coin with NO ladder that is on-chain, bindable and not a carrier is refused as a
//!            BUG — it should have been laddered;
//!        4c. **[B1 regression]** a coin whose recorded reason is the TRANSIENT
//!            `rgb-state-unavailable` is refused. That reason used to be on the "legitimately flat"
//!            list, so in a token wallet — the only kind that can produce it — one RGB blip
//!            licensed flat conveyance of that coin forever after;
//!        4d. **[B2 regression]** a coin whose funding output cannot be resolved is refused. The
//!            fallback classifier used to `return Ok(())` there, so ANY coin the classifier could
//!            not explain (an electrum blip is enough) sailed onto the flat lane — the exact
//!            opposite of the fail-CLOSED contract in that function's own doc comment.
//!      every refusal happens BEFORE any SE co-sign, so the coin is untouched: it transfers normally
//!      right afterwards and the receiver claims it on the R′ path.
//!   5. **the legitimate case keeps working**: `assert_flat_conveyance_is_legitimate` says YES to the
//!      RGB carrier, and the carrier still transfers end-to-end. Without this the suite could pass
//!      by refusing everything.
//!   6. [M3] the persisted reason is readable through the SDK (`ladder_skip_reason` /
//!      `flat_only_coins`), so an app that starts AFTER the one-shot `LadderSkipped` event can still
//!      learn that a coin is flat-only.
//!
//! Run: SDK_E2E=71 ML_NETWORK=regtest cargo run   (regtest + lockbox + RGB proxy up)

use std::time::Duration;

use anyhow::{anyhow, Result};
use mercury_utexo_sdk::{LadderSkipReason, SdkConfig, UtexoWallet, WalletEvent};
use mercuryrustlib::CoinStatus;

use crate::bitcoin_core;

const PLAIN_AMOUNT: u32 = 123_456; // distinctive, so the plain coin is unambiguous vs the carrier

async fn prepaid_token(cc: &mercuryrustlib::client_config::ClientConfig) -> Result<String> {
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    crate::utils::handle_token_response(cc, &token).await
}

/// Drain everything currently queued on an event stream (lag is tolerated — a long confirm loop can
/// overflow the broadcast buffer, and we only ever assert on PRESENCE of an event).
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

/// A reply the coordinator client cannot parse — HTTP 200 with a body that is not JSON. Drives the
/// `Err` arm of `get_statechain_info` (the coordinator ERRORED).
const HTTP_GARBAGE: &str =
    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 8\r\nConnection: close\r\n\r\nnot json";
/// HTTP 404 — the coordinator answering "no such statechain id", which `get_statechain_info` maps to
/// `Ok(None)`. That arm used to `return Ok(())` and license the flat lane.
const HTTP_404: &str = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

/// A one-response HTTP server on an ephemeral loopback port; returns its base URL, suitable as a
/// `statechain_entity`. Lets the test drive the coordinator arms that no real coordinator will
/// produce on demand. The task loops for the life of the process (the test is a single run).
async fn canned_http_server(response: &'static str) -> Result<String> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        loop {
            let Ok((mut s, _)) = listener.accept().await else { return };
            let mut buf = [0u8; 2048];
            let _ = s.read(&mut buf).await; // drain the request line/headers
            let _ = s.write_all(response.as_bytes()).await;
            let _ = s.flush().await;
            let _ = s.shutdown().await;
        }
    });
    Ok(format!("http://{addr}"))
}

async fn raw_row(cc: &mercuryrustlib::client_config::ClientConfig, wallet: &str, key: &str) -> Option<String> {
    mercuryrustlib::sqlite_manager::get_all_backup_txs(&cc.pool, wallet)
        .await
        .ok()?
        .into_iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v)
}

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    for d in ["./rgb-data-sdk71_alice", "./rgb-data-sdk71_bob"] {
        let _ = std::fs::remove_dir_all(d);
    }
    std::env::set_var("ML_NETWORK", "regtest");

    // `mut` so cases 4h..4j can point `statechain_entity` at a dead port / a canned server and prove
    // that a coordinator we cannot ask licenses NOTHING. Restored immediately afterwards.
    let mut cc = mercuryrustlib::client_config::load().await;
    let mut alice_cfg = SdkConfig::regtest("sdk71_alice");
    alice_cfg.rgb_data_dir = Some("./rgb-data-sdk71_alice".to_string());
    let mut bob_cfg = SdkConfig::regtest("sdk71_bob");
    bob_cfg.rgb_data_dir = Some("./rgb-data-sdk71_bob".to_string());

    let (alice, _) = UtexoWallet::initialize(alice_cfg, None).await?;
    let (bob, _) = UtexoWallet::initialize(bob_cfg, None).await?;
    let bob_address = bob.get_utexo_address().await?;
    let mut alice_events = alice.subscribe();

    // ---- 1. A plain deposit + an RGB issuance carrier, in ONE wallet. ---------------------------
    let t = prepaid_token(&cc).await?;
    alice.add_prepaid_token(&t).await;
    let plain_addr = alice.get_deposit_address(PLAIN_AMOUNT as u64).await?;
    bitcoin_core::sendtoaddress(PLAIN_AMOUNT, &plain_addr)?;
    let rgb_fund_addr = alice.get_token_funding_address().await?;
    bitcoin_core::sendtoaddress(100_000, &rgb_fund_addr)?;
    let core = bitcoin_core::getnewaddress()?;
    bitcoin_core::generatetoaddress(3, &core)?;
    tokio::time::sleep(Duration::from_secs(3)).await; // electrs indexing

    let t = prepaid_token(&cc).await?;
    alice.add_prepaid_token(&t).await;
    let asset_id = alice.issue_token("TKN", "One Coin Type Token", 0, 1000).await?;
    bitcoin_core::generatetoaddress(3, &core)?;

    // No `establish` call anywhere: claim() is the ONLY thing that ladders.
    let mut seen: Vec<WalletEvent> = Vec::new();
    let mut waited = 0;
    loop {
        alice.claim().await?;
        seen.extend(drain(&mut alice_events));
        let b = alice.get_balance().await?;
        if b.available_sats >= PLAIN_AMOUNT as u64 && !b.tokens.is_empty() {
            break;
        }
        waited += 1;
        if waited > 60 {
            return Err(anyhow!("plain coin + carrier did not both confirm: {b:?}"));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    seen.extend(drain(&mut alice_events));
    assert_eq!(alice.get_token_balances().await?[0].balance, 1000, "full supply on alice");

    let coins = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk71_alice").await?.coins;
    let confirmed: Vec<_> = coins
        .iter()
        .filter(|c| c.status == CoinStatus::CONFIRMED && c.duplicate_index == 0)
        .collect();
    let plain = confirmed
        .iter()
        .find(|c| c.amount == Some(PLAIN_AMOUNT))
        .ok_or(anyhow!("plain coin not found"))?;
    let plain_sid = plain.statechain_id.clone().ok_or(anyhow!("plain has no sid"))?;
    let carriers: Vec<_> = confirmed.iter().filter(|c| c.amount != Some(PLAIN_AMOUNT)).collect();
    assert!(!carriers.is_empty(), "the issuance produced a carrier coin");

    // ---- 2. The plain deposit is laddered with NO opt-in. ---------------------------------------
    assert!(
        mercuryrustlib::tesr::load(&cc, "sdk71_alice", &plain_sid).await?.is_some(),
        "a plain deposit must be laddered by claim() alone (unconditional laddering)"
    );
    assert!(
        seen.iter().any(|e| matches!(e, WalletEvent::LadderEstablished { statechain_id } if *statechain_id == plain_sid)),
        "LadderEstablished was not emitted for the plain coin"
    );
    // No stale "left flat" record survives on a laddered coin.
    assert!(
        mercuryrustlib::transfer_sender::read_ladder_skip(&cc, "sdk71_alice", &plain_sid, 0).await.is_none(),
        "a laddered coin must carry no flat-lane record"
    );
    assert!(
        mercuryrustlib::transfer_sender::is_ladder_managed(&cc, "sdk71_alice").await,
        "claim() must mark the wallet ladder-managed"
    );
    println!("SDK71 - plain deposit {plain_sid} laddered by claim() alone");

    // ---- 3. The carrier is NOT laddered, and the skip is SURFACED (the UX defect this closes). ---
    for c in &carriers {
        let sid = c.statechain_id.clone().ok_or(anyhow!("carrier has no sid"))?;
        assert!(
            mercuryrustlib::tesr::load(&cc, "sdk71_alice", &sid).await?.is_none(),
            "the RGB carrier {sid} must NOT be laddered (terminal freeze)"
        );
        assert_eq!(
            mercuryrustlib::transfer_sender::read_ladder_skip(&cc, "sdk71_alice", &sid, 0).await.as_deref(),
            Some(mercuryrustlib::transfer_sender::FLAT_RGB_CARRIER),
            "the carrier's flat-lane reason must be recorded for the conveyance path"
        );
        assert!(
            seen.iter().any(|e| matches!(
                e,
                WalletEvent::LadderSkipped { statechain_id, reason }
                    if *statechain_id == sid && *reason == LadderSkipReason::RgbCarrier
            )),
            "LadderSkipped{{RgbCarrier}} was not emitted for carrier {sid} — the app would only \
             discover the coin is flat-only at transfer time"
        );
    }
    println!("SDK71 - {} carrier(s) left flat, recorded + surfaced as LadderSkipped", carriers.len());

    // ---- 4a. An unreadable ladder is REFUSED, never silently degraded to the flat lane. ----------
    let tesr_key = format!("tesr-{plain_sid}");
    let saved = raw_row(&cc, "sdk71_alice", &tesr_key)
        .await
        .ok_or(anyhow!("the plain coin's ladder row is missing"))?;
    mercuryrustlib::sqlite_manager::insert_raw_backup_txs(
        &cc.pool,
        "sdk71_alice",
        &tesr_key,
        "{\"not\":\"a bundle\"}",
    )
    .await?;
    let err = mercuryrustlib::transfer_sender::execute(
        &cc, &bob_address, "sdk71_alice", &plain_sid, None, false, None,
    )
    .await
    .expect_err("an unreadable ladder must refuse the transfer, not convey protocol_version 0")
    .to_string();
    assert!(
        err.contains("could not read the exit ladder"),
        "unexpected refusal: {err}"
    );
    // [M2] The ladder pass must RECORD and SURFACE the unreadable row rather than skipping the coin
    // in silence — this was the one flat-only case that produced no record and no event at all, and
    // it is the one the owner most needs to hear about (the coin is untransferable until restored).
    let mut m2_events = alice.subscribe();
    alice.claim().await?;
    let m2_seen = drain(&mut m2_events);
    assert_eq!(
        mercuryrustlib::transfer_sender::read_ladder_skip(&cc, "sdk71_alice", &plain_sid, 0).await.as_deref(),
        Some(mercuryrustlib::transfer_sender::FLAT_LADDER_UNREADABLE),
        "[M2] an unreadable tesr- row must be recorded as a flat-lane reason"
    );
    assert!(
        m2_seen.iter().any(|e| matches!(
            e,
            WalletEvent::LadderSkipped { statechain_id, reason }
                if *statechain_id == plain_sid && *reason == LadderSkipReason::LadderUnreadable
        )),
        "[M2] LadderSkipped{{LadderUnreadable}} was not emitted: {m2_seen:?}"
    );
    // [M3] and it is readable through the SDK, without having caught the one-shot event.
    assert_eq!(
        alice.ladder_skip_reason(&plain_sid).await,
        Some(LadderSkipReason::LadderUnreadable),
        "[M3] the SDK accessor must report the persisted flat-only reason"
    );
    let flat = alice.flat_only_coins().await?;
    assert!(
        flat.iter().any(|(sid, reason, transferable)| {
            *sid == plain_sid
                && reason == mercuryrustlib::transfer_sender::FLAT_LADDER_UNREADABLE
                && !*transferable
        }),
        "[M3] flat_only_coins must list the coin as NOT transferable: {flat:?}"
    );
    // An unreadable ladder must NOT license flat conveyance either.
    assert!(
        !LadderSkipReason::LadderUnreadable.permits_flat_conveyance(),
        "[M2] 'ladder-unreadable' must never license a flat conveyance"
    );
    println!("SDK71 - unreadable ladder refused, recorded (M2) and readable via the SDK (M3)");

    // ---- 4b. A coin that SHOULD have a ladder but has none is refused as a bug. ------------------
    mercuryrustlib::transfer_sender::delete_raw_backup_row(&cc, "sdk71_alice", &tesr_key).await?;
    mercuryrustlib::transfer_sender::delete_raw_backup_row(
        &cc,
        "sdk71_alice",
        &mercuryrustlib::transfer_sender::ladder_skip_key(&plain_sid, 0),
    )
    .await?;
    let err = mercuryrustlib::transfer_sender::execute(
        &cc, &bob_address, "sdk71_alice", &plain_sid, None, false, None,
    )
    .await
    .expect_err("an on-chain, bindable, non-carrier coin with no ladder must not convey flat")
    .to_string();
    assert!(err.contains("should have been laddered"), "unexpected refusal: {err}");
    println!("SDK71 - unexplained un-laddered coin refused (flat lane is carrier-only here)");

    // ---- 4c. [B1] A TRANSIENT recorded reason must NOT license a flat conveyance. ----------------
    //
    // `rgb-state-unavailable` is written when the wallet's RGB state could not be read for ONE
    // claim() pass; the next pass self-heals. It used to sit on the "legitimately flat" list, which
    // meant that in a token wallet — the only kind that can produce it, i.e. exactly the wallets
    // where carriers matter — a single RGB blip licensed that coin to convey flat forever after.
    // The coin's ladder is still deleted here (from 4b), so this exercises the recorded-reason arm.
    let skip_key = mercuryrustlib::transfer_sender::ladder_skip_key(&plain_sid, 0);
    for transient in [
        mercuryrustlib::transfer_sender::FLAT_RGB_STATE_UNAVAILABLE,
        mercuryrustlib::transfer_sender::FLAT_COORDINATOR_UNAVAILABLE,
        mercuryrustlib::transfer_sender::FLAT_ESTABLISH_FAILED,
        mercuryrustlib::transfer_sender::FLAT_FUNDING_UNRESOLVABLE,
    ] {
        mercuryrustlib::sqlite_manager::insert_raw_backup_txs(
            &cc.pool,
            "sdk71_alice",
            &skip_key,
            &format!("{{\"reason\":\"{transient}\",\"at\":\"2026-07-29T00:00:00Z\"}}"),
        )
        .await?;
        let err = mercuryrustlib::transfer_sender::execute(
            &cc, &bob_address, "sdk71_alice", &plain_sid, None, false, None,
        )
        .await
        .err()
        .map(|e| e.to_string())
        .unwrap_or_else(|| {
            panic!(
                "[B1] REGRESSION: a coin recorded '{transient}' was CONVEYED on the flat lane. A \
                 transient skip reason must never license flat conveyance."
            )
        });
        assert!(
            err.contains(transient) && err.contains("Refusing to convey"),
            "[B1] unexpected refusal for '{transient}': {err}"
        );
        assert!(
            !mercuryrustlib::transfer_sender::is_legitimate_flat_reason(transient),
            "[B1] '{transient}' is a DEFERRAL, not a licence to convey flat"
        );
    }
    // The refusal must survive a wallet reload too (the record is persisted, not in-memory state).
    assert_eq!(
        mercuryrustlib::transfer_sender::read_ladder_skip(&cc, "sdk71_alice", &plain_sid, 0)
            .await
            .as_deref(),
        Some(mercuryrustlib::transfer_sender::FLAT_FUNDING_UNRESOLVABLE),
    );
    println!("SDK71 - [B1] all 4 transient reasons (rgb-state-unavailable / coordinator-unavailable / establish-failed / funding-unresolvable) refused, not licensed");

    // ---- 4d. [B2] A coin with NO positive explanation must NOT be conveyable flat. ---------------
    //
    // `assert_flat_conveyance_is_legitimate` promises in its own doc comment that anything it cannot
    // positively explain is an error. Its fallback contradicted that: when the coin's funding output
    // could not be resolved it `return Ok(())`-ed, and that is not the [B0] off-chain case — an
    // electrum blip produces the identical `None`. Point the coin at a funding tx the chain backend
    // has never heard of and require a refusal.
    mercuryrustlib::transfer_sender::delete_raw_backup_row(&cc, "sdk71_alice", &skip_key).await?;
    const NO_SUCH_TXID: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    let real_txid = {
        let mut w = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk71_alice").await?;
        let mut saved_txid = None;
        for c in w.coins.iter_mut() {
            if c.statechain_id.as_deref() == Some(plain_sid.as_str()) && c.duplicate_index == 0 {
                saved_txid = c.utxo_txid.clone();
                c.utxo_txid = Some(NO_SUCH_TXID.to_string());
            }
        }
        mercuryrustlib::sqlite_manager::update_wallet(&cc.pool, &w).await?;
        saved_txid.ok_or(anyhow!("plain coin has no funding txid to swap"))?
    };
    let err = mercuryrustlib::transfer_sender::execute(
        &cc, &bob_address, "sdk71_alice", &plain_sid, None, false, None,
    )
    .await
    .err()
    .map(|e| e.to_string())
    .unwrap_or_else(|| {
        panic!(
            "[B2] REGRESSION: a coin whose funding output could not be resolved was CONVEYED on the \
             flat lane. 'cannot explain' must fail CLOSED, not open."
        )
    });
    // The refusal must come from the CLASSIFIER, before any co-sign. Any other error means the
    // classifier let the coin through and something downstream tripped over it instead — which is
    // the fail-OPEN this case exists to catch.
    assert!(
        err.contains("could not be read from the chain backend") && err.contains("Refusing to convey"),
        "[B2] the flat-lane classifier did not fail CLOSED on an unresolvable funding output; the \
         transfer got past it and failed elsewhere instead: {err}"
    );
    // Restore the real funding outpoint.
    {
        let mut w = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk71_alice").await?;
        for c in w.coins.iter_mut() {
            if c.statechain_id.as_deref() == Some(plain_sid.as_str()) && c.duplicate_index == 0 {
                c.utxo_txid = Some(real_txid.clone());
            }
        }
        mercuryrustlib::sqlite_manager::update_wallet(&cc.pool, &w).await?;
    }
    println!("SDK71 - [B2] an unexplainable coin refused (the fallback fails CLOSED)");

    // ---- 4e..4k. [B3] ONE CASE PER REJECTED CLASS — the inverted classifier. ---------------------
    //
    // Three review rounds each found a NEW fail-open here, because the function was written as
    // "return Ok(()) unless something looks wrong": every arm was a candidate hole, and patching
    // arms one at a time never converged. The polarity is now inverted — the classifier computes an
    // `Option<PermanentLicence>` from POSITIVE evidence and has exactly ONE `Ok(())`, reached only
    // by `Some(licence)`. These cases pin the classes that must therefore REFUSE. Each one used to
    // be (or was one edit away from being) a silent flat conveyance.
    //
    // The plain coin is un-laddered here (4b deleted its `tesr-` row) with no recorded reason (4d
    // deleted its `ladderskip-` row), so every case below reaches the live probes.
    let branch_key = format!("branch-{plain_sid}");
    let child_key = format!("ctesr-{plain_sid}");

    // 4e. An UNPARSEABLE `branch-<id>` row. A `branch-` chain is the [B0] "funding is off chain"
    //     witness; a row we cannot read is not that witness, it is an unreadable row.
    mercuryrustlib::sqlite_manager::insert_raw_backup_txs(&cc.pool, "sdk71_alice", &branch_key, "not json").await?;
    let err = mercuryrustlib::transfer_sender::execute(
        &cc, &bob_address, "sdk71_alice", &plain_sid, None, false, None,
    )
    .await
    .err()
    .map(|e| e.to_string())
    .unwrap_or_else(|| panic!("[B3] an unparseable branch- row licensed a flat conveyance"));
    assert!(
        err.contains("exit-branch row that could not be parsed") && err.contains("Refusing to convey"),
        "[B3] unexpected refusal for an unparseable branch- row: {err}"
    );
    mercuryrustlib::transfer_sender::delete_raw_backup_row(&cc, "sdk71_alice", &branch_key).await?;

    // 4f. A `ctesr-<id>` row that EXISTS but does not parse. A prior round accepted this key on row
    //     existence alone — while requiring its `branch-` sibling to parse — so any bytes under that
    //     key licensed the flat lane.
    mercuryrustlib::sqlite_manager::insert_raw_backup_txs(&cc.pool, "sdk71_alice", &child_key, "not json").await?;
    let err = mercuryrustlib::transfer_sender::execute(
        &cc, &bob_address, "sdk71_alice", &plain_sid, None, false, None,
    )
    .await
    .err()
    .map(|e| e.to_string())
    .unwrap_or_else(|| {
        panic!("[B3] a ctesr- row that does not parse licensed a flat conveyance (existence is not evidence)")
    });
    assert!(
        err.contains("split-child bundle row that could not be parsed") && err.contains("Refusing to convey"),
        "[B3] unexpected refusal for an unparseable ctesr- row: {err}"
    );
    mercuryrustlib::transfer_sender::delete_raw_backup_row(&cc, "sdk71_alice", &child_key).await?;
    println!("SDK71 - [B3] unparseable branch- and ctesr- rows both refused (a row is not evidence unless it parses)");

    // 4g. `ladder_binding_precheck` failing for a NON-aggregate reason. The classifier used to test
    //     `.is_err()` and license the flat lane on EVERY cause, folding "this funding output is not
    //     even taproot" and "this aggregate does not control F" (a decoy-shaped coin) into the
    //     harmless pre-0009 legacy explanation. Point the coin at a DIFFERENT output of its own
    //     funding tx — a real, on-chain, readable output that the coin's aggregate does not control.
    let plain_coin = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk71_alice")
        .await?
        .coins
        .into_iter()
        .find(|c| c.statechain_id.as_deref() == Some(plain_sid.as_str()) && c.duplicate_index == 0)
        .ok_or(anyhow!("plain coin vanished"))?;
    // THE MISMATCH: an outpoint that is valid but is NOT THIS COIN'S.
    //
    // This used to bump the vout within the coin's own funding tx, which needed that tx to have a
    // second output — i.e. it depended on bitcoind's coin selection happening to produce change. It
    // does not always, and then the test aborted with "the funding tx has a single output; cannot
    // build the mismatch case" — a SETUP failure wearing the costume of a result.
    //
    // Another coin's funding outpoint is a mismatch by construction, always available (this wallet
    // has several coins by now), and closer to the real shape: the sender declares an outpoint that
    // belongs to something else.
    let mismatched = {
        let others: Vec<_> = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk71_alice")
            .await?
            .coins
            .into_iter()
            .filter(|c| {
                c.utxo_txid.is_some() && c.utxo_txid != plain_coin.utxo_txid
            })
            .collect();
        let donor = others.first().ok_or(anyhow!(
            "no second funded coin to borrow an outpoint from — this wallet should hold several by \
             now; the mismatch case needs one outpoint that is not this coin's"
        ))?;
        let mut m = plain_coin.clone();
        m.utxo_txid = donor.utxo_txid.clone();
        m.utxo_vout = donor.utxo_vout;
        m
    };
    let err = mercuryrustlib::transfer_sender::assert_flat_conveyance_is_legitimate(
        &cc, "sdk71_alice", &plain_sid, &mismatched, "regtest",
    )
    .await
    .err()
    .map(|e| e.to_string())
    .unwrap_or_else(|| {
        panic!(
            "[B3] a binding failure that is NOT the legacy no-aggregate case licensed a flat \
             conveyance — the blanket is_err() fail-open is back"
        )
    });
    assert!(
        err.contains("NOT the legacy no-aggregate case") && err.contains("Refusing to convey"),
        "[B3] unexpected refusal for a non-aggregate binding failure: {err}"
    );
    println!("SDK71 - [B3] a NON-aggregate binding failure refused (only 'the coordinator recorded no aggregate' licenses)");

    // 4h..4j. THE COORDINATOR. "We could not ask" is not "the coordinator said no aggregate exists",
    //     and a prior round collapsed exactly those two. All three shapes must refuse: unreachable,
    //     an answer we cannot read, and a 404 (`Ok(None)` — no record at all, which is an anomaly
    //     for a coin about to be transferred, not a licence).
    let real_entity = cc.statechain_entity.clone();
    for (label, entity, expect) in [
        // Unreachable: nothing is listening on port 1.
        ("unreachable", "http://127.0.0.1:1".to_string(), "could not be reached"),
        // Errored: a reply we cannot parse.
        ("errored", canned_http_server(HTTP_GARBAGE).await?, "could not be reached"),
        // Ambiguous: HTTP 404 → `Ok(None)`. Used to `return Ok(())`.
        ("no-record", canned_http_server(HTTP_404).await?, "has no record of it at all"),
    ] {
        cc.statechain_entity = entity;
        let err = mercuryrustlib::transfer_sender::assert_flat_conveyance_is_legitimate(
            &cc, "sdk71_alice", &plain_sid, &plain_coin, "regtest",
        )
        .await
        .err()
        .map(|e| e.to_string())
        .unwrap_or_else(|| {
            panic!("[B3] coordinator '{label}' licensed a flat conveyance — 'we could not ask' is not evidence")
        });
        assert!(
            err.contains(expect) && err.contains("Refusing to convey"),
            "[B3] unexpected refusal for coordinator '{label}': {err}"
        );
    }
    cc.statechain_entity = real_entity;
    println!("SDK71 - [B3] coordinator unreachable / errored / no-record all refused (only a positive 'no aggregate' answer licenses)");

    // 4k. A MISSING SCOPE MARKER must not be a global off-switch. `ladder-managed` is written by a
    //     best-effort insert, and its absence used to disable the ENTIRE classifier for the wallet —
    //     one failed write and every coin in it could convey flat forever. The gate is now armed by
    //     positive evidence that the SDK ladder pass has run (any `tesr-` / `ctesr-` / `ladderskip-`
    //     artefact), so deleting the marker changes nothing.
    mercuryrustlib::transfer_sender::delete_raw_backup_row(
        &cc, "sdk71_alice", mercuryrustlib::transfer_sender::LADDER_MANAGED_KEY,
    )
    .await?;
    assert!(
        !mercuryrustlib::transfer_sender::is_ladder_managed(&cc, "sdk71_alice").await,
        "the scope marker must actually be gone for this case to mean anything"
    );
    let err = mercuryrustlib::transfer_sender::execute(
        &cc, &bob_address, "sdk71_alice", &plain_sid, None, false, None,
    )
    .await
    .err()
    .map(|e| e.to_string())
    .unwrap_or_else(|| {
        panic!(
            "[B3] deleting the `ladder-managed` row turned the whole classifier off — a missing \
             scope marker must not be a global off-switch"
        )
    });
    assert!(err.contains("should have been laddered"), "[B3] unexpected refusal with no scope marker: {err}");
    mercuryrustlib::transfer_sender::mark_ladder_managed(&cc, "sdk71_alice").await?;
    println!("SDK71 - [B3] a missing scope marker still REFUSES (the gate is armed by the pass's own artefacts)");

    // Restore the ladder. Every refusal above fired BEFORE any SE co-sign, so the coin is untouched —
    // the transfer in step 6 proves it.
    mercuryrustlib::sqlite_manager::insert_raw_backup_txs(&cc.pool, "sdk71_alice", &tesr_key, &saved).await?;

    // ---- 5. The carrier still TRANSFERS (flat lane intact for the coins that need it). -----------
    //
    // THE LEGITIMATE CASE MUST KEEP WORKING. Without this the fail-closed rewrite could "pass" by
    // refusing every coin in the wallet, which would be a worse bug than the one it fixes. Assert it
    // directly against the classifier first, then end-to-end through a real transfer.
    for c in &carriers {
        let sid = c.statechain_id.clone().ok_or(anyhow!("carrier has no sid"))?;
        mercuryrustlib::transfer_sender::assert_flat_conveyance_is_legitimate(
            &cc,
            "sdk71_alice",
            &sid,
            c,
            "regtest",
        )
        .await
        .map_err(|e| anyhow!("an RGB carrier must still be conveyable flat, but it was refused: {e}"))?;
        assert!(
            LadderSkipReason::RgbCarrier.permits_flat_conveyance(),
            "'rgb-carrier' must remain a licence for the flat lane"
        );
        // [M3] and the app can see the reason without having caught the event.
        assert_eq!(
            alice.ladder_skip_reason(&sid).await,
            Some(LadderSkipReason::RgbCarrier),
            "[M3] the SDK accessor must report the carrier's flat-only reason"
        );
    }
    let flat = alice.flat_only_coins().await?;
    for c in &carriers {
        let sid = c.statechain_id.clone().unwrap();
        assert!(
            flat.iter().any(|(s, r, t)| *s == sid
                && r == mercuryrustlib::transfer_sender::FLAT_RGB_CARRIER
                && *t),
            "[M3] flat_only_coins must list carrier {sid} as still transferable: {flat:?}"
        );
    }
    println!("SDK71 - the legitimate flat lane still says YES to {} carrier(s)", carriers.len());

    let mut bob_events = bob.subscribe();
    let bob_bg = bob.start_background();
    for _ in 0..2 {
        let t = prepaid_token(&cc).await?;
        alice.add_prepaid_token(&t).await;
    }
    let r = alice.transfer_tokens(&asset_id, &bob_address, 250).await?;
    assert!(r.used_split, "off-chain colored split");
    let recv_amount = tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            match bob_events.recv().await {
                Ok(WalletEvent::TokenTransferClaimed { asset_id: a, amount, .. }) if a == asset_id => break amount,
                Ok(_) => continue,
                Err(e) => panic!("event stream closed: {e}"),
            }
        }
    })
    .await
    .map_err(|_| anyhow!("bob did not claim the token transfer in time"))?;
    bob_bg.abort();
    assert_eq!(recv_amount, 250, "bob booked 250 TKN off-chain");
    assert_eq!(alice.get_token_balances().await?[0].balance, 750, "alice keeps 750 change");
    println!("SDK71 - carrier transferred off-chain (250 TKN) with no ladder involved");

    // ---- 6. The laddered coin transfers normally, on the R′ path. -------------------------------
    mercuryrustlib::transfer_sender::execute(
        &cc, &bob_address, "sdk71_alice", &plain_sid, None, false, None,
    )
    .await?;
    let mut claimed = false;
    for _ in 0..60 {
        bob.claim().await?;
        let got = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk71_bob")
            .await?
            .coins
            .iter()
            .any(|c| {
                c.statechain_id.as_deref() == Some(plain_sid.as_str())
                    && c.status == CoinStatus::CONFIRMED
            });
        if got {
            claimed = true;
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    assert!(claimed, "bob did not claim the laddered coin");
    assert!(
        mercuryrustlib::tesr::load(&cc, "sdk71_bob", &plain_sid).await?.is_some(),
        "bob's received coin carries the conveyed ladder (protocol_version 2 path)"
    );
    // [M1] The four refusals above all fired before any SE co-sign, so the coin's `num_sigs` was
    // never inflated — which is exactly why this transfer, on the same coin, just succeeded on the
    // R′ path. A refusal that bricked the cooperative path would have failed here instead.
    println!("SDK71 - laddered coin conveyed at protocol_version 2 and claimed by bob (M1: the refusals left the coin untouched)");

    println!("SDK71 - PASS: laddering is unconditional, the flat lane is carrier-only, a laddered coin can never be conveyed flat, and everything the classifier cannot explain fails CLOSED");
    Ok(())
}
