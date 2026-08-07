//! Transfer cancellation — withdrawing an opened, unclaimed transfer so the coin becomes
//! spendable again.
//!
//! # Why this needs a policy module at all
//!
//! The coordinator's pending-transfer lock (`has_open_transfer`) is LOAD-BEARING. While a transfer
//! of a coin is open the SE refuses every co-signature of that coin, and that refusal is the ONLY
//! thing standing between a conveyed-but-unclaimed receiver and a still-owner sender who co-signs a
//! rival state. Cancellation releases that lock, so a naive "the sender asked nicely" cancel
//! reopens exactly the hole the lock closes:
//!
//! 1. sender posts the transfer message for Bob — Bob now holds claimable material,
//! 2. sender cancels,
//! 3. sender opens a new transfer to Carol,
//! 4. Bob and Carol race; the loser believes they were paid and was not.
//!
//! The coordinator cannot distinguish "Bob has not downloaded the message" from "Bob downloaded it
//! and is about to claim": `GET /transfer/get_msg_addr/<key>` is unauthenticated and
//! non-destructive, so a `retrieved_at` column would be settable or withholdable by anyone who
//! knows the (non-secret) receiver key. **Sender authorization is therefore NOT a sufficient
//! condition for cancelling a conveyed transfer.**
//!
//! # The rule
//!
//! Exactly one condition makes the general case safe: the only party who can be defrauded signs the
//! release.
//!
//! | coordinator-observable state | test | authorization required |
//! |---|---|---|
//! | opened, message never posted | `encrypted_transfer_msg IS NULL` | sender alone |
//! | message posted, unclaimed | msg NOT NULL, `key_updated = false` | sender **and** recipient |
//! | claimed | `key_updated = true` | never cancellable |
//! | batched (lightning latch) | `batch_id IS NOT NULL` | never cancellable |
//!
//! Row 1 is sound RELATIVE TO THE PROTOCOL, not physically: a sender can always hand the encrypted
//! message to a recipient out of band, and the coordinator cannot see that. A conforming recipient
//! learns of a transfer by polling `get_msg_addr` and finds nothing there, so no conforming
//! recipient is defrauded. This is the same guarantee the pre-existing one-hour expiry already
//! gives — cancellation only shortens the wait from an hour to now. It is NOT a general "the sender
//! may take it back" power, and [`CancelDecision::RecipientConsentRequired`] is the named refusal
//! for the case that cannot be made safe.
//!
//! Row 4 is refused rather than co-signed because an LN-latched transfer is governed by the latch's
//! own clock: cancelling would race the SSP's preimage settlement, and the receiver-consent rule
//! does not help (the latch, not the receiver, decides whether the payment happened).
//!
//! # Replay
//!
//! Both signatures are single-use and endpoint-bound (`"<nonce>:<sig>"` over `sha256(nonce|endpoint)`,
//! see the server's `validate_signature_nonce`), never the static `sha256(statechain_id)` signature.
//! This is not decoration. With a static recipient signature the sender would hold a REUSABLE cancel
//! token for that coin/recipient-key pair: after one cooperative cancellation it could open a second
//! transfer to the same recipient key (`has_open_transfer_to_other_auth` permits same-key reopens),
//! post the message so the recipient believes they were paid, replay the captured signature to
//! cancel, and convey to a third party — the exact two-victim break the consent rule exists to
//! prevent. Single-use nonces make each consent authorize exactly one cancellation.

use serde::{Deserialize, Serialize};

/// Whether a recipient co-signature accompanied the cancellation request, and whether it verified.
///
/// "Verified" means: a single-use, endpoint-bound signature (`"<nonce>:<sig>"` over
/// `sha256(nonce|"transfer/cancel/recipient")`) by the key the coordinator RECORDED as this
/// transfer's `new_user_auth_public_key`. A signature by any other key is [`Self::Invalid`] — the
/// consent that matters is the recorded recipient's, not whoever the sender chooses to present.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecipientConsent {
    /// No recipient signature was supplied.
    Absent,
    /// A recipient signature was supplied but did not verify: wrong key, malformed or bad
    /// signature, or a nonce that was unknown, expired, or already spent.
    Invalid,
    /// A fresh single-use signature by the transfer's recorded recipient auth key.
    Valid,
}

