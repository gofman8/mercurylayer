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
//! …and steps [7]-[8] attack the recipient's consent itself, which is the only thing row 2 rests on.
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
//!  7. **[ATTACK] MISDIRECTION — THE RECIPIENT MUST NOT SIGN BLIND.** bob is receiving TWO coins of
//!     DIFFERENT value on different slots: E (`SMALL`) and F (`LARGE`). alice describes the small
//!     one and names the large one. **What this test asserts is the second of the prompt's two
//!     alternatives: the preview shows the TRUE amount, so the mismatch is visible.** The stronger
//!     statement is structural and is asserted here as well — bob enumerates his OWN mailbox with
//!     `preview_all_cancellable_consents`, so alice names nothing at all, and the token bob mints is
//!     minted FROM a previewed object whose fields are private and all derived from one decrypted
//!     message. There is no arrangement of this API in which the amount bob is shown belongs to one
//!     transfer and the signature to another; the unit-level census
//!     `a_previewed_consent_cannot_be_assembled_field_by_field` pins that a caller cannot rebuild
//!     the object field by field.
//!  8. **[ATTACK] REPLAY — A CONSENT RELEASES ONE TRANSFER INSTANCE, NOT A COIN.** bob consents to
//!     cancelling E. alice WITHHOLDS the token and tries to get a second live transfer of E to the
//!     SAME key to spend it against. Every route is asserted shut, and then the token is asserted
//!     still good for the transfer it was actually given for:
//!       * cross-instance: presenting E's token against coin F's cancellation is refused
//!         (`recipient consent is bound to different transfer material`);
//!       * that refusal does NOT burn bob's nonce — the very next step spends the same token
//!         successfully on E, which is the property `decide_transfer_cancel` orders its guards to
//!         preserve;
//!       * re-opening the row: `get_new_x1` for E to bob's same key is refused by the re-address
//!         guard (`Transfer message already exists…`);
//!       * re-conveying: blocked earlier still, by the SE's pending-transfer lock at co-sign time;
//!       * after the cancellation: refused again, by the cancelled-recipient-key guard.
//!     Then E goes to carol and bob can never claim it — the step-6 property, for the attacked coin.
//!
//! ## Expected coin counts, end state
//!
//! * alice: 6 deposits in (A..D at `DEPOSIT`, E at `SMALL`, F at `LARGE`), 0 spendable coins left.
//!   A -> carol (step 1), B -> carol (step 6), C -> bob (step 4), D still locked by its
//!   un-cancellable batched transfer (step 5), E -> carol (step 8), F -> bob (step 8).
//! * bob:   2 CONFIRMED coins — C and F. Coin B was cancelled out from under him and coin E was
//!          cancelled after he consented; neither may ever appear. Coin D was never conveyed to him.
//! * carol: 3 CONFIRMED coins — A, B and E.
//! * The step-5 cancel attempts leave NOTHING changed: coin D's transfer row keeps its lock.
//! * Coin F is never cancelled: the misdirected consent is refused, and F completes normally.
//!
//! ## Stack prerequisites
//!
//! * bitcoind + electrs + mercury-server on regtest, the standard SDK_E2E stack.
//! * **The server must also carry the consent-binding work** (`recipient_transfer_digest`, the
//!   `recipient_consent_stale` decision) and the NULL-aware re-address guard
//!   (`batch_id IS NOT DISTINCT FROM $3`). Both are source changes with no migration, so a rebuild
//!   and restart is enough — but a container older than them fails step 8: the cross-instance
//!   presentation would be answered `recipient_signature_invalid` instead of stale, and the
//!   `get_new_x1` re-open would be ACCEPTED rather than refused.
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

