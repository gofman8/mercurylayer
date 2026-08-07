//! E2E (SDK_E2E=85) — **TRANSFER CANCELLATION: withdrawing an opened transfer, and the one case
//! that must never be withdrawable.**
//!
//! **NOT RUN.** This file was authored without executing it: the orchestrator owns the single
//! regtest chain and the single test binary, and migration `0010_transfer_cancel.sql` is embedded at
//! COMPILE time by `sqlx::migrate!("./migrations")` (server/src/main.rs), so the running dev
//! container's binary does not contain it. Until the server is rebuilt and restarted,
//! `get_transfer_for_cancel` errors on the missing `cancelled_at` column and every step below fails
//! at the first `cancel_transfer`. See "Stack prerequisites".
//!
//! ## Why cancellation needs an adversarial E2E and not a happy path
//!
//! The coordinator's pending-transfer lock is LOAD-BEARING. While a transfer of a coin is open the
//! SE refuses every co-signature of that coin, and that refusal is the ONLY thing standing between a
//! conveyed-but-unclaimed receiver and a still-owner sender who co-signs a rival state. Cancellation
//! RELEASES that lock, so a sender-only cancel of a CONVEYED transfer reopens exactly the hole the
//! lock closes:
//!
//!   1. alice posts the transfer message for bob — bob now holds claimable material,
//!   2. alice cancels,
//!   3. alice opens a new transfer to carol,
//!   4. bob and carol race; the loser believes they were paid and was not.
//!
//! Every step below exists to pin one row of `mercurylib::transfer::cancel`'s authorization table
//! against a live coordinator and a live chain:
//!
//! | coordinator-observable state | test                                | authorization  | step |
//! |---|---|---|---|
//! | opened, message never posted | `encrypted_transfer_msg IS NULL`    | sender alone   | [1]  |
//! | posted, unclaimed            | msg NOT NULL, `key_updated = false` | sender AND recipient | [2],[3] |
//! | claimed                      | `key_updated = true`                | never          | [4]  |
//! | batched (lightning latch)    | `batch_id IS NOT NULL`              | never          | [5]  |
//!
//! ## The numbered steps
//!
//!  1. **ROW 1 — SENDER ALONE.** alice OPENS a transfer of coin A to bob without ever posting the
//!     mailbox message (`transfer_sender::get_new_x1`, which is the half of `execute` that talks to
//!     `POST /transfer/sender`; the message is posted by a later call `execute` makes and this test
//!     deliberately does not). The lock is asserted to be REALLY THERE first — a transfer of coin A
//!     to carol must be refused by the SE — then alice cancels ALONE, and the same transfer to carol
//!     now succeeds. Cancelling again is idempotent, not an error.
//!  2. **ROW 2, THE REFUSAL.** alice transfers coin B to bob for real (message posted, unclaimed).
//!     A sender-only cancel must be REFUSED, and refused as a
//!     `CancelNeedsRecipientConsent` carrying bob's recipient auth key programmatically — not as
//!     prose a caller has to scrape. The lock must still be in place afterwards.
//!  3. **ROW 2, THE COOPERATIVE CANCEL.** bob mints a single-use consent token for that exact key
//!     (`cancel_consent`, in bob's OWN wallet — this is the cross-wallet path, which the
//!     self-addressed case in tb05 does not reach), hands it back out of band, and alice completes
//!     with `cancel_transfer_with_consent`. Coin B is spendable again.
//!  4. **ROW 3 — CLAIMED IS TERMINAL.** alice transfers coin C to bob and bob CLAIMS it. alice's
//!     cancel must be refused permanently: a completed payment is never withdrawable, whatever
//!     consent is presented.
//!  5. **ROW 4 — BATCHED IS NEVER CANCELLABLE.** alice opens a transfer of coin D carrying a
//!     `batch_id` (the lightning latch's shape — a batched row is created by
//!     `get_new_x1(..., Some(batch_id))`, so this needs no LN daemon). Cancellation must be refused:
//!     an LN-latched transfer is governed by the latch's own clock, and recipient consent is not the
//!     right authorization for it (the preimage is).
//!  6. **[SAFETY] THE DOUBLE-PAY HOLE, CLOSED.** The whole design exists to make this impossible:
//!     after coin B's cancellation in step 3, bob must NEVER be able to claim it, and alice's
//!     re-transfer of coin B must land with carol — EXACTLY ONE of them is paid. bob claims in a
//!     loop and never books coin B; carol claims it and does.
//!
//! ## Expected coin counts, end state
//!
//! * alice: 4 deposits in, 0 spendable coins left. A -> carol (step 1), B -> carol (step 6),
//!   C -> bob (step 4), D still locked by its un-cancellable batched transfer (step 5).
//! * bob:   1 CONFIRMED coin — coin C only. Coin B was cancelled out from under him and must never
//!          appear; coin D was never conveyed to him.
//! * carol: 2 CONFIRMED coins — coin A and coin B.
//! * The step-5 cancel attempts leave NOTHING changed: coin D's transfer row keeps its lock.
//!
//! ## Stack prerequisites
//!
//! * bitcoind + electrs + mercury-server on regtest, the standard SDK_E2E stack.
//! * **The server binary must contain migration 0010.** `sqlx::migrate!` embeds migrations at build
//!   time, so the container must be REBUILT and restarted, not merely restarted:
//!   `docker compose build mercury-server` then restart it. Per the deploy note in
//!   [mainnet-audit-2026-07]: `docker cp` + `touch` + restart, **never** `--force-recreate
//!   mercury-server`. Without 0010, `POST /transfer/cancel` fail-closes to 503 and step 1 dies at
//!   the first `cancel_transfer` with a message naming `cancelled_at`.
//! * No lightning-node infrastructure is needed: step 5 constructs the batched row directly.
//!
//! Run: SDK_E2E=85 ML_NETWORK=regtest cargo +stable run