/// The coordinator-observable state of a coin's transfer row, as the cancel endpoint reads it.
///
/// Every field is something the COORDINATOR can establish from its own database. Nothing here is
/// asserted by the sender, and nothing here requires the SE to see an amount, a colour, or which
/// leg of a transfer is which — the enclave is not consulted by a cancellation at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransferCancelState {
    /// A `statechain_transfer` row exists for this statechain id.
    pub row_exists: bool,
    /// The recipient completed the key handover (`key_updated = true`). Terminal.
    pub key_updated: bool,
    /// The row already carries a `cancelled_at`.
    pub already_cancelled: bool,
    /// The transfer belongs to a batch (lightning latch).
    pub is_batched: bool,
    /// `encrypted_transfer_msg IS NOT NULL` — the sender posted the mailbox message, so claimable
    /// material has left the sender's control.
    pub message_posted: bool,
    /// A `/transfer/receiver` claim latched this row within the latch window and may already have
    /// reached the enclave. Cancelling under it could brick the coin (the enclave rotates its share
    /// on keyupdate and cannot be told to roll back), so cancellation defers to the claim.
    pub claim_in_flight: bool,
    /// Recipient co-signature status (see [`RecipientConsent`]).
    pub recipient_consent: RecipientConsent,
}

impl TransferCancelState {
    /// A state with every flag cleared and no recipient consent — the "no such row" base case.
    /// Intended for constructing states in tests and at call sites field-by-field.
    pub fn empty() -> Self {
        TransferCancelState {
            row_exists: false,
            key_updated: false,
            already_cancelled: false,
            is_batched: false,
            message_posted: false,
            claim_in_flight: false,
            recipient_consent: RecipientConsent::Absent,
        }
    }
}

/// What the coordinator should do with a cancellation request whose SENDER authorization has
/// already been verified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CancelDecision {
    /// Release the transfer: clear `x1` and the mailbox message, stamp `cancelled_at`, tombstone it.
    Allow,
    /// The row is already cancelled. Idempotent success — a client retry must not become an error.
    AlreadyCancelled,
    /// No transfer row for this statechain id (404).
    NoSuchTransfer,
    /// The recipient completed the handover. A completed payment is never withdrawable (410).
    AlreadyClaimed,
    /// Batched (lightning-latch) transfer: governed by the latch expiry, not by the sender (409).
    Batched,
    /// A claim is latched and may already have reached the enclave; retry after the latch lapses
    /// (409).
    ClaimInFlight,
    /// **The named refusal.** The message was posted, so a recipient may already hold claimable
    /// material, and no recipient co-signature was supplied. The sender alone cannot withdraw this;
    /// either obtain the recipient's consent or wait out the expiry window (409).
    RecipientConsentRequired,
    /// A recipient co-signature was supplied but did not verify under the RECORDED recipient key
    /// (403). Distinct from [`Self::RecipientConsentRequired`] so a wrong-key or replayed consent is
    /// never reported as a missing one.
    RecipientSignatureInvalid,
}

impl CancelDecision {
    /// True only for the decision that actually releases the pending-transfer lock.
    pub fn releases_lock(&self) -> bool {
        matches!(self, CancelDecision::Allow)
    }

    /// A stable machine-readable code for the response body, so clients branch on this rather than
    /// on prose that adversarial tests pin.
    pub fn code(&self) -> &'static str {
        match self {
            CancelDecision::Allow => "cancelled",
            CancelDecision::AlreadyCancelled => "already_cancelled",
            CancelDecision::NoSuchTransfer => "no_such_transfer",
            CancelDecision::AlreadyClaimed => "already_claimed",
            CancelDecision::Batched => "batched_transfer",
            CancelDecision::ClaimInFlight => "claim_in_flight",
            CancelDecision::RecipientConsentRequired => "recipient_consent_required",
            CancelDecision::RecipientSignatureInvalid => "recipient_signature_invalid",
        }
    }

    /// The refusal text. Adversarial tests pin these exactly.
    pub fn message(&self) -> &'static str {
        match self {
            CancelDecision::Allow => "transfer cancelled",
            CancelDecision::AlreadyCancelled => "transfer was already cancelled",
            CancelDecision::NoSuchTransfer => "no open transfer for this statechain id",
            CancelDecision::AlreadyClaimed => {
                "transfer already claimed by the recipient; it cannot be cancelled"
            }
            CancelDecision::Batched => {
                "batched (lightning-latch) transfer cannot be cancelled; it is released by the latch expiry"
            }
            CancelDecision::ClaimInFlight => {
                "a claim for this transfer is in progress; cancellation refused"
            }
            CancelDecision::RecipientConsentRequired => {
                "transfer message already conveyed; the recipient must co-sign the cancellation"
            }
            CancelDecision::RecipientSignatureInvalid => {
                "recipient co-signature does not match this transfer's recipient key"
            }
        }
    }
}

