//! E2E (SDK_E2E=71) — **unconditional laddering, and NO lane for a coin without a ladder**.
//!
//! One coin type, one lane. A coin's only exit material is its TES-R ladder, established at FIRST
//! SIGHT of the funding transaction — before it confirms — and a coin that has no ladder row cannot
//! be conveyed at all. There is no flat backup chain, no absolute calendar, and no "flat lane
//! classifier": the `ladderskip-<sid>` records the ladder pass writes are DIAGNOSTIC ONLY
//! (`is_legitimate_flat_reason` is always false, `LadderSkipReason::permits_flat_conveyance` is
//! always false). Proven here on the live stack:
//!
//!   1. **the ladder exists while the deposit is still IN_MEMPOOL** — `claim()` alone, no
//!      `establish` call anywhere in this test, and no block mined between the broadcast and the
//!      assertion. The enclave count is exactly 3 (T, X, S), the census is `tiers + superseded`
//!      with a flat term of 0 (`verify_bundle(&b, 3, 0)` accepts; a flat term of 1 or a fourth
//!      co-sign is refused), and the coin carries no absolute calendar (`locktime == None`);
//!   2. a plain deposit is laddered with no opt-in and no flag, carries no skip record, and the
//!      wallet is marked ladder-managed;
//!   3. [#162] an RGB carrier is laddered too, and its ladder is COLOURED — one tier, one
//!      consignment. It carries no skip record and is never surfaced as `LadderSkipped`;
//!   3b. the skip-reason vocabulary is closed and inert: every persisted spelling round-trips through
//!      `LadderSkipReason::from_str`/`as_str` (including the new `plain-ladder-over-carrier`), and
//!      NONE of them licenses a conveyance;
//!   4. **a coin without a readable ladder is refused by `transfer_sender::execute` BEFORE any SE
//!      co-sign, whatever else is recorded about it**:
//!        4a. an unreadable `tesr-` row is refused by name ("could not read the exit ladder"), and
//!            [M2] the ladder pass RECORDS and SURFACES that state (`LadderUnreadable`) instead of
//!            skipping the coin in silence; [M3] the record is readable through the SDK
//!            (`ladder_skip_reason` / `flat_only_coins`, which reports it as NOT transferable);
//!        4b. a coin whose `tesr-` row is gone is refused by name ("has no exit ladder and cannot be
//!            conveyed"), and the SDK's payment planner excludes it as having no exit material
//!            (`quote_transfer` → `no_exit_material_coins`, `fundable == false`);
//!        4c. the refusal is the SAME whatever reason is recorded under `ladderskip-<sid>` — every
//!            spelling, transient or "permanent", is tried, and each is a deferral, not a licence;
//!        4d. the refusal is DECIDED LOCALLY from the `tesr-` row alone. The sub-cases the retired
//!            classifier used to distinguish — a funding txid the chain backend has never heard of,
//!            a funding outpoint that belongs to another coin, an unparseable `branch-`/`ctesr-`
//!            row, an unreachable coordinator, a missing `ladder-managed` marker — all produce the
//!            identical refusal, because none of them is consulted; and after all of them the SE's
//!            `num_sigs` is still exactly 3 (M1: nothing irreversible happened);
//!   5. the laddered carrier is not flat-only at all (no recorded reason, absent from
//!      `flat_only_coins`) and still pays off-chain over its COLOURED ladder;
//!   6. with its ladder restored, the plain coin conveys normally: bob books the ladder, no flat
//!      backup row, and `locktime == None` — no absolute calendar on the receiving side either.
//!
//! Run: SDK_E2E=71 ML_NETWORK=regtest cargo run   (regtest + lockbox + RGB proxy up)

use std::time::Duration;

use anyhow::{anyhow, Result};
use mercury_utexo_sdk::{LadderSkipReason, SdkConfig, UtexoWallet, WalletEvent};
use mercuryrustlib::CoinStatus;

use crate::bitcoin_core;

const PLAIN_AMOUNT: u32 = 123_456; // distinctive, so the plain coin is unambiguous vs the carrier

/// A stable fragment of the ONE refusal `transfer_sender::execute` raises for a coin with no
/// `tesr-` row. Every case in part 4b–4d must land on this exact refusal: any other error means the
/// coin got past the ladder check and something downstream tripped over it instead.
const NO_LADDER_REFUSAL: &str = "has no exit ladder and cannot be conveyed";