use std::time::Duration;

use anyhow::{anyhow, Result};
use mercury_utexo_sdk::{CancelNeedsRecipientConsent, CancelOutcome, SdkConfig, UtexoWallet};
use mercurylib::wallet::{Coin, CoinStatus};
use mercuryrustlib::client_config::ClientConfig;

use crate::bitcoin_core;

/// One deposit per row of the authorization table, plus the safety step's coin. Large enough that a
/// conveyance fee never makes a transfer unfundable, and all four equal so `coin_worth` cannot pick
/// the wrong one — the coins are told apart by statechain id, never by amount.
const DEPOSIT: u64 = 120_000;

async fn prepaid_token(cc: &ClientConfig) -> Result<String> {
    let token = mercuryrustlib::deposit::get_token(cc).await?;
    crate::utils::handle_token_response(cc, &token).await
}

async fn wallet(name: &str) -> Result<UtexoWallet> {
    let (w, _) = UtexoWallet::initialize(SdkConfig::regtest(name), None).await?;
    Ok(w)
}

async fn coins_of(cc: &ClientConfig, wallet_name: &str) -> Result<Vec<Coin>> {
    Ok(mercuryrustlib::sqlite_manager::get_wallet(&cc.pool, wallet_name).await?.coins)
}

/// The wallet's CONFIRMED, index-0 coin for `statechain_id`, or an error naming what was there
/// instead. Deliberately not `Option`: every caller below is asserting a state, and "no such coin"
/// read as "nothing to check" is how a safety step passes without observing anything.
async fn confirmed_coin(cc: &ClientConfig, wallet_name: &str, statechain_id: &str) -> Result<Coin> {
    coins_of(cc, wallet_name)
        .await?
        .into_iter()
        .find(|c| {
            c.statechain_id.as_deref() == Some(statechain_id)
                && c.duplicate_index == 0
                && c.status == CoinStatus::CONFIRMED
        })
        .ok_or_else(|| anyhow!("{wallet_name} has no CONFIRMED coin for statechain id {statechain_id}"))
}

/// Does this wallet hold ANY coin (any status) for `statechain_id`?
async fn holds_any(cc: &ClientConfig, wallet_name: &str, statechain_id: &str) -> Result<bool> {
    Ok(coins_of(cc, wallet_name)
        .await?
        .iter()
        .any(|c| c.statechain_id.as_deref() == Some(statechain_id)))
}