/// Decide a cancellation from coordinator-observable state.
///
/// PRECONDITION: the caller has already verified the SENDER's single-use, endpoint-bound
/// authorization for `statechain_id`. This function decides only whether the transfer's STATE
/// permits withdrawal; it never re-decides who the sender is.
///
/// The ordering of the guards is itself the safety argument:
///
/// * `key_updated` is tested before everything else, because a completed payment is terminal — no
///   later state (a stale cancel flag, a batch, a latch) may un-complete it.
/// * `already_cancelled` next, so retries are idempotent rather than degrading into a different
///   refusal.
/// * `is_batched` before the consent rule, because recipient consent is not the right authorization
///   for an LN latch (the preimage is).
/// * `claim_in_flight` before `Allow`, because past that point the enclave may already have rotated
///   its share; a cancel that wins the coordinator race but loses the enclave race would brick the
///   coin.
/// * only then does `message_posted` select between sender-alone and sender-plus-recipient.
pub fn decide_transfer_cancel(state: &TransferCancelState) -> CancelDecision {
    if !state.row_exists {
        return CancelDecision::NoSuchTransfer;
    }
    if state.key_updated {
        return CancelDecision::AlreadyClaimed;
    }
    if state.already_cancelled {
        return CancelDecision::AlreadyCancelled;
    }
    if state.is_batched {
        return CancelDecision::Batched;
    }
    if state.claim_in_flight {
        return CancelDecision::ClaimInFlight;
    }
    if !state.message_posted {
        // Nothing left the coordinator: no conforming recipient can be holding claimable material,
        // because a conforming recipient learns of a transfer only by polling `get_msg_addr`.
        return CancelDecision::Allow;
    }
    match state.recipient_consent {
        RecipientConsent::Valid => CancelDecision::Allow,
        RecipientConsent::Invalid => CancelDecision::RecipientSignatureInvalid,
        RecipientConsent::Absent => CancelDecision::RecipientConsentRequired,
    }
}

/// `POST /transfer/cancel` request.
///
/// `auth_sig` and `recipient_auth_sig` are BOTH single-use tokens of the form `"<nonce>:<sig>"`
/// (nonce from `GET /auth/challenge/<statechain_id>`), signed over `sha256(nonce|endpoint)` with
/// the endpoint strings [`CANCEL_SENDER_ENDPOINT`] and [`CANCEL_RECIPIENT_ENDPOINT`] respectively.
/// The distinct endpoint strings keep the two legs from ever being substituted for one another, and
/// keep either from being redirected at another nonce-protected endpoint such as
/// `withdraw/complete`.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TransferCancelRequestPayload {
    pub statechain_id: String,
    /// Sender leg: `"<nonce>:<sig>"` over `sha256(nonce|"transfer/cancel")`.
    pub auth_sig: String,
    /// Recipient leg: `"<nonce>:<sig>"` over `sha256(nonce|"transfer/cancel/recipient")`. Required
    /// once the mailbox message has been posted; ignored otherwise.
    pub recipient_auth_sig: Option<String>,
    /// The recipient auth public key the recipient leg claims to be. Must equal the transfer's
    /// recorded `new_user_auth_public_key` or the consent is [`RecipientConsent::Invalid`].
    pub recipient_auth_pub_key: Option<String>,
}

/// `POST /transfer/cancel` response, on success and on every refusal.
///
/// `recipient_auth_pub_key` is populated ONLY on [`CancelDecision::RecipientConsentRequired`], and
/// only after the sender's own authorization verified — it tells an authenticated sender which key
/// must co-sign. It is deliberately not exposed anywhere unauthenticated: publishing coin →
/// recipient-key links would let an observer correlate a coin with its next owner's mailbox.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TransferCancelResponsePayload {
    /// [`CancelDecision::code`].
    pub code: String,
    /// [`CancelDecision::message`].
    pub message: String,
    /// The recipient key whose co-signature is required, on `recipient_consent_required` only.
    pub recipient_auth_pub_key: Option<String>,
}