/// Every persisted `ladderskip-` spelling this build knows. Kept in the test rather than read from
/// the enum so that a reason ADDED to the enum without a matching constant (or vice versa) fails
/// here, not in a wallet.
const ALL_SKIP_SPELLINGS: &[&str] = &[
    mercuryrustlib::transfer_sender::FLAT_RGB_CARRIER,
    mercuryrustlib::transfer_sender::FLAT_TERMINALIZED_CARRIER,
    mercuryrustlib::transfer_sender::FLAT_RGB_STATE_UNAVAILABLE,
    mercuryrustlib::transfer_sender::FLAT_FUNDING_NOT_ONCHAIN,
    mercuryrustlib::transfer_sender::FLAT_NOT_BINDABLE,
    mercuryrustlib::transfer_sender::FLAT_COORDINATOR_UNAVAILABLE,
    mercuryrustlib::transfer_sender::FLAT_ESTABLISH_FAILED,
    mercuryrustlib::transfer_sender::FLAT_DUPLICATE_DEPOSIT,
    mercuryrustlib::transfer_sender::FLAT_LADDER_UNREADABLE,
    mercuryrustlib::transfer_sender::FLAT_FUNDING_UNRESOLVABLE,
    mercuryrustlib::transfer_sender::FLAT_ATTESTATION_UNPINNED,
    mercuryrustlib::transfer_sender::FLAT_ATTESTATION_INVALID,
    mercuryrustlib::transfer_sender::FLAT_BINDING_UNRESOLVED,
    mercuryrustlib::transfer_sender::FLAT_PLAIN_LADDER_OVER_CARRIER,
];

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

async fn raw_row(cc: &mercuryrustlib::client_config::ClientConfig, wallet: &str, key: &str) -> Option<String> {
    mercuryrustlib::sqlite_manager::get_all_backup_txs(&cc.pool, wallet)
        .await
        .ok()?
        .into_iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v)
}

/// The SE's cumulative co-sign counter for this node — the authoritative `num_sigs` the receiver's
/// census checks. Read from the coordinator, never from the wallet.
async fn num_sigs(cc: &mercuryrustlib::client_config::ClientConfig, sid: &str) -> Result<u32> {
    Ok(mercuryrustlib::utils::get_statechain_info(sid, cc)
        .await?
        .ok_or(anyhow!("no statechain_info for {sid}"))?
        .num_sigs)
}

/// The index-0 coin for `sid` as the wallet record currently holds it.
async fn coin_of(
    cc: &mercuryrustlib::client_config::ClientConfig,
    wallet: &str,
    sid: &str,
) -> Result<mercuryrustlib::Coin> {
    mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, wallet)
        .await?
        .coins
        .into_iter()
        .find(|c| c.statechain_id.as_deref() == Some(sid) && c.duplicate_index == 0)
        .ok_or(anyhow!("coin {sid} not found in wallet {wallet}"))
}

/// Point the index-0 coin's funding outpoint somewhere else (and back), in the wallet record.
async fn set_funding(
    cc: &mercuryrustlib::client_config::ClientConfig,
    wallet: &str,
    sid: &str,
    txid: Option<String>,
    vout: Option<u32>,
) -> Result<()> {
    let mut w = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, wallet).await?;
    let mut hit = false;
    for c in w.coins.iter_mut() {
        if c.statechain_id.as_deref() == Some(sid) && c.duplicate_index == 0 {
            c.utxo_txid = txid.clone();
            c.utxo_vout = vout;
            hit = true;
        }
    }
    if !hit {
        return Err(anyhow!("coin {sid} not found in wallet {wallet}"));
    }
    mercuryrustlib::sqlite_manager::update_wallet(&cc.pool, &w).await?;
    Ok(())
}