/// Steps [7]-[8] need bob to hold TWO transfers of OBVIOUSLY different value at the same time, so
/// that "consent to cancelling the small one" while naming the large one is a describable attack.
/// A 4x gap, both comfortably fundable.
const SMALL: u64 = 120_000;
const LARGE: u64 = 480_000;

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
    // Progress logging, because this test had none and its failures arrive as bare `Error:` lines
    // with no indication of WHICH of the eleven conveyances raised them. Every step below either
    // conveys or refuses a conveyance, so labelling this one call is enough to localise any of them.
    println!("SDK85 - convey: {wallet_name} -> {} (coin {statechain_id})",
             &recipient_address[..recipient_address.len().min(24)]);
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
/// Each call adds exactly one coin, so the caller can name the coins A..F by the order it made them
/// rather than by amount.
async fn fund_one(cc: &ClientConfig, w: &UtexoWallet, name: &str, core: &str) -> Result<String> {
    fund_one_of(cc, w, name, core, DEPOSIT).await
}

/// [7]/[8] `fund_one` for a chosen amount. Coins E and F must differ in value for the misdirection
/// attack to have a "small one" and a "large one" to lie about.
async fn fund_one_of(
    cc: &ClientConfig,
    w: &UtexoWallet,
    name: &str,
    core: &str,
    deposit: u64,
) -> Result<String> {
    let before: Vec<String> = coins_of(cc, name)
        .await?
        .into_iter()
        .filter(|c| c.status == CoinStatus::CONFIRMED)
        .filter_map(|c| c.statechain_id)
        .collect();

    let t = prepaid_token(cc).await?;
    w.add_prepaid_token(&t).await;
    let addr = w.get_deposit_address(deposit).await?;
    bitcoin_core::sendtoaddress(u32::try_from(deposit)?, &addr)?;
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
    Err(anyhow!("{name} never confirmed its {deposit}-sat deposit"))
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
    // [7]/[8]: the two coins of different value the misdirection attack needs.
    let coin_e = fund_one_of(&cc, &alice, "sdk85_alice", &core, SMALL).await?;
    let coin_f = fund_one_of(&cc, &alice, "sdk85_alice", &core, LARGE).await?;

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
        // NOT "coin has an open transfer" any more, and the difference is the point. At [1] the
        // coordinator's pending-transfer lock is what refuses. HERE alice has additionally
        // CO-SIGNED and handed out S'_bob, so the local rival-state guard fires FIRST and never
        // reaches the coordinator — building another state now would produce a rival that ties
        // with or loses to the one bob already holds. The earlier, more specific refusal is the
        // superseded-disclosure work doing its job; the assertion is re-derived, not relaxed.
        "outstanding conveyed state",
    )?;

    // =============================================================================================
    // [3] ROW 2, THE COOPERATIVE CANCEL — THE ONLY PARTY WHO CAN BE DEFRAUDED SIGNS THE RELEASE.
    //
    // CROSS-WALLET, which is the case tb05 cannot reach: bob is a different wallet, so the consent
    // has to be minted in bob's wallet and carried to alice out of band. The token is single-use,
    // endpoint-bound, and bound to THIS transfer's conveyed material, so alice can neither bank it
    // nor spend it against a re-addressed replacement — steps [7] and [8] attack exactly that.
    // =============================================================================================

    // INFORMED CONSENT. bob sees what he is being asked to abandon BEFORE signing, and every field
    // is established in bob's own wallet by decrypting the mailbox message — alice asserts none of
    // it. Note that the preview takes only a coin id: bob derives his own receiving key rather than
    // accepting one from the party requesting the consent.
    let preview = bob.preview_cancel_consent(&coin_b).await?;
    if preview.recipient_auth_pub_key() != bob_auth_b.to_string() {
        return Err(anyhow!(
            "[3] the preview derived recipient key {} but the transfer was addressed to {}",
            preview.recipient_auth_pub_key(),
            bob_auth_b
        ));
    }
    if preview.amount() != DEPOSIT {
        return Err(anyhow!(
            "[3] bob must be shown the real amount before consenting; preview said {} sats, coin B \
             is worth {DEPOSIT}",
            preview.amount()
        ));
    }
    if !preview.claimable_material_posted() {
        return Err(anyhow!(
            "[3] the preview must report that claimable material WAS posted — that is the fact that \
             makes bob's consent necessary at all"
        ));
    }
    if preview.statechain_id() != coin_b {
        return Err(anyhow!("[3] the preview described the wrong coin: {}", preview.statechain_id()));
    }
    if preview.is_coloured() {
        return Err(anyhow!("[3] coin B carries no RGB allocation; the preview says it does"));
    }

    // bob consents to THE OBJECT HE WAS SHOWN — there is no coin id and no key in this call, so
    // nothing alice said can steer it.
    let consent = bob.cancel_consent(&preview).await?;

    // The token carries its own binding: signature and subject travel together as one opaque string.
    if consent.split(':').count() != 3 {
        return Err(anyhow!(
            "[3] a consent token must carry <nonce>:<sig>:<digest>; got {} fields",
            consent.split(':').count()
        ));
    }
    if !consent.ends_with(preview.transfer_digest()) {
        return Err(anyhow!("[3] the minted token is not bound to the previewed transfer"));
    }

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
    match bob.preview_cancel_consent(&coin_c).await {
        std::result::Result::Ok(previewed) => {
            let consent = bob.cancel_consent(&previewed).await?;
            refused_with(
                alice
                    .cancel_transfer_with_consent(&coin_c, &bob_auth_c.to_string(), &consent)
                    .await,
                "[4] cancelling a CLAIMED transfer WITH the recipient's consent",
                "already claimed",
            )?;
        }
        // bob's receiving slot became the coin itself on claim, so previewing the old slot may
        // legitimately refuse here — and when it does it must refuse BY NAME as AlreadyClaimed,
        // which is the recipient-side half of the same rule. That is a stronger outcome than the
        // coordinator's refusal above, not a skipped assertion.
        Err(e) => {
            let unavailable = e
                .downcast_ref::<mercury_utexo_sdk::ConsentUnavailable>()
                .ok_or_else(|| {
                    anyhow!("[4] refusing to preview a CLAIMED transfer must be TYPED; got: {e}")
                })?;
            if !matches!(
                unavailable.reason,
                mercury_utexo_sdk::ConsentBlocked::AlreadyClaimed
                    | mercury_utexo_sdk::ConsentBlocked::NoPendingTransfer
            ) {
                return Err(anyhow!(
                    "[4] a claimed transfer must refuse as AlreadyClaimed (or vanish from the \
                     mailbox entirely); got {:?}",
                    unavailable.reason
                ));
            }
        }
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

    // Consent is not the right authorization for a latch, and row 4 is tested BEFORE the consent
    // rule in `decide_transfer_cancel` for exactly that reason. There are now TWO layers refusing
    // this, and both are asserted:
    //
    // First, bob cannot even MINT a consent for coin D. Only `get_new_x1` ran, so no mailbox message
    // was ever posted and bob has downloaded nothing — a recipient signs over material it has SEEN,
    // and there is none. The refusal is local and by name.
    match bob.preview_cancel_consent(&coin_d).await {
        std::result::Result::Ok(previewed) => {
            // If a consent can be minted at all, the latch must still refuse it.
            let token = bob.cancel_consent(&previewed).await?;
            refused_with(
                alice
                    .cancel_transfer_with_consent(&coin_d, &bob_auth_d.to_string(), &token)
                    .await,
                "[5] cancelling a BATCHED transfer WITH the recipient's consent",
                "batched (lightning-latch) transfer cannot be cancelled",
            )?;
        }
        Err(e) => {
            let unavailable = e
                .downcast_ref::<mercury_utexo_sdk::ConsentUnavailable>()
                .ok_or_else(|| {
                    anyhow!("[5] refusing to mint an unconveyed consent must be TYPED; got: {e}")
                })?;
            if unavailable.reason != mercury_utexo_sdk::ConsentBlocked::NoPendingTransfer {
                return Err(anyhow!(
                    "[5] coin D was never conveyed, so the refusal must be NoPendingTransfer; got \
                     {:?}",
                    unavailable.reason
                ));
            }
        }
    }

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

    // =============================================================================================
    // [7] ATTACK — MISDIRECTION. "CONSENT TO CANCELLING THE SMALL ONE", WHILE NAMING THE LARGE ONE.
    //
    // bob is receiving TWO coins of different value on two different slots: E (SMALL) and F (LARGE).
    // The old primitive took the coin id AND the recipient key from the caller and checked only that
    // the wallet held that key, so alice could describe one payment and name another; bob's wallet
    // would find the key, sign, and never show him an amount.
    //
    // WHICH GUARANTEE THIS ASSERTS, stated plainly: **the preview shows the TRUE amount, so the
    // mismatch is visible** — that is the second of the two alternatives, and it is the one the
    // design gives. The request is not "refused" on content, because nothing in the protocol lets a
    // wallet know what a human was told out of band. What it CAN guarantee, and what is asserted
    // here, is that the number bob sees always belongs to the transfer his signature will release.
    // =============================================================================================
    let bob_slot_e = mercuryrustlib::transfer_receiver::new_transfer_address(&cc, "sdk85_bob").await?;
    let (_, _, bob_auth_e) = mercurylib::decode_transfer_address(&bob_slot_e)?;
    let bob_slot_f = mercuryrustlib::transfer_receiver::new_transfer_address(&cc, "sdk85_bob").await?;
    let (_, _, bob_auth_f) = mercurylib::decode_transfer_address(&bob_slot_f)?;
    convey(&cc, "sdk85_alice", &bob_slot_e, &coin_e).await?;
    convey(&cc, "sdk85_alice", &bob_slot_f, &coin_f).await?;

    // THE STRONGEST FORM: alice names NOTHING. bob enumerates his own mailbox and sees both
    // transfers with their real, branch-validated amounts. A request that describes a small payment
    // has to match an entry here.
    let cancellable = bob.preview_all_cancellable_consents().await?;
    let seen: Vec<(String, u64)> = cancellable
        .iter()
        .map(|p| (p.statechain_id().to_string(), p.amount()))
        .collect();
    if !seen.iter().any(|(sid, amt)| sid == &coin_e && *amt == SMALL) {
        return Err(anyhow!("[7] bob's own enumeration lost coin E at {SMALL} sats; saw {seen:?}"));
    }
    if !seen.iter().any(|(sid, amt)| sid == &coin_f && *amt == LARGE) {
        return Err(anyhow!("[7] bob's own enumeration lost coin F at {LARGE} sats; saw {seen:?}"));
    }

    // THE MISDIRECTION ITSELF. alice says "the small one" and passes coin F. Whatever she says, the
    // preview describes F truthfully — the LARGE amount — so the lie is visible before anything is
    // signed. This is the assertion that would have failed against the old blind primitive, which
    // showed no amount at all.
    let misdirected = bob.preview_cancel_consent(&coin_f).await?;
    if misdirected.amount() != LARGE {
        return Err(anyhow!(
            "SAFETY: [7] alice named coin F while describing the small payment, and bob was shown \
             {} sats instead of the true {LARGE}",
            misdirected.amount()
        ));
    }
    if misdirected.recipient_auth_pub_key() != bob_auth_f.to_string() {
        return Err(anyhow!(
            "[7] the preview of coin F derived recipient key {} rather than the slot it was \
             addressed to, {bob_auth_f}",
            misdirected.recipient_auth_pub_key()
        ));
    }

    // …and bob consents to the SMALL one, which is what he actually agreed to. The token is minted
    // from the previewed object, so it can only ever be about coin E.
    let preview_e = bob.preview_cancel_consent(&coin_e).await?;
    if preview_e.amount() != SMALL || preview_e.statechain_id() != coin_e {
        return Err(anyhow!(
            "[7] bob's preview of the small coin is wrong: {} sats for {}",
            preview_e.amount(),
            preview_e.statechain_id()
        ));
    }
    if preview_e.transfer_digest() == misdirected.transfer_digest() {
        return Err(anyhow!(
            "SAFETY: [7] two different transfers share a consent binding — one token would release \
             either"
        ));
    }
    let consent_e = bob.cancel_consent(&preview_e).await?;

    // =============================================================================================
    // [8] ATTACK — REPLAY. A CONSENT RELEASES ONE TRANSFER INSTANCE, NOT A COIN.
    //
    // alice now holds a token bob minted for coin E. Every way of spending it on something else is
    // asserted shut, in the order an attacker would try them.
    // =============================================================================================

    // [8a] CROSS-INSTANCE. Present E's token against F's cancellation. The coordinator recomputes
    // the binding FROM F's OWN ROW and refuses the mismatch by its own name — not as a bad
    // signature, which is a different accusation and would send alice back to an honest bob.
    refused_with(
        alice
            .cancel_transfer_with_consent(&coin_f, &bob_auth_f.to_string(), &consent_e)
            .await,
        "[8a] spending a consent minted for coin E against coin F",
        "bound to different transfer material",
    )?;

    // [8b] RE-OPENING THE ROW to manufacture a second instance to spend the token on. `get_new_x1`
    // is the raw POST /transfer/sender — the re-address guard is what must refuse it, and until that
    // guard's `batch_id = $3` became `IS NOT DISTINCT FROM $3` it was dead code on this
    // (non-batched) path and this call SUCCEEDED, minting a fresh x1 over the top of bob's transfer.
    let coin = confirmed_coin(&cc, "sdk85_alice", &coin_e).await.ok();
    let signed_e = match coin.and_then(|c| c.signed_statechain_id) {
        Some(s) => s,
        // The coin is IN_TRANSFER, so alice's own wallet no longer offers it as CONFIRMED. Recover
        // the signed id from the row she still holds rather than skipping the attack.
        None => coins_of(&cc, "sdk85_alice")
            .await?
            .into_iter()
            .find(|c| c.statechain_id.as_deref() == Some(coin_e.as_str()) && c.duplicate_index == 0)
            .and_then(|c| c.signed_statechain_id)
            .ok_or_else(|| anyhow!("[8b] alice cannot sign for coin E"))?,
    };
    refused_with(
        mercuryrustlib::transfer_sender::get_new_x1(
            &cc,
            &coin_e,
            &signed_e,
            &bob_auth_e.to_string(),
            None,
        )
        .await,
        "[8b] re-opening a conveyed transfer to the SAME recipient key",
        "Transfer message already exists",
    )?;

    // [8c] RE-CONVEYING outright. This used to die on the coordinator's pending-transfer lock; it
    // now dies one layer EARLIER still, in the wallet, on the rival-state guard — alice has already
    // co-signed and handed out a state at a lower CSV, so a second one would tie with or lose to it
    // over the same outpoint. Local, before any co-signature is spent. Re-derived, not relaxed.
    refused_with(
        convey(&cc, "sdk85_alice", &bob_slot_e, &coin_e).await,
        "[8c] re-conveying a coin whose transfer is still open",
        "outstanding conveyed state",
    )?;

    // [8d] THE TOKEN IS STILL GOOD FOR THE TRANSFER IT WAS GIVEN FOR. This is the other half of
    // [8a]: the staleness test runs BEFORE the nonce is consumed, so a sender who tried to misuse a
    // consent cannot ALSO have burned it. If this fails, the attacker has gained a denial of
    // service against every honest cancellation.
    let outcome = alice
        .cancel_transfer_with_consent(&coin_e, &bob_auth_e.to_string(), &consent_e)
        .await?;
    if outcome != CancelOutcome::Cancelled {
        return Err(anyhow!(
            "[8d] the refused cross-instance attempt burned bob's nonce — the honest cancellation \
             of coin E got {outcome:?}"
        ));
    }

    // [8e] AND AFTERWARDS the same key cannot be reused: a recipient key whose transfer was
    // cancelled is retired, so alice cannot rebuild the situation and ask for a fresh consent.
    //
    // PROBED WITH THE RAW `get_new_x1`, exactly as [8b] probes the re-address guard, and NOT with a
    // full `convey`. The guard under test is the coordinator's `recipient_key_was_cancelled` — it
    // lives in `POST /transfer/sender`, so the raw call reaches it directly. A full `convey` would
    // reach it too, but only AFTER the client co-signs `S'`, which SPENDS A STATE RUNG: the regtest
    // schedule gives a coin three (d0 24, delta 6, floor 6 => 24 -> 18 -> 12 -> 6) and coin E has
    // already spent two here — the conveyance to bob, and the cancellation's reclaim in [8d]. The
    // third would leave nothing for [8f]'s conveyance to carol, which is the step that proves
    // exactly one payee. Burning a rung to reach a server-side refusal tests the same property and
    // costs the budget the SCENARIO needs; this asserts the same guard at the layer it lives on.
    refused_with(
        mercuryrustlib::transfer_sender::get_new_x1(
            &cc,
            &coin_e,
            &signed_e,
            &bob_auth_e.to_string(),
            None,
        )
        .await,
        "[8e] re-opening a transfer to a recipient key whose transfer was cancelled",
        "was cancelled",
    )?;

    // [8f] EXACTLY ONE PAYEE, for the attacked coin: bob never books E, carol does.
    for _ in 0..15 {
        let pass = bob.claim().await?;
        if pass.confirmed_deposits.iter().any(|d| d.statechain_id.as_deref() == Some(&coin_e)) {
            return Err(anyhow!("SAFETY: [8f] bob booked a deposit for the CANCELLED coin E"));
        }
        if confirmed_coin(&cc, "sdk85_bob", &coin_e).await.is_ok() {
            return Err(anyhow!(
                "SAFETY: [8f] bob CLAIMED statechain id {coin_e} after consenting to its \
                 cancellation — both bob and carol can now be paid from one coin"
            ));
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
    let carol_slot_e = mercuryrustlib::transfer_receiver::new_transfer_address(&cc, "sdk85_carol").await?;
    convey(&cc, "sdk85_alice", &carol_slot_e, &coin_e).await?;
    claim_until(&cc, &carol, "sdk85_carol", &coin_e).await?;

    // [8g] THE UN-ATTACKED TRANSFER IS UNHARMED. Coin F was the target of the misdirection and of
    // the cross-instance replay, and both were refused — so F must still complete normally. A fix
    // that defended by breaking honest cancellations, or honest CLAIMS, would pass every negative
    // assertion above and fail here.
    claim_until(&cc, &bob, "sdk85_bob", &coin_f).await?;

    // End state, as declared in the module doc: bob holds C and F; carol holds A, B and E.
    confirmed_coin(&cc, "sdk85_bob", &coin_c).await?;
    confirmed_coin(&cc, "sdk85_bob", &coin_f).await?;
    confirmed_coin(&cc, "sdk85_carol", &coin_a).await?;
    confirmed_coin(&cc, "sdk85_carol", &coin_b).await?;
    confirmed_coin(&cc, "sdk85_carol", &coin_e).await?;
    if holds_any(&cc, "sdk85_bob", &coin_e).await? {
        return Err(anyhow!(
            "SAFETY: [8f] bob still holds material for coin E after carol was paid with it"
        ));
    }

    println!("sdk85_transfer_cancel: OK");
    Ok(())
}