/// Endpoint string bound into the sender's cancellation signature.
pub const CANCEL_SENDER_ENDPOINT: &str = "transfer/cancel";
/// Endpoint string bound into the recipient's cancellation co-signature.
pub const CANCEL_RECIPIENT_ENDPOINT: &str = "transfer/cancel/recipient";

#[cfg(test)]
mod tests {
    use super::*;

    /// An open, conveyed, unclaimed transfer — the state the whole feature is about.
    fn conveyed_unclaimed() -> TransferCancelState {
        TransferCancelState {
            row_exists: true,
            key_updated: false,
            already_cancelled: false,
            is_batched: false,
            message_posted: true,
            claim_in_flight: false,
            recipient_consent: RecipientConsent::Absent,
        }
    }

    /// An opened transfer whose mailbox message was never posted.
    fn opened_not_conveyed() -> TransferCancelState {
        TransferCancelState { message_posted: false, ..conveyed_unclaimed() }
    }

    #[test]
    fn missing_row_is_not_found() {
        assert_eq!(
            decide_transfer_cancel(&TransferCancelState::empty()),
            CancelDecision::NoSuchTransfer
        );
    }

    #[test]
    fn opened_but_never_conveyed_needs_only_the_sender() {
        assert_eq!(decide_transfer_cancel(&opened_not_conveyed()), CancelDecision::Allow);
    }

    /// THE rule. A conveyed transfer is not the sender's to take back.
    #[test]
    fn conveyed_transfer_refuses_a_sender_only_cancel() {
        let d = decide_transfer_cancel(&conveyed_unclaimed());
        assert_eq!(d, CancelDecision::RecipientConsentRequired);
        assert!(!d.releases_lock(), "a sender-only cancel must NOT release the pending-transfer lock");
        assert_eq!(
            d.message(),
            "transfer message already conveyed; the recipient must co-sign the cancellation"
        );
    }

    #[test]
    fn conveyed_transfer_allows_a_recipient_co_signed_cancel() {
        let s = TransferCancelState {
            recipient_consent: RecipientConsent::Valid,
            ..conveyed_unclaimed()
        };
        assert_eq!(decide_transfer_cancel(&s), CancelDecision::Allow);
    }

    /// A signature by a key that is not the RECORDED recipient is reported as invalid, never as
    /// consent and never as a missing signature.
    #[test]
    fn wrong_key_consent_is_invalid_not_missing() {
        let s = TransferCancelState {
            recipient_consent: RecipientConsent::Invalid,
            ..conveyed_unclaimed()
        };
        let d = decide_transfer_cancel(&s);
        assert_eq!(d, CancelDecision::RecipientSignatureInvalid);
        assert!(!d.releases_lock());
    }

    #[test]
    fn claimed_transfer_is_never_cancellable_whatever_the_consent() {
        for consent in [RecipientConsent::Absent, RecipientConsent::Invalid, RecipientConsent::Valid]
        {
            for posted in [true, false] {
                let s = TransferCancelState {
                    key_updated: true,
                    message_posted: posted,
                    recipient_consent: consent,
                    ..conveyed_unclaimed()
                };
                assert_eq!(
                    decide_transfer_cancel(&s),
                    CancelDecision::AlreadyClaimed,
                    "a completed handover must stay completed (consent={consent:?}, posted={posted})"
                );
            }
        }
    }

    /// Even with a valid recipient co-signature, an LN-latched transfer is refused: the latch, not
    /// the recipient, decides whether the payment happened.
    #[test]
    fn batched_transfer_is_refused_even_with_valid_consent() {
        for consent in [RecipientConsent::Absent, RecipientConsent::Invalid, RecipientConsent::Valid]
        {
            let s = TransferCancelState {
                is_batched: true,
                recipient_consent: consent,
                ..conveyed_unclaimed()
            };
            assert_eq!(decide_transfer_cancel(&s), CancelDecision::Batched);
        }
    }

    /// A batched transfer that was never conveyed is STILL refused — `message_posted` must not be a
    /// back door around the latch.
    #[test]
    fn batched_and_unconveyed_is_still_refused() {
        let s = TransferCancelState { is_batched: true, ..opened_not_conveyed() };
        assert_eq!(decide_transfer_cancel(&s), CancelDecision::Batched);
    }