/// `transfer_sender::execute` on a coin with no `tesr-` row MUST refuse with [`NO_LADDER_REFUSAL`].
/// Returns the refusal text so a caller can print it.
async fn expect_no_ladder_refusal(
    cc: &mercuryrustlib::client_config::ClientConfig,
    recipient: &str,
    sid: &str,
    label: &str,
) -> Result<String> {
    let err = mercuryrustlib::transfer_sender::execute(cc, recipient, "sdk71_alice", sid, None, false, None)
        .await
        .err()
        .map(|e| e.to_string())
        .unwrap_or_else(|| {
            panic!(
                "[{label}] REGRESSION: a coin with NO exit ladder was CONVEYED. There is no lane \
                 for such a coin — the receiver's census cannot balance it and it has no exit to \
                 inherit."
            )
        });
    assert!(
        err.contains(NO_LADDER_REFUSAL),
        "[{label}] the coin got past the ladder check and failed elsewhere instead (or was refused \
         for a reason that no longer exists): {err}"
    );
    Ok(err)
}

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    for d in ["./rgb-data-sdk71_alice", "./rgb-data-sdk71_bob"] {
        let _ = std::fs::remove_dir_all(d);
    }
    std::env::set_var("ML_NETWORK", "regtest");

    // `mut` so part 4d can point `statechain_entity` at a dead port and prove the refusal needs no
    // coordinator. Restored immediately afterwards.
    let mut cc = mercuryrustlib::client_config::load().await;
    let mut alice_cfg = SdkConfig::regtest("sdk71_alice");
    alice_cfg.rgb_data_dir = Some("./rgb-data-sdk71_alice".to_string());
    let mut bob_cfg = SdkConfig::regtest("sdk71_bob");
    bob_cfg.rgb_data_dir = Some("./rgb-data-sdk71_bob".to_string());

    let (alice, _) = UtexoWallet::initialize(alice_cfg, None).await?;
    let (bob, _) = UtexoWallet::initialize(bob_cfg, None).await?;
    let bob_address = bob.get_utexo_address().await?;
    let mut alice_events = alice.subscribe();
    let mut seen: Vec<WalletEvent> = Vec::new();

    // ---- 1. THE LADDER EXISTS WHILE THE DEPOSIT IS STILL IN THE MEMPOOL. ------------------------
    //
    // Broadcast the plain deposit and do NOT mine. `claim()` is the only thing that runs: its
    // `update_coins_ex(.., Defer)` books the coin IN_MEMPOOL at first sighting, and the establish
    // pass in the SAME call gives it its ladder. The old shape co-signed a flat `tx1` here and left
    // the ladder for after confirmation; that lane no longer exists, so the assertion is made
    // before a single block is mined — where the old code could not have passed it.
    let t = prepaid_token(&cc).await?;
    alice.add_prepaid_token(&t).await;
    let plain_addr = alice.get_deposit_address(PLAIN_AMOUNT as u64).await?;
    bitcoin_core::sendtoaddress(PLAIN_AMOUNT, &plain_addr)?;

    let mut mempool_laddered: Option<(String, mercuryrustlib::tesr::TesrBundle)> = None;
    for _ in 0..45 {
        alice.claim().await?;
        seen.extend(drain(&mut alice_events));
        let coins = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk71_alice").await?.coins;
        let Some(c) = coins.iter().find(|c| c.aggregated_address.as_deref() == Some(plain_addr.as_str())) else {
            tokio::time::sleep(Duration::from_secs(2)).await; // electrs mempool indexing
            continue;
        };
        if c.status == CoinStatus::INITIALISED {
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        }
        // No block has been mined since the broadcast, so the ONLY status the coin may hold here is
        // IN_MEMPOOL. Anything else means the pre-confirmation property was not measured.
        assert_eq!(
            c.status,
            CoinStatus::IN_MEMPOOL,
            "the plain deposit was booked as {:?} before any block was mined — this run cannot \
             measure the mempool-time ladder",
            c.status
        );
        let sid = c.statechain_id.clone().ok_or(anyhow!("IN_MEMPOOL coin has no statechain id"))?;
        if let Some(b) = mercuryrustlib::tesr::load(&cc, "sdk71_alice", &sid).await? {
            assert!(c.locktime.is_none(), "an IN_MEMPOOL laddered coin carries no absolute calendar: {:?}", c.locktime);
            mempool_laddered = Some((sid, b));
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let (plain_sid, mempool_bundle) = mempool_laddered.ok_or(anyhow!(
        "the plain deposit was never laddered while IN_MEMPOOL: the ladder must be established at \
         FIRST SIGHT of the funding tx, before confirmation"
    ))?;
    assert!(!mempool_bundle.is_colored(), "a plain deposit gets a PLAIN ladder");
    assert_eq!(
        mempool_bundle.exit_tiers().len(),
        3,
        "a fresh ladder is exactly T, X_0, S_0"
    );
    assert!(
        mempool_bundle.superseded_states.is_empty() && mempool_bundle.superseded_extensions.is_empty(),
        "nothing is superseded on a fresh ladder"
    );
    // THE CENSUS. The enclave co-signed exactly the three tiers and nothing else — in particular no
    // flat `tx1` — so the count is 3 and the flat term is 0. A flat term of 1 (the old baseline) or
    // a fourth co-sign must be refused by the same verifier the receiver runs.
    let ns_mempool = num_sigs(&cc, &plain_sid).await?;
    assert_eq!(ns_mempool, 3, "the enclave count after deposit is exactly 3 (T, X, S) — no flat tx1");
    mercuryrustlib::tesr::verify_bundle(&mempool_bundle, 3, 0)
        .map_err(|e| anyhow!("verify_bundle(3, 0) rejected the deposit-time ladder: {e}"))?;
    assert!(
        mercuryrustlib::tesr::verify_bundle(&mempool_bundle, 3, 1).is_err(),
        "a flat term of 1 must NOT balance: there is no flat backup to count"
    );
    assert!(
        mercuryrustlib::tesr::verify_bundle(&mempool_bundle, 4, 0).is_err(),
        "a hidden fourth co-sign must be refused"
    );
    println!(
        "SDK71 - plain deposit {plain_sid} laddered while IN_MEMPOOL: 3 tiers, num_sigs == 3, \
         census (3, 0) balances and (3, 1) / (4, 0) do not, locktime == None"
    );

    // Now the RGB issuance carrier, and confirmations for both.
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
    assert_eq!(plain.statechain_id.as_deref(), Some(plain_sid.as_str()), "the confirmed plain coin is the mempool-laddered one");
    assert!(plain.locktime.is_none(), "a confirmed laddered coin still carries no absolute calendar: {:?}", plain.locktime);
    let carriers: Vec<_> = confirmed.iter().filter(|c| c.amount != Some(PLAIN_AMOUNT)).collect();
    assert!(!carriers.is_empty(), "the issuance produced a carrier coin");

    // ---- 2. The plain deposit is laddered with NO opt-in, and confirmation added nothing. --------
    let confirmed_bundle = mercuryrustlib::tesr::load(&cc, "sdk71_alice", &plain_sid)
        .await?
        .ok_or(anyhow!("a plain deposit must be laddered by claim() alone (unconditional laddering)"))?;
    assert_eq!(
        confirmed_bundle.exit_tiers().len(),
        3,
        "confirmation must not add tiers — the ladder was complete at first sight"
    );
    assert_eq!(
        num_sigs(&cc, &plain_sid).await?,
        3,
        "confirmation must not add co-signs — the ladder pass is idempotent on a laddered coin"
    );
    assert!(
        seen.iter().any(|e| matches!(e, WalletEvent::LadderEstablished { statechain_id } if *statechain_id == plain_sid)),
        "LadderEstablished was not emitted for the plain coin"
    );
    // No stale skip record survives on a laddered coin.
    assert!(
        mercuryrustlib::transfer_sender::read_ladder_skip(&cc, "sdk71_alice", &plain_sid, 0).await.is_none(),
        "a laddered coin must carry no skip record"
    );
    assert!(
        mercuryrustlib::transfer_sender::is_ladder_managed(&cc, "sdk71_alice").await,
        "claim() must mark the wallet ladder-managed"
    );
    println!("SDK71 - plain deposit {plain_sid} laddered by claim() alone; confirmation added no tier and no co-sign");

    // ---- 3. The carrier is laddered TOO, and its ladder is COLOURED. ---------------------------
    //
    // [#162] This section asserted the OPPOSITE until the one-coin-shape flip: that an RGB carrier
    // is left un-laddered under a "terminal freeze", recorded as `rgb-carrier` and surfaced as
    // `LadderSkipped`. A carrier is laddered like any other coin, every tier coloured — the property
    // worth pinning is that a carrier reaches the same place a plain coin does.
    for c in &carriers {
        let sid = c.statechain_id.clone().ok_or(anyhow!("carrier has no sid"))?;
        // The coloured half attaches tier by tier, so poll claim() instead of asserting on the
        // first pass — a slow attach and a missing one are different failures.
        let mut colored = None;
        for _ in 0..40 {
            if let Some(b) = mercuryrustlib::tesr::load(&cc, "sdk71_alice", &sid).await? {
                if b.rgb.is_some() {
                    colored = Some(b);
                    break;
                }
            }
            alice.claim().await?;
        }
        let bundle =
            colored.unwrap_or_else(|| panic!("the RGB carrier {sid} never got a COLOURED ladder"));
        let rgb = bundle.rgb.clone().expect("the coloured half was just checked");
        assert_eq!(
            rgb.consignments.len(),
            bundle.exit_tiers().len(),
            "carrier {sid}: every tier of a coloured ladder carries a consignment"
        );
        assert!(c.locktime.is_none(), "a laddered carrier {sid} carries no absolute calendar: {:?}", c.locktime);
        // A laddered coin carries no skip record — the same invariant part 2 asserts for the plain
        // deposit. In particular no `plain-ladder-over-carrier`: the carrier was coloured at
        // first sight, not plain-laddered and discovered to be a carrier later.
        assert!(
            mercuryrustlib::transfer_sender::read_ladder_skip(&cc, "sdk71_alice", &sid, 0).await.is_none(),
            "a laddered carrier {sid} must carry no skip record"
        );
        assert!(
            seen.iter().any(|e| matches!(e, WalletEvent::LadderEstablished { statechain_id } if *statechain_id == sid)),
            "LadderEstablished was not emitted for carrier {sid}"
        );
        assert!(
            !seen.iter().any(|e| matches!(e, WalletEvent::LadderSkipped { statechain_id, .. } if *statechain_id == sid)),
            "carrier {sid} was surfaced as LadderSkipped — under one coin shape a carrier is laddered, \
             so telling the app it is un-laddered is the stale two-shape behaviour"
        );
    }
    println!(
        "SDK71 - {} carrier(s) laddered with a COLOURED ladder, no skip record, no LadderSkipped",
        carriers.len()
    );

    // ---- 3b. The skip-reason vocabulary is closed and INERT. ------------------------------------
    //
    // Every persisted spelling must parse back to exactly one variant (a reason the enum knows but
    // `from_str` does not would be read as "flat-only for an unknown reason" forever), and NO
    // reason may license a conveyance: the records are diagnostic, and the conveyance path never
    // reads them.
    for spelling in ALL_SKIP_SPELLINGS {
        let reason = LadderSkipReason::from_str(spelling)
            .unwrap_or_else(|| panic!("`{spelling}` is a persisted skip spelling this build cannot parse back"));
        assert_eq!(reason.as_str(), *spelling, "as_str/from_str must round-trip `{spelling}`");
        assert!(
            !reason.permits_flat_conveyance(),
            "`{spelling}` ({reason:?}) must never license a conveyance — there is no un-laddered lane"
        );
        assert!(
            !mercuryrustlib::transfer_sender::is_legitimate_flat_reason(spelling),
            "`{spelling}` must never be a legitimate reason to convey without a ladder"
        );
    }
    assert_eq!(
        LadderSkipReason::from_str(mercuryrustlib::transfer_sender::FLAT_PLAIN_LADDER_OVER_CARRIER),
        Some(LadderSkipReason::PlainLadderOverCarrier),
        "the new `plain-ladder-over-carrier` spelling must parse to its variant"
    );
    assert_eq!(
        LadderSkipReason::from_str("plain-ladder-over-carrier"),
        Some(LadderSkipReason::PlainLadderOverCarrier),
        "the persisted spelling of PlainLadderOverCarrier is `plain-ladder-over-carrier`"
    );
    assert_eq!(LadderSkipReason::from_str("no-such-reason"), None, "an unknown spelling parses to None, never to a variant");
    assert!(
        !mercuryrustlib::transfer_sender::is_legitimate_flat_reason("no-such-reason"),
        "an unknown spelling licenses nothing either"
    );
    println!(
        "SDK71 - {} skip spellings round-trip through LadderSkipReason (incl. plain-ladder-over-carrier); none licenses a conveyance",
        ALL_SKIP_SPELLINGS.len()
    );

    // ---- 4a. An unreadable ladder is REFUSED by name, recorded (M2) and readable (M3). ----------
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
    .expect_err("an unreadable ladder must refuse the transfer — a ladder we cannot read is not a ladder we may assume away")
    .to_string();
    assert!(
        err.contains("could not read the exit ladder"),
        "unexpected refusal: {err}"
    );
    // [M2] The ladder pass must RECORD and SURFACE the unreadable row rather than skipping the coin
    // in silence — the coin is untransferable until the row is restored, and the owner must hear.
    let mut m2_events = alice.subscribe();
    alice.claim().await?;
    let m2_seen = drain(&mut m2_events);
    assert_eq!(
        mercuryrustlib::transfer_sender::read_ladder_skip(&cc, "sdk71_alice", &plain_sid, 0).await.as_deref(),
        Some(mercuryrustlib::transfer_sender::FLAT_LADDER_UNREADABLE),
        "[M2] an unreadable tesr- row must be recorded as a skip reason"
    );
    assert!(
        m2_seen.iter().any(|e| matches!(
            e,
            WalletEvent::LadderSkipped { statechain_id, reason }
                if *statechain_id == plain_sid && *reason == LadderSkipReason::LadderUnreadable
        )),
        "[M2] LadderSkipped{{LadderUnreadable}} was not emitted: {m2_seen:?}"
    );
    // The pass must NOT have established a second ladder over the unreadable row.
    assert_eq!(
        num_sigs(&cc, &plain_sid).await?,
        3,
        "[M2] the ladder pass must not co-sign a second ladder over an unreadable row"
    );
    // [M3] and it is readable through the SDK, without having caught the one-shot event.
    assert_eq!(
        alice.ladder_skip_reason(&plain_sid).await,
        Some(LadderSkipReason::LadderUnreadable),
        "[M3] the SDK accessor must report the persisted reason"
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
    assert!(
        flat.iter().all(|(_, _, transferable)| !*transferable),
        "[M3] no recorded reason makes a coin transferable — there is no lane for one: {flat:?}"
    );
    println!("SDK71 - unreadable ladder refused by name, recorded (M2) and readable via the SDK (M3), no second ladder co-signed");

    // ---- 4b. A coin whose ladder row is GONE has no lane at all. --------------------------------
    mercuryrustlib::transfer_sender::delete_raw_backup_row(&cc, "sdk71_alice", &tesr_key).await?;
    let skip_key = mercuryrustlib::transfer_sender::ladder_skip_key(&plain_sid, 0);
    mercuryrustlib::transfer_sender::delete_raw_backup_row(&cc, "sdk71_alice", &skip_key).await?;
    let err = expect_no_ladder_refusal(&cc, &bob_address, &plain_sid, "4b bare").await?;
    println!("SDK71 - a coin with no tesr- row refused: {}", err.lines().next().unwrap_or(err.as_str()));
    // The SDK's payment planner draws the same conclusion BEFORE a send is attempted: the coin has
    // no exit material on any lane, so it is excluded and named, rather than offered and refused
    // at the far end of the payment.
    let quote = alice
        .quote_transfer(PLAIN_AMOUNT as u64)
        .await
        .map_err(|e| anyhow!("quote_transfer failed on a wallet holding a coin with no exit material: {e}"))?;
    assert!(
        quote.no_exit_material_coins.iter().any(|s| *s == plain_sid),
        "[#145] the planner must name the coin as having NO exit material: {quote:?}"
    );
    assert!(
        !quote.fundable,
        "[#145] a coin with no exit material must not be counted as fundable: {quote:?}"
    );
    println!("SDK71 - the planner excludes the coin as having no exit material (fundable == false)");

    // ---- 4c. The refusal is the SAME whatever reason is recorded. ------------------------------
    //
    // Under the retired classifier a recorded reason could LICENSE a flat conveyance (and [B1] a
    // transient one once did, forever). Now the conveyance path never reads the record: every
    // spelling — transient, "permanent", the new plain-ladder-over-carrier — lands on the identical
    // refusal, and the record survives the refusal (it is a persisted diagnostic, not state the
    // refusal consumes).
    for spelling in ALL_SKIP_SPELLINGS {
        mercuryrustlib::sqlite_manager::insert_raw_backup_txs(
            &cc.pool,
            "sdk71_alice",
            &skip_key,
            &format!("{{\"reason\":\"{spelling}\",\"at\":\"2026-07-29T00:00:00Z\"}}"),
        )
        .await?;
        expect_no_ladder_refusal(&cc, &bob_address, &plain_sid, &format!("4c recorded '{spelling}'")).await?;
        assert_eq!(
            mercuryrustlib::transfer_sender::read_ladder_skip(&cc, "sdk71_alice", &plain_sid, 0).await.as_deref(),
            Some(*spelling),
            "the recorded reason must survive the refusal"
        );
        assert!(
            !mercuryrustlib::transfer_sender::is_legitimate_flat_reason(spelling),
            "`{spelling}` is a DEFERRAL, not a licence"
        );
    }
    mercuryrustlib::transfer_sender::delete_raw_backup_row(&cc, "sdk71_alice", &skip_key).await?;
    println!(
        "SDK71 - all {} recorded reasons produce the same 'no exit ladder' refusal; none licenses",
        ALL_SKIP_SPELLINGS.len()
    );

    // ---- 4d. The refusal is decided LOCALLY, from the `tesr-` row alone. -----------------------
    //
    // The retired classifier distinguished these as separate sub-cases (an unresolvable funding
    // output, a mismatched outpoint, unparseable `branch-`/`ctesr-` rows, a coordinator that could
    // not be asked, a missing scope marker), and three review rounds each found a new fail-open
    // among them. There is nothing left to get wrong: none of them is consulted before the ladder
    // check, so each produces the identical refusal — and the SE's count proves none of them cost
    // a co-sign.
    let plain_coin = coin_of(&cc, "sdk71_alice", &plain_sid).await?;
    let real_txid = plain_coin.utxo_txid.clone().ok_or(anyhow!("plain coin has no funding txid"))?;
    let real_vout = plain_coin.utxo_vout;
    const NO_SUCH_TXID: &str = "1111111111111111111111111111111111111111111111111111111111111111";

    // (i) a funding txid the chain backend has never heard of.
    set_funding(&cc, "sdk71_alice", &plain_sid, Some(NO_SUCH_TXID.to_string()), real_vout).await?;
    expect_no_ladder_refusal(&cc, &bob_address, &plain_sid, "4d funding-unresolvable").await?;
    set_funding(&cc, "sdk71_alice", &plain_sid, Some(real_txid.clone()), real_vout).await?;

    // (ii) a real, on-chain outpoint that belongs to ANOTHER coin (a donor is always available: this
    //      wallet holds the carrier by now).
    let donor = mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, "sdk71_alice")
        .await?
        .coins
        .into_iter()
        .find(|c| c.utxo_txid.is_some() && c.utxo_txid.as_deref() != Some(real_txid.as_str()))
        .ok_or(anyhow!("no second funded coin to borrow an outpoint from"))?;
    set_funding(&cc, "sdk71_alice", &plain_sid, donor.utxo_txid.clone(), donor.utxo_vout).await?;
    expect_no_ladder_refusal(&cc, &bob_address, &plain_sid, "4d mismatched outpoint").await?;
    set_funding(&cc, "sdk71_alice", &plain_sid, Some(real_txid.clone()), real_vout).await?;

    // (iii) an unparseable `branch-` row and (iv) an unparseable `ctesr-` row: bytes under a retired
    //       key are not evidence of anything.
    for key in [format!("branch-{plain_sid}"), format!("ctesr-{plain_sid}")] {
        mercuryrustlib::sqlite_manager::insert_raw_backup_txs(&cc.pool, "sdk71_alice", &key, "not json").await?;
        expect_no_ladder_refusal(&cc, &bob_address, &plain_sid, &format!("4d unparseable {key}")).await?;
        mercuryrustlib::transfer_sender::delete_raw_backup_row(&cc, "sdk71_alice", &key).await?;
    }

    // (v) an unreachable coordinator (nothing listens on port 1). The refusal precedes every network
    //     round-trip, so the coordinator is never asked.
    let real_entity = cc.statechain_entity.clone();
    cc.statechain_entity = "http://127.0.0.1:1".to_string();
    let coordinator_result = expect_no_ladder_refusal(&cc, &bob_address, &plain_sid, "4d coordinator unreachable").await;
    cc.statechain_entity = real_entity;
    coordinator_result?;

    // (vi) a missing `ladder-managed` marker. The marker is a wallet-level note, not a gate: the
    //      refusal is per coin, from the coin's own row.
    mercuryrustlib::transfer_sender::delete_raw_backup_row(
        &cc, "sdk71_alice", mercuryrustlib::transfer_sender::LADDER_MANAGED_KEY,
    )
    .await?;
    assert!(
        !mercuryrustlib::transfer_sender::is_ladder_managed(&cc, "sdk71_alice").await,
        "the scope marker must actually be gone for this case to mean anything"
    );
    expect_no_ladder_refusal(&cc, &bob_address, &plain_sid, "4d no ladder-managed marker").await?;
    mercuryrustlib::transfer_sender::mark_ladder_managed(&cc, "sdk71_alice").await?;

    // [M1] Every refusal above fired BEFORE any SE co-sign — measured at the SE, not inferred.
    assert_eq!(
        num_sigs(&cc, &plain_sid).await?,
        3,
        "[M1] a refused conveyance must not cost a co-sign: the SE's count must still be exactly 3"
    );
    // And the coin is still exactly where it was: CONFIRMED, not IN_TRANSFER, no calendar.
    let after = coin_of(&cc, "sdk71_alice", &plain_sid).await?;
    assert_eq!(after.status, CoinStatus::CONFIRMED, "a refused conveyance leaves the coin CONFIRMED");
    assert!(after.locktime.is_none(), "no refusal path books a calendar on the coin");
    println!(
        "SDK71 - 6 retired classifier sub-cases (bogus txid / foreign outpoint / unparseable branch- and \
         ctesr- / dead coordinator / no scope marker) all produce the same local refusal; num_sigs still 3 (M1)"
    );

    // Restore the ladder. Part 6 conveys the coin on it.
    mercuryrustlib::sqlite_manager::insert_raw_backup_txs(&cc.pool, "sdk71_alice", &tesr_key, &saved).await?;

    // ---- 5. The carrier is not flat-only at all, and still PAYS over its COLOURED ladder. -------
    //
    // THE LEGITIMATE CASE MUST KEEP WORKING. Without this the fail-closed conveyance path could
    // "pass" by refusing every coin in the wallet, which would be a worse bug than the one it
    // closes.
    for c in &carriers {
        let sid = c.statechain_id.clone().ok_or(anyhow!("carrier has no sid"))?;
        assert!(
            !LadderSkipReason::RgbCarrier.permits_flat_conveyance(),
            "'rgb-carrier' must NOT license a conveyance — it was retired with the one-coin-shape flip"
        );
        // [M3] The SDK reports no skip reason, because there is none to report.
        assert_eq!(
            alice.ladder_skip_reason(&sid).await,
            None,
            "[M3] a laddered carrier must report no skip reason"
        );
    }
    let flat = alice.flat_only_coins().await?;
    for c in &carriers {
        let sid = c.statechain_id.clone().unwrap();
        assert!(
            !flat.iter().any(|(s, _, _)| *s == sid),
            "[M3] flat_only_coins must NOT list the laddered carrier {sid}: {flat:?}"
        );
    }
    println!(
        "SDK71 - {} carrier(s) are not flat-only at all: no recorded reason, absent from \
         flat_only_coins, and `rgb-carrier` licenses nothing",
        carriers.len()
    );

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
    println!("SDK71 - carrier paid 250 TKN off-chain over its COLOURED ladder");

    // ---- 6. With its ladder restored, the plain coin conveys normally — ladder only. -------------
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
    let bob_bundle = mercuryrustlib::tesr::load(&cc, "sdk71_bob", &plain_sid)
        .await?
        .ok_or(anyhow!("bob's received coin must carry the conveyed ladder"))?;
    assert_eq!(bob_bundle.f_txid, real_txid, "bob's ladder is rooted at the same funding tx");
    let bob_coin = coin_of(&cc, "sdk71_bob", &plain_sid).await?;
    assert!(
        bob_coin.locktime.is_none(),
        "the receiver books no absolute calendar on a conveyed coin: {:?}",
        bob_coin.locktime
    );
    // No flat backup row travels with the coin: either bob has no row under the sid at all, or an
    // empty one. `get_backup_txs` is fetch_one, so absence is an `Err` — both are the right shape.
    if let Ok(rows) = mercuryrustlib::sqlite_manager::get_backup_txs(&cc.pool, "sdk71_bob", &plain_sid).await {
        assert!(
            rows.is_empty(),
            "a laddered coin conveys `backup_transactions: []` — bob must hold no flat backup, got {}",
            rows.len()
        );
    }
    // The sender's refusals in part 4 left the coin's count untouched — which is exactly why this
    // conveyance, on the same coin, just balanced the receiver's census.
    println!("SDK71 - laddered coin conveyed and claimed by bob: ladder present, no flat backup row, locktime == None");

    println!(
        "SDK71 - PASS: the ladder exists at first mempool sighting (3 tiers, flat term 0), a coin \
         without a readable ladder has no lane whatever is recorded about it, and every refusal \
         costs no co-sign"
    );
    Ok(())
}