/// Assert a refusal happened AND that it is the refusal we meant. A negative step that passes on an
/// unrelated parse/network/funding error reports a safety it never observed.
fn refused_with<T: std::fmt::Debug>(r: Result<T>, attack: &str, expect: &str) -> Result<String> {
    match r {
        std::result::Result::Ok(v) => Err(anyhow!("SAFETY: {attack} was ACCEPTED, got {v:?}")),
        Err(e) => {
            let msg = e.to_string();
            if !msg.contains(expect) {
                return Err(anyhow!(
                    "{attack} was refused for the WRONG reason — expected an error containing \
                     {expect:?}, got: {msg}"
                ));
            }
            Ok(msg)
        }
    }
}

/// Convey a WHOLE coin, named by statechain id, to `recipient_address`.
///
/// `transfer_sender::execute` rather than the SDK's `transfer(addr, amount)` on purpose: a partial
/// amount takes the in-ladder SPLIT route, which mints a CHILD with a NEW statechain id. Every
/// assertion in this test is keyed on the statechain id of a specific coin — the authorization table
/// is per transfer ROW — so the id has to survive the hop, and only a whole-coin transfer preserves
/// it. The SDK is still the layer under test for cancellation itself (`cancel_transfer`,
/// `cancel_consent`, `cancel_transfer_with_consent`, `claim`).
async fn convey(
    cc: &ClientConfig,
    wallet_name: &str,
    recipient_address: &str,
    statechain_id: &str,
) -> Result<()> {
    mercuryrustlib::transfer_sender::execute(
        cc,
        recipient_address,
        wallet_name,
        statechain_id,
        None,
        false,
        None,
    )
    .await
}