    #[test]
    fn claim_in_flight_defers_even_with_valid_consent() {
        let s = TransferCancelState {
            claim_in_flight: true,
            recipient_consent: RecipientConsent::Valid,
            ..conveyed_unclaimed()
        };
        assert_eq!(decide_transfer_cancel(&s), CancelDecision::ClaimInFlight);
        // and for the unconveyed case too
        let s = TransferCancelState { claim_in_flight: true, ..opened_not_conveyed() };
        assert_eq!(decide_transfer_cancel(&s), CancelDecision::ClaimInFlight);
    }

    #[test]
    fn cancellation_is_idempotent() {
        let s = TransferCancelState { already_cancelled: true, ..conveyed_unclaimed() };
        let d = decide_transfer_cancel(&s);
        assert_eq!(d, CancelDecision::AlreadyCancelled);
        assert!(!d.releases_lock(), "an idempotent retry must not re-run the release");
    }

    /// Exhaustive: over EVERY reachable state, `Allow` is returned only when the coin cannot have a
    /// defrauded conforming recipient — i.e. either nothing was conveyed, or the recorded recipient
    /// signed the release.
    #[test]
    fn allow_only_when_no_conforming_recipient_can_be_defrauded() {
        let bools = [false, true];
        let consents =
            [RecipientConsent::Absent, RecipientConsent::Invalid, RecipientConsent::Valid];
        let mut allowed = 0usize;
        for &key_updated in &bools {
            for &already_cancelled in &bools {
                for &is_batched in &bools {
                    for &message_posted in &bools {
                        for &claim_in_flight in &bools {
                            for &recipient_consent in &consents {
                                let s = TransferCancelState {
                                    row_exists: true,
                                    key_updated,
                                    already_cancelled,
                                    is_batched,
                                    message_posted,
                                    claim_in_flight,
                                    recipient_consent,
                                };
                                if decide_transfer_cancel(&s) == CancelDecision::Allow {
                                    allowed += 1;
                                    assert!(
                                        !key_updated
                                            && !already_cancelled
                                            && !is_batched
                                            && !claim_in_flight,
                                        "Allow returned for a terminal/latched state: {s:?}"
                                    );
                                    assert!(
                                        !message_posted
                                            || recipient_consent == RecipientConsent::Valid,
                                        "Allow returned for a CONVEYED transfer without recipient \
                                         consent — this is the two-victim break: {s:?}"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
        // sanity: the exhaustive sweep actually reached the Allow branch
        assert!(allowed > 0, "no state reached Allow; the sweep proves nothing");
    }

    /// A row_exists = false state is NEVER cancellable regardless of the other flags, so a deleted
    /// or never-opened transfer cannot be "cancelled" into releasing anything.
    #[test]
    fn absent_row_never_allows() {
        let bools = [false, true];
        for &key_updated in &bools {
            for &message_posted in &bools {
                for &recipient_consent in
                    &[RecipientConsent::Absent, RecipientConsent::Valid]
                {
                    let s = TransferCancelState {
                        row_exists: false,
                        key_updated,
                        message_posted,
                        recipient_consent,
                        ..TransferCancelState::empty()
                    };
                    assert_eq!(decide_transfer_cancel(&s), CancelDecision::NoSuchTransfer);
                }
            }
        }
    }

    // ==========================================================================================
    // ADVERSARIAL: the two-victim scenario the pending-transfer lock exists to prevent.
    // ==========================================================================================

    /// A minimal model of the coordinator's transfer row, exercising the ACTUAL transitions the
    /// server performs: `insert_new_transfer` (DELETE + INSERT, so re-addressing overwrites),
    /// `update_msg`, `decide_transfer_cancel` + apply, and `claim`.
    #[derive(Debug, Clone)]
    struct CoordinatorRow {
        recipient: &'static str,
        message_posted: bool,
        key_updated: bool,
        cancelled: bool,
    }

    #[derive(Debug, Default)]
    struct Coordinator {
        row: Option<CoordinatorRow>,
        /// (statechain-scoped) recipient keys that had a cancelled transfer — the tombstone.
        tombstoned: Vec<&'static str>,
        /// Recipients whose claim succeeded. More than one entry here is theft.
        paid: Vec<&'static str>,
    }

    impl Coordinator {
        /// `POST /transfer/sender` — open (or re-address) a transfer.
        fn open(&mut self, recipient: &'static str) -> Result<(), &'static str> {
            if let Some(r) = &self.row {
                if !r.cancelled && !r.key_updated && r.recipient != recipient {
                    return Err("coin already has an open transfer to a different recipient");
                }
            }
            // The tombstone rule: a recipient key that already had a cancelled transfer of this coin
            // may not be reused, so stale conveyed material can never meet a fresh x1.
            if self.tombstoned.contains(&recipient) {
                return Err("recipient key had a cancelled transfer of this coin; use a fresh address");
            }
            // insert_new_transfer: DELETE then INSERT.
            self.row = Some(CoordinatorRow {
                recipient,
                message_posted: false,
                key_updated: false,
                cancelled: false,
            });
            Ok(())
        }

        /// `POST /transfer/update_msg` — post the mailbox message.
        fn convey(&mut self) {
            if let Some(r) = &mut self.row {
                r.message_posted = true;
            }
        }

        /// `POST /transfer/cancel`, with `consent` naming the key that co-signed (if any).
        fn cancel(&mut self, consent: Option<&'static str>) -> CancelDecision {
            let (state, recipient) = match &self.row {
                None => (TransferCancelState::empty(), None),
                Some(r) => {
                    let recipient_consent = match consent {
                        None => RecipientConsent::Absent,
                        Some(k) if k == r.recipient => RecipientConsent::Valid,
                        Some(_) => RecipientConsent::Invalid,
                    };
                    (
                        TransferCancelState {
                            row_exists: true,
                            key_updated: r.key_updated,
                            already_cancelled: r.cancelled,
                            is_batched: false,
                            message_posted: r.message_posted,
                            claim_in_flight: false,
                            recipient_consent,
                        },
                        Some(r.recipient),
                    )
                }
            };
            let decision = decide_transfer_cancel(&state);
            if decision.releases_lock() {
                let r = self.row.as_mut().unwrap();
                r.cancelled = true;
                r.message_posted = false; // the mailbox message is destroyed
                self.tombstoned.push(recipient.unwrap());
            }
            decision
        }

        /// `POST /transfer/receiver` — a recipient claims with material it holds.
        fn claim(&mut self, who: &'static str) -> Result<(), &'static str> {
            let r = self.row.as_mut().ok_or("no transfer row")?;
            if r.cancelled {
                return Err("transfer cancelled");
            }
            if r.recipient != who {
                return Err("signature does not match this transfer's recipient key");
            }
            if !r.message_posted && !r.key_updated {
                return Err("no conveyed material");
            }
            r.key_updated = true;
            self.paid.push(who);
            Ok(())
        }
    }

    /// The headline attack: convey to Bob, cancel, convey to Carol. It must be impossible to make
    /// two recipients both believe they were paid.
    #[test]
    fn adversarial_convey_cancel_reconvey_cannot_pay_two_recipients() {
        let mut c = Coordinator::default();

        // 1. sender conveys to Bob — Bob now holds claimable material.
        c.open("bob").unwrap();
        c.convey();

        // 2. sender tries to take it back alone. REFUSED — this is the whole safety property.
        assert_eq!(c.cancel(None), CancelDecision::RecipientConsentRequired);
        // ... and with someone else's signature (e.g. its own, or Carol's).
        assert_eq!(c.cancel(Some("carol")), CancelDecision::RecipientSignatureInvalid);
        assert_eq!(c.cancel(Some("sender")), CancelDecision::RecipientSignatureInvalid);

        // 3. so the sender cannot re-address to Carol either: the lock still holds.
        assert_eq!(
            c.open("carol").unwrap_err(),
            "coin already has an open transfer to a different recipient"
        );

        // 4. Bob claims. Exactly one recipient is paid.
        c.claim("bob").unwrap();
        assert_eq!(c.paid, vec!["bob"]);

        // 5. and the claimed transfer can never be cancelled afterwards.
        assert_eq!(c.cancel(Some("bob")), CancelDecision::AlreadyClaimed);
        assert_eq!(c.cancel(None), CancelDecision::AlreadyClaimed);
    }

    /// The cooperative path: Bob agrees he was not paid and co-signs. The coin returns to the
    /// sender, Carol can then be paid — and Bob cannot ALSO claim.
    #[test]
    fn adversarial_cooperative_cancel_pays_exactly_the_second_recipient() {
        let mut c = Coordinator::default();

        c.open("bob").unwrap();
        c.convey();
        assert_eq!(c.cancel(Some("bob")), CancelDecision::Allow);

        // Bob's stale material is dead.
        assert_eq!(c.claim("bob").unwrap_err(), "transfer cancelled");

        c.open("carol").unwrap();
        c.convey();
        c.claim("carol").unwrap();

        assert_eq!(c.paid, vec!["carol"], "exactly one recipient may end up paid");
        // Bob cannot claim the re-addressed transfer either.
        assert_eq!(
            c.claim("bob").unwrap_err(),
            "signature does not match this transfer's recipient key"
        );
    }

    /// Consent is single-use. After Bob co-signs one cancellation, the sender may not reuse that
    /// consent against a SECOND transfer to Bob: the tombstone refuses the key outright, so the
    /// "cancel with a captured signature, then convey to Carol" replay never gets started.
    #[test]
    fn adversarial_recipient_consent_cannot_be_replayed_against_a_later_transfer() {
        let mut c = Coordinator::default();

        c.open("bob").unwrap();
        c.convey();
        assert_eq!(c.cancel(Some("bob")), CancelDecision::Allow);

        // The sender now holds a used consent from "bob". Reopening to the same key is refused.
        assert_eq!(
            c.open("bob").unwrap_err(),
            "recipient key had a cancelled transfer of this coin; use a fresh address"
        );

        // A fresh recipient key from the same human ("bob2") is fine, and its cancellation needs a
        // FRESH consent — the used one names a different key and reads as invalid.
        c.open("bob2").unwrap();
        c.convey();
        assert_eq!(c.cancel(Some("bob")), CancelDecision::RecipientSignatureInvalid);
        c.claim("bob2").unwrap();
        assert_eq!(c.paid, vec!["bob2"]);
    }

    /// The un-conveyed cancel does not become a re-address back door: after cancelling an
    /// un-conveyed transfer the sender may address a NEW recipient, and only that recipient can
    /// claim.
    #[test]
    fn adversarial_unconveyed_cancel_still_pays_only_one() {
        let mut c = Coordinator::default();

        c.open("bob").unwrap();
        // nothing conveyed
        assert_eq!(c.cancel(None), CancelDecision::Allow);
        assert_eq!(c.claim("bob").unwrap_err(), "transfer cancelled");

        c.open("carol").unwrap();
        c.convey();
        c.claim("carol").unwrap();
        assert_eq!(c.paid, vec!["carol"]);
    }

    /// tb05's shape: the sender transfers to an address it OWNS, so it holds both keys and can
    /// cancel unilaterally-in-effect — without that being a general sender-only cancel.
    #[test]
    fn self_addressed_transfer_can_be_cancelled_by_its_own_owner() {
        let mut c = Coordinator::default();
        c.open("self").unwrap();
        c.convey();
        // sender alone: still refused, on the same rule as everyone else.
        assert_eq!(c.cancel(None), CancelDecision::RecipientConsentRequired);
        // but the sender HOLDS the recipient key, so it can produce the consent.
        assert_eq!(c.cancel(Some("self")), CancelDecision::Allow);
        c.open("wallet2").unwrap();
        c.convey();
        c.claim("wallet2").unwrap();
        assert_eq!(c.paid, vec!["wallet2"]);
    }

    #[test]
    fn codes_are_distinct_and_stable() {
        let all = [
            CancelDecision::Allow,
            CancelDecision::AlreadyCancelled,
            CancelDecision::NoSuchTransfer,
            CancelDecision::AlreadyClaimed,
            CancelDecision::Batched,
            CancelDecision::ClaimInFlight,
            CancelDecision::RecipientConsentRequired,
            CancelDecision::RecipientSignatureInvalid,
        ];
        let mut codes: Vec<&str> = all.iter().map(|d| d.code()).collect();
        codes.sort();
        let n = codes.len();
        codes.dedup();
        assert_eq!(codes.len(), n, "decision codes must be distinct");
        assert!(all.iter().all(|d| !d.message().is_empty()));
    }

    #[test]
    fn endpoint_strings_are_domain_separated() {
        assert_ne!(CANCEL_SENDER_ENDPOINT, CANCEL_RECIPIENT_ENDPOINT);
        assert_ne!(CANCEL_SENDER_ENDPOINT, "withdraw/complete");
        assert_ne!(CANCEL_RECIPIENT_ENDPOINT, "withdraw/complete");
    }
}