/// Fund `w` with one confirmed `DEPOSIT`-sat coin and return its statechain id.
///
/// Each call adds exactly one coin, so the caller can name the four coins A..D by the order it made
/// them rather than by amount.
async fn fund_one(cc: &ClientConfig, w: &UtexoWallet, name: &str, core: &str) -> Result<String> {
    let before: Vec<String> = coins_of(cc, name)
        .await?
        .into_iter()
        .filter(|c| c.status == CoinStatus::CONFIRMED)
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
            .filter(|c| c.status == CoinStatus::CONFIRMED)
            .filter_map(|c| c.statechain_id)
            .filter(|sid| !before.contains(sid))
            .collect();
        if let Some(sid) = fresh.first() {
            return Ok(sid.clone());
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Err(anyhow!("{name} never confirmed its {DEPOSIT}-sat deposit"))
}

/// Poll `claim()` until `w` books a CONFIRMED coin for `statechain_id`.
///
/// Also asserts, on every pass, that the claim did not quietly report the transfer as CANCELLED —
/// `ClaimResult::cancelled_transfers` is the SDK's channel for a withdrawn payment, and a step that
/// waits for a receipt while the payment was being withdrawn must say so rather than time out with
/// a generic message.
async fn claim_until(
    cc: &ClientConfig,
    w: &UtexoWallet,
    name: &str,
    statechain_id: &str,
) -> Result<Coin> {
    for _ in 0..60 {
        let pass = w.claim().await?;
        if pass.cancelled_transfers.iter().any(|s| s == statechain_id) {
            return Err(anyhow!(
                "{name} was told statechain id {statechain_id} had been CANCELLED while waiting to \
                 receive it"
            ));
        }
        if let std::result::Result::Ok(coin) = confirmed_coin(cc, name, statechain_id).await {
            return Ok(coin);
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Err(anyhow!("{name} never claimed statechain id {statechain_id}"))
}

pub async fn execute() -> Result<()> {
    for f in ["wallet.db", "wallet.db-shm", "wallet.db-wal"] {
        let _ = std::fs::remove_file(f);
    }
    std::env::set_var("ML_NETWORK", "regtest");
    let cc = mercuryrustlib::client_config::load().await;
    let core = bitcoin_core::getnewaddress()?;

    let alice = wallet("sdk85_alice").await?;
    let bob = wallet("sdk85_bob").await?;
    let carol = wallet("sdk85_carol").await?;

    let coin_a = fund_one(&cc, &alice, "sdk85_alice", &core).await?;
    let coin_b = fund_one(&cc, &alice, "sdk85_alice", &core).await?;
    let coin_c = fund_one(&cc, &alice, "sdk85_alice", &core).await?;
    let coin_d = fund_one(&cc, &alice, "sdk85_alice", &core).await?;

    // =============================================================================================
    // [1] ROW 1 — OPENED, NEVER CONVEYED: THE SENDER ALONE MAY WITHDRAW IT.
    //
    // `get_new_x1` is the half of `transfer_sender::execute` that talks to POST /transfer/sender. It
    // creates the `statechain_transfer` row — and therefore the pending-transfer lock — with
    // `encrypted_transfer_msg` still NULL, because posting the message is a LATER call that this
    // step deliberately never makes. That is row 1 exactly as the coordinator observes it.
    //
    // Row 1 is sound RELATIVE TO THE PROTOCOL, not physically: alice could always hand bob the
    // message out of band and the coordinator cannot see that. A CONFORMING recipient learns of a
    // transfer only by polling `get_msg_addr` and finds nothing there, so no conforming recipient is
    // defrauded — and cancellation only shortens the pre-existing one-hour expiry to now.
    // =============================================================================================
    let bob_slot_a = mercuryrustlib::transfer_receiver::new_transfer_address(&cc, "sdk85_bob").await?;
    let (_, _, bob_auth_a) = mercurylib::decode_transfer_address(&bob_slot_a)?;

    let coin = confirmed_coin(&cc, "sdk85_alice", &coin_a).await?;
    let signed_statechain_id = coin
        .signed_statechain_id
        .clone()
        .ok_or_else(|| anyhow!("coin A carries no signed statechain id"))?;

    mercuryrustlib::transfer_sender::get_new_x1(
        &cc,
        &coin_a,
        &signed_statechain_id,
        &bob_auth_a.to_string(),
        None,
    )
    .await?;

    // Assert the lock is REALLY THERE before asserting it can be released. Without this, a cancel
    // that did nothing at all would still make the next transfer succeed and the step would pass.
    let carol_slot_a = mercuryrustlib::transfer_receiver::new_transfer_address(&cc, "sdk85_carol").await?;
    refused_with(
        convey(&cc, "sdk85_alice", &carol_slot_a, &coin_a).await,
        "[1] transferring a coin that already has an OPEN transfer",
        "coin has an open transfer",
    )?;

    // Row 1: sender alone. No recipient co-signature is presented and none is required.
    let outcome = alice.cancel_transfer(&coin_a).await?;
    if outcome != CancelOutcome::Cancelled {
        return Err(anyhow!("[1] expected a fresh cancellation, got {outcome:?}"));
    }

    // Idempotent: cancellation is irreversible, so a client retrying after a dropped response must
    // not be told something different the second time.
    let again = alice.cancel_transfer(&coin_a).await?;
    if again != CancelOutcome::AlreadyCancelled {
        return Err(anyhow!("[1] a repeated cancel must be idempotent, got {again:?}"));
    }

    // The lock is released: the coin is spendable again, to a DIFFERENT recipient.
    convey(&cc, "sdk85_alice", &carol_slot_a, &coin_a).await?;
    claim_until(&cc, &carol, "sdk85_carol", &coin_a).await?;

    // =============================================================================================
    // [2] ROW 2, THE REFUSAL — CONVEYED AND UNCLAIMED: THE SENDER ALONE MUST NOT BE ABLE TO WITHDRAW.
    //
    // This is the two-victim break. The coordinator cannot distinguish "bob has not downloaded the
    // message" from "bob downloaded it and is about to claim": `GET /transfer/get_msg_addr/<key>` is
    // unauthenticated and non-destructive, so a `retrieved_at` column would be settable or
    // withholdable by anyone who knows the (non-secret) receiver key.
    // =============================================================================================
    let bob_slot_b = mercuryrustlib::transfer_receiver::new_transfer_address(&cc, "sdk85_bob").await?;
    let (_, _, bob_auth_b) = mercurylib::decode_transfer_address(&bob_slot_b)?;
    convey(&cc, "sdk85_alice", &bob_slot_b, &coin_b).await?;

    let refusal = alice.cancel_transfer(&coin_b).await;
    let err = match refusal {
        std::result::Result::Ok(v) => {
            return Err(anyhow!(
                "SAFETY: [2] a sender-only cancel of a CONVEYED transfer was ACCEPTED ({v:?}) — bob \
                 holds claimable material and alice could now convey the same coin to carol"
            ))
        }
        Err(e) => e,
    };

    // TYPED, not prose. A cross-wallet caller has to be able to route the consent request without
    // scraping a string, and the key it routes to must be the one the COORDINATOR recorded.
    let needs = err.downcast_ref::<CancelNeedsRecipientConsent>().ok_or_else(|| {
        anyhow!("[2] the consent refusal must be a typed error; got: {err}")
    })?;
    if needs.recipient_auth_pub_key.as_deref() != Some(bob_auth_b.to_string().as_str()) {
        return Err(anyhow!(
            "[2] the refusal named recipient key {:?}, expected bob's slot key {}",
            needs.recipient_auth_pub_key,
            bob_auth_b
        ));
    }
    if !needs.to_string().contains("the recipient must co-sign the cancellation") {
        return Err(anyhow!("[2] the refusal lost the coordinator's named reason: {needs}"));
    }

    // ...and the refusal left the lock in place: alice still cannot convey coin B to anyone else.
    let carol_slot_b = mercuryrustlib::transfer_receiver::new_transfer_address(&cc, "sdk85_carol").await?;
    refused_with(
        convey(&cc, "sdk85_alice", &carol_slot_b, &coin_b).await,
        "[2] conveying a coin whose cancel was REFUSED",
        "coin has an open transfer",
    )?;

    // =============================================================================================
    // [3] ROW 2, THE COOPERATIVE CANCEL — THE ONLY PARTY WHO CAN BE DEFRAUDED SIGNS THE RELEASE.
    //
    // CROSS-WALLET, which is the case tb05 cannot reach: bob is a different wallet, so the consent
    // has to be minted in bob's wallet and carried to alice out of band. The token is single-use and
    // endpoint-bound (`sha256(nonce|"transfer/cancel/recipient")`), so alice cannot bank it and
    // replay it against a later transfer to the same key.
    // =============================================================================================
    let consent = bob
        .cancel_consent(&coin_b, &bob_auth_b.to_string())
        .await?;

    let outcome = alice
        .cancel_transfer_with_consent(&coin_b, &bob_auth_b.to_string(), &consent)
        .await?;
    if outcome != CancelOutcome::Cancelled {
        return Err(anyhow!("[3] a consented cancel must release the lock, got {outcome:?}"));
    }

    // =============================================================================================
    // [4] ROW 3 — CLAIMED IS TERMINAL. A completed payment is never withdrawable, whatever consent
    // is presented: the enclave has already rotated its share, and there is nothing to roll back to.
    // =============================================================================================
    let bob_slot_c = mercuryrustlib::transfer_receiver::new_transfer_address(&cc, "sdk85_bob").await?;
    let (_, _, bob_auth_c) = mercurylib::decode_transfer_address(&bob_slot_c)?;
    convey(&cc, "sdk85_alice", &bob_slot_c, &coin_c).await?;
    claim_until(&cc, &bob, "sdk85_bob", &coin_c).await?;

    refused_with(
        alice.cancel_transfer(&coin_c).await,
        "[4] cancelling a transfer bob has already CLAIMED",
        "already claimed",
    )?;

    // ...and not even bob's own consent can un-complete it. `key_updated` is tested before every
    // other guard in `decide_transfer_cancel` precisely so no later state can reopen a finished
    // payment.
    match bob.cancel_consent(&coin_c, &bob_auth_c.to_string()).await {
        std::result::Result::Ok(consent) => {
            refused_with(
                alice
                    .cancel_transfer_with_consent(&coin_c, &bob_auth_c.to_string(), &consent)
                    .await,
                "[4] cancelling a CLAIMED transfer WITH the recipient's consent",
                "already claimed",
            )?;
        }
        // bob's receiving slot became the coin itself on claim, so minting a consent for the old
        // slot key may legitimately fail here. That is not the property under test — the refusal
        // above is — so a failure to mint is not a failure of the step.
        Err(_) => {}
    }

    // =============================================================================================
    // [5] ROW 4 — BATCHED (LIGHTNING LATCH) IS NEVER CANCELLABLE.
    //
    // A batched transfer is governed by the latch's own clock: cancelling would race the SSP's
    // preimage settlement, and recipient consent does not help, because the LATCH — not the
    // receiver — decides whether the payment happened. Constructed directly with a fresh batch id,
    // so this needs no lightning daemon: what `decide_transfer_cancel` reads is `batch_id IS NOT
    // NULL`, and that is exactly what this creates.
    // =============================================================================================
    let bob_slot_d = mercuryrustlib::transfer_receiver::new_transfer_address(&cc, "sdk85_bob").await?;
    let (_, _, bob_auth_d) = mercurylib::decode_transfer_address(&bob_slot_d)?;
    let coin = confirmed_coin(&cc, "sdk85_alice", &coin_d).await?;
    let signed_statechain_id = coin
        .signed_statechain_id
        .clone()
        .ok_or_else(|| anyhow!("coin D carries no signed statechain id"))?;
    let batch_id = format!("sdk85-batch-{}", &coin_d[..8.min(coin_d.len())]);

    mercuryrustlib::transfer_sender::get_new_x1(
        &cc,
        &coin_d,
        &signed_statechain_id,
        &bob_auth_d.to_string(),
        Some(batch_id),
    )
    .await?;

    refused_with(
        alice.cancel_transfer(&coin_d).await,
        "[5] cancelling a BATCHED (lightning-latch) transfer",
        "batched (lightning-latch) transfer cannot be cancelled",
    )?;

    // Consent is not the right authorization for a latch, so presenting one changes nothing. Row 4
    // is tested BEFORE the consent rule in `decide_transfer_cancel` for exactly this reason.
    let consent = bob.cancel_consent(&coin_d, &bob_auth_d.to_string()).await?;
    refused_with(
        alice
            .cancel_transfer_with_consent(&coin_d, &bob_auth_d.to_string(), &consent)
            .await,
        "[5] cancelling a BATCHED transfer WITH the recipient's consent",
        "batched (lightning-latch) transfer cannot be cancelled",
    )?;

    // =============================================================================================
    // [6] THE SAFETY ASSERTION — THE DOUBLE-PAY HOLE, CLOSED.
    //
    // Coin B was conveyed to bob and then cancelled WITH bob's consent (step 3). bob must never be
    // able to claim it, and alice's re-conveyance must land with carol. EXACTLY ONE of them ends up
    // paid — which is the entire reason the consent rule exists.
    // =============================================================================================
    for _ in 0..15 {
        let pass = bob.claim().await?;
        if pass.confirmed_deposits.iter().any(|d| d.statechain_id.as_deref() == Some(&coin_b)) {
            return Err(anyhow!("SAFETY: [6] bob booked a deposit for the CANCELLED coin B"));
        }
        if confirmed_coin(&cc, "sdk85_bob", &coin_b).await.is_ok() {
            return Err(anyhow!(
                "SAFETY: [6] bob CLAIMED statechain id {coin_b} after its transfer was cancelled — \
                 both bob and carol can now be paid from one coin"
            ));
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
    }

    // The other half of "exactly one": the coin really did go somewhere.
    convey(&cc, "sdk85_alice", &carol_slot_b, &coin_b).await?;
    claim_until(&cc, &carol, "sdk85_carol", &coin_b).await?;

    if holds_any(&cc, "sdk85_bob", &coin_b).await? {
        return Err(anyhow!(
            "SAFETY: [6] bob still holds material for coin B after carol was paid with it"
        ));
    }

    // End state, as declared in the module doc: bob holds coin C only; carol holds A and B.
    confirmed_coin(&cc, "sdk85_bob", &coin_c).await?;
    confirmed_coin(&cc, "sdk85_carol", &coin_a).await?;
    confirmed_coin(&cc, "sdk85_carol", &coin_b).await?;

    println!("sdk85_transfer_cancel: OK");
    Ok(())
}
