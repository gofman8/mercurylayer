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
/// `sha256(nonce|"transfer/cancel/recipient|<digest>")`) by the key the coordinator RECORDED as this
/// transfer's `new_user_auth_public_key`, where `<digest>` is [`transfer_consent_digest`] over the
/// row's own conveyed material. A signature by any other key is [`Self::Invalid`] — the consent that
/// matters is the recorded recipient's, not whoever the sender chooses to present. A signature bound
/// to a DIFFERENT transfer of the same coin to the same key is [`Self::Stale`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecipientConsent {
    /// No recipient signature was supplied.
    Absent,
    /// A recipient signature was supplied but did not verify: wrong key, malformed or bad
    /// signature, or a nonce that was unknown, expired, or already spent.
    Invalid,
    /// The supplied consent names conveyed material that is NOT this row's current material — or
    /// names none at all (the legacy unbound token shape).
    ///
    /// Distinct from [`Self::Invalid`] because it is NOT a bad signature: it is a good signature
    /// over a transfer instance that no longer exists. Collapsing the two would tell an honest
    /// sender that its recipient signed badly, when what actually happened is that the transfer was
    /// re-addressed out from under a perfectly valid consent. See
    /// [`CancelDecision::RecipientConsentStale`].
    Stale,
    /// A fresh single-use signature by the transfer's recorded recipient auth key, bound to the
    /// material this row currently holds.
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
    /// **The instance binding refused.** A consent by the RECORDED recipient key arrived, but it is
    /// bound to conveyed material this transfer no longer holds — or to no material at all (409).
    ///
    /// This is the refusal that closes MINT -> REOPEN -> CANCEL. A sender may re-address a coin to
    /// the SAME recipient key (`has_open_transfer_to_other_auth` permits it by design), which
    /// DELETEs the old row and mints a fresh `x1`; without this decision a consent the recipient
    /// gave up on transfer T1 would silently withdraw its replacement T2, which the recipient can
    /// see in its mailbox and believes is live.
    ///
    /// Deliberately NOT [`Self::RecipientSignatureInvalid`] (the signature is good) and NOT
    /// [`Self::RecipientConsentRequired`] (a consent did arrive). Both of those would send an honest
    /// sender back to ask for a token it is already holding; this one tells it the truth — the
    /// transfer moved, so ask again for the transfer that exists now.
    RecipientConsentStale,
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
            CancelDecision::RecipientConsentStale => "recipient_consent_stale",
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
            CancelDecision::RecipientConsentStale => {
                "recipient consent is bound to different transfer material; ask the recipient to consent to the transfer that exists now"
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
        RecipientConsent::Stale => CancelDecision::RecipientConsentStale,
        RecipientConsent::Absent => CancelDecision::RecipientConsentRequired,
    }
}

/// The transfer-INSTANCE identity a recipient consent is bound to.
///
/// # Why a consent needs one at all
///
/// The coordinator permits a sender to re-address a coin to the SAME recipient key
/// (`has_open_transfer_to_other_auth` refuses only `new_user_auth_public_key <> $2`), and
/// `insert_new_transfer` implements that as DELETE-then-INSERT with a fresh `x1`. So
/// (coin, recipient key) does NOT identify a transfer across time, and a consent bound only to those
/// — which is what a bare `sha256(nonce|endpoint)` token is, since the nonce carries the coin and
/// the verify key carries the recipient — can be minted against one transfer and spent against its
/// replacement. The recipient sees the replacement in its mailbox and believes it is live. The
/// single-use nonce does not help: the token IS spent exactly once, just not on the transfer it was
/// given for.
///
/// # Why THIS preimage
///
/// `encrypted_transfer_msg` is the only field that is both (a) different for every transfer instance
/// and (b) visible to the recipient. Its `t1` is blinded against the row's `x1`, which the server
/// mints freshly and randomly per open, so a sender cannot reproduce a superseded instance's bytes.
/// And the recipient downloads exactly these bytes from `GET /transfer/get_msg_addr` — it signs over
/// material it has actually seen, not over a server assertion.
///
/// `statechain_id` and the recipient key are folded in as well. They are already bound elsewhere
/// (the nonce row's `statechain_id`, and the DB-read verification key), so this is belt-and-braces:
/// it means the digest alone is a complete statement of what is being abandoned, and a digest quoted
/// in a log or a UI cannot be confused between coins.
///
/// Both sides compute this from the same three inputs; the coordinator recomputes it FROM THE ROW at
/// verify time and never accepts the caller's copy as authoritative.
pub fn transfer_consent_digest(
    statechain_id: &str,
    recipient_auth_pub_key: &str,
    encrypted_transfer_msg: &str,
) -> String {
    use bitcoin::hashes::{sha256, Hash};
    // Length-prefixed, so no concatenation of one field's tail with the next field's head can be
    // reinterpreted as a different triple.
    let preimage = format!(
        "utexo/transfer/cancel/consent/v1|{}:{}|{}:{}|{}:{}",
        statechain_id.len(),
        statechain_id,
        recipient_auth_pub_key.len(),
        recipient_auth_pub_key,
        encrypted_transfer_msg.len(),
        encrypted_transfer_msg,
    );
    sha256::Hash::hash(preimage.as_bytes()).to_string()
}

/// The endpoint string bound into the recipient's signature, for a consent over `digest`.
///
/// Folding the digest into the ENDPOINT rather than adding a parallel field means the existing
/// `sha256(nonce|endpoint)` verification path carries the binding unchanged: there is exactly one
/// place where the signed bytes are constructed, on both sides, so the two cannot drift.
pub fn recipient_consent_endpoint(digest: &str) -> String {
    format!("{CANCEL_RECIPIENT_ENDPOINT}|{digest}")
}

/// True if a supplied consent is not bound to the row's CURRENT conveyed material, and must
/// therefore be refused as [`RecipientConsent::Stale`] before its signature is even checked.
///
/// Deciding this UP FRONT rather than letting the signature simply fail to verify is deliberate, for
/// two reasons. The digest is inside the signed preimage, so a stale token would fail
/// `verify_schnorr` anyway — but that outcome is indistinguishable from a genuinely corrupt
/// signature, and the two need different answers (`ask again` vs `your recipient signed badly`).
/// And the nonce is consumed only inside the verify path, so refusing here means a sender replaying
/// a stale token cannot burn the recipient's fresh nonce as a side effect.
///
/// `supplied` is `None` for a legacy token bound to no material at all: stale, because an unbound
/// consent is indistinguishable from one minted for a superseded transfer. `row` is `None` when the
/// row holds no conveyed material — in which case no consent is required at all and this is not
/// reached, but it fails closed regardless.
pub fn consent_binding_is_stale(supplied: Option<&str>, row: Option<&str>) -> bool {
    // Equality alone is not enough. Two sides that both failed to establish a binding would
    // "match" — an empty digest equals an empty digest — and that is a fail-open, not a consent.
    // Demand a WELL-FORMED digest on both sides, so the only way past this gate is that each side
    // independently hashed something. Not constant-time, and does not need to be: the digest is
    // public material the recipient downloaded and the sender relayed, not a secret.
    fn well_formed(d: Option<&str>) -> Option<&str> {
        d.filter(|d| d.len() == DIGEST_HEX_LEN && d.chars().all(|c| c.is_ascii_hexdigit()))
    }
    !matches!((well_formed(supplied), well_formed(row)), (Some(s), Some(r)) if s == r)
}

/// Length of a [`transfer_consent_digest`] in hex characters — a sha256, so 32 bytes.
const DIGEST_HEX_LEN: usize = 64;

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
    /// The transfer-instance digest the recipient signed over ([`transfer_consent_digest`]).
    ///
    /// The coordinator RECOMPUTES this from the row's own `encrypted_transfer_msg` and refuses a
    /// mismatch as [`CancelDecision::RecipientConsentStale`]; the caller's copy is never
    /// authoritative, it only says which instance the signature claims to be for. Absent means a
    /// legacy unbound token, which is refused on the same rule — see [`consent_binding_is_stale`].
    pub recipient_transfer_digest: Option<String>,
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
        for consent in [
            RecipientConsent::Absent,
            RecipientConsent::Invalid,
            RecipientConsent::Stale,
            RecipientConsent::Valid,
        ] {
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
        for consent in [
            RecipientConsent::Absent,
            RecipientConsent::Invalid,
            RecipientConsent::Stale,
            RecipientConsent::Valid,
        ] {
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
        let consents = [
            RecipientConsent::Absent,
            RecipientConsent::Invalid,
            RecipientConsent::Stale,
            RecipientConsent::Valid,
        ];
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
                    &[RecipientConsent::Absent, RecipientConsent::Stale, RecipientConsent::Valid]
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
        /// The transfer-instance secret. `insert_new_transfer` mints a FRESH `x1`
        /// (`SecretKey::new(&mut rand::thread_rng())`, server/src/endpoints/transfer_sender.rs)
        /// on every open, INCLUDING a re-address to the same recipient key. Modelled as a counter
        /// because all that matters here is that it differs between instances.
        x1: u32,
    }

    impl CoordinatorRow {
        /// The mailbox ciphertext `update_msg` stores. `t1` inside it is blinded against this row's
        /// `x1`, so a re-address — even to the SAME key — necessarily changes these bytes. This is
        /// the material a recipient polling `get_msg_addr` actually downloads, and therefore the
        /// only transfer-instance identity a recipient can bind a signature to.
        fn material(&self) -> Option<String> {
            self.message_posted.then(|| format!("msg[to={},x1={}]", self.recipient, self.x1))
        }

        /// What the coordinator recomputes from the row, and what a recipient computes over the
        /// bytes it downloaded — the SAME production function, which is the point.
        fn digest(&self) -> Option<String> {
            self.material()
                .map(|m| transfer_consent_digest(MODEL_STATECHAIN_ID, self.recipient, &m))
        }
    }

    /// The one coin every Coordinator test operates on.
    const MODEL_STATECHAIN_ID: &str = "model-statechain-id";

    /// A consent token as it exists on the wire between two wallets: minted at a moment in time,
    /// over the material the recipient could actually see at that moment.
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Consent {
        recipient: &'static str,
        /// [`transfer_consent_digest`] over the conveyed material this consent was minted against,
        /// or `None` for a legacy token bound to nothing but the coin and the key.
        digest: Option<String>,
    }

    #[derive(Debug, Default)]
    struct Coordinator {
        row: Option<CoordinatorRow>,
        /// (statechain-scoped) recipient keys that had a cancelled transfer — the tombstone.
        tombstoned: Vec<&'static str>,
        /// Recipients whose claim succeeded. More than one entry here is theft.
        paid: Vec<&'static str>,
        /// Bumped by every `insert_new_transfer`.
        next_x1: u32,
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
            // insert_new_transfer: DELETE then INSERT, with a fresh x1.
            self.next_x1 += 1;
            self.row = Some(CoordinatorRow {
                recipient,
                message_posted: false,
                key_updated: false,
                cancelled: false,
                x1: self.next_x1,
            });
            Ok(())
        }

        /// `POST /transfer/update_msg` — post the mailbox message.
        fn convey(&mut self) {
            if let Some(r) = &mut self.row {
                r.message_posted = true;
            }
        }

        /// What a recipient polling `get_msg_addr` would download right now.
        fn posted_material(&self) -> Option<String> {
            self.row.as_ref().and_then(|r| r.material())
        }

        /// The recipient half of `cancel_consent`: sign over the material currently in the mailbox.
        /// A recipient can only ever bind to what it can SEE, which is exactly the point.
        fn mint_consent(&self, who: &'static str) -> Option<Consent> {
            let digest = self.row.as_ref()?.digest()?;
            Some(Consent { recipient: who, digest: Some(digest) })
        }

        /// `POST /transfer/cancel`, with `consent` naming the key that co-signed (if any).
        ///
        /// Convenience for the mint-and-immediately-spend case: the consent is minted against the
        /// material that is in the mailbox at this instant, which is what an honest cooperative
        /// cancellation does.
        fn cancel(&mut self, consent: Option<&'static str>) -> CancelDecision {
            let token = consent.map(|k| {
                self.mint_consent(k)
                    // Nothing posted (or no row): a legacy token bound to nothing.
                    .unwrap_or(Consent { recipient: k, digest: None })
            });
            self.cancel_with(token.as_ref())
        }

        /// `POST /transfer/cancel` carrying a specific, possibly stale, consent token.
        fn cancel_with(&mut self, consent: Option<&Consent>) -> CancelDecision {
            let (state, recipient) = match &self.row {
                None => (TransferCancelState::empty(), None),
                Some(r) => {
                    let recipient_consent = Self::classify(consent, r);
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

        /// MIRROR OF THE SERVER, kept deliberately faithful — this is the part of the coordinator
        /// under test, and every claim the adversarial tests make rests on it being an honest copy.
        ///
        /// Mirrors `server/src/endpoints/transfer_sender.rs` `transfer_cancel`: the caller-supplied
        /// key is equality-checked against `row.recipient_auth_pub_key`, then the signature is
        /// verified under the RECORDED key with a single-use nonce, and the token is bound to the
        /// row's conveyed material.
        fn classify(consent: Option<&Consent>, row: &CoordinatorRow) -> RecipientConsent {
            let Some(token) = consent else { return RecipientConsent::Absent };
            if token.recipient != row.recipient {
                return RecipientConsent::Invalid;
            }
            // [INSTANCE BINDING] The coordinator recomputes the digest from the row's own conveyed
            // material and refuses anything else, BEFORE the signature is verified so a stale replay
            // cannot burn the recipient's nonce.
            if consent_binding_is_stale(token.digest.as_deref(), row.digest().as_deref()) {
                return RecipientConsent::Stale;
            }
            RecipientConsent::Valid
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

    /// **THE REPLAY THE TOMBSTONE DOES NOT CLOSE.**
    ///
    /// `adversarial_recipient_consent_cannot_be_replayed_against_a_later_transfer` (above) covers
    /// CANCEL -> REOPEN -> REPLAY, which the tombstone refuses at the reopen. This is the OTHER
    /// ordering, MINT -> REOPEN -> CANCEL, where no tombstone exists yet because nothing has been
    /// cancelled:
    ///
    /// 1. Alice opens T1 to Bob's key K and posts the mailbox message.
    /// 2. Alice tells Bob "wrong amount, please consent so I can resend". Bob mints a consent.
    /// 3. Alice WITHHOLDS it and re-addresses the coin to the SAME key K.
    ///    `has_open_transfer_to_other_auth` permits same-key re-addressing by design
    ///    (`AND new_user_auth_public_key <> $2`). `insert_new_transfer` then DELETEs T1 and INSERTs
    ///    T2 with a FRESH x1.
    /// 4. Alice posts T2's message. Bob polls `get_msg_addr`, finds it, and reads it as the promised
    ///    corrected payment. It is structurally valid and freshly blinded.
    /// 5. Alice spends Bob's T1 consent against T2.
    ///
    /// Bob's signature, given to abandon a transfer he had rejected, withdrew one he believed was
    /// live. The single-use nonce does not help: the token is spent exactly once, just not on the
    /// transfer it was minted for. Nothing above requires more than the nonce's 300 s lifetime, and
    /// Alice controls every step in it.
    ///
    /// The fix this test forces is a transfer-INSTANCE binding: the recipient signs over the
    /// material it downloaded, and the coordinator recomputes that from the row.
    ///
    /// # Why this stays a test of the BINDING, and not of the re-address guard
    ///
    /// Step 3 also has a second, independent line of defence:
    /// `exists_msg_for_same_statechain_id_and_new_user_auth_key` refuses a second
    /// `POST /transfer/sender` for the same (coin, recipient key). That guard used to be dead code —
    /// its `AND batch_id = $3` binds `$3` as SQL NULL on the non-batched path, and `NULL = NULL` is
    /// UNKNOWN, so `COUNT(*)` was unconditionally 0 — and it is now spelled
    /// `batch_id IS NOT DISTINCT FROM $3`.
    ///
    /// The `Coordinator` below deliberately does NOT model that guard, for two reasons.
    ///
    /// 1. It does not close the attack. The comparison is NULL-AWARE equality, not a wildcard, so a
    ///    BATCHED T1 followed by a non-batched T2 to the same key is still permitted
    ///    (`'B' IS NOT DISTINCT FROM NULL` is FALSE), and `has_open_transfer_to_other_auth` waves the
    ///    same-key reopen through. The recipient minting a consent cannot see that its transfer is
    ///    batched, so it will happily sign. The binding is what refuses the replay in that ordering.
    /// 2. Layered defences must be tested SEPARATELY or the outer one hides the inner one going
    ///    quiet. Modelling the guard here would make this test pass at `open()` and stop exercising
    ///    `consent_binding_is_stale` at all — which is exactly how a defect like the vacuous SQL
    ///    survives: something upstream happened to be covering for it.
    ///
    /// The guard is tested on its own, against the production query text and under a model of SQL
    /// three-valued logic, in `server/src/database/transfer_sender.rs`.
    #[test]
    fn adversarial_consent_minted_for_one_transfer_cannot_cancel_its_same_key_replacement() {
        let mut c = Coordinator::default();

        // T1: conveyed to Bob's key.
        c.open("bob").unwrap();
        c.convey();
        let t1_material = c.posted_material().expect("T1 was conveyed");
        let consent = c.mint_consent("bob").expect("Bob consents to abandoning T1");
        assert_eq!(consent.digest, c.row.as_ref().unwrap().digest());

        // T2: the same key, re-addressed. Permitted by the coordinator's own guards.
        c.open("bob").expect("same-key re-address is permitted; that is the premise");
        c.convey();
        let t2_material = c.posted_material().expect("T2 was conveyed");
        assert_ne!(
            t1_material, t2_material,
            "a re-address mints a fresh x1, so the conveyed bytes MUST differ — without that there \
             is no instance identity to bind to and the whole fix is impossible"
        );

        // Bob believes T2 is live: it is in his mailbox, uncancelled and unclaimed.
        assert_eq!(
            c.row.as_ref().map(|r| (r.cancelled, r.key_updated, r.message_posted)),
            Some((false, false, true))
        );

        // Alice spends the T1 consent against T2.
        let decision = c.cancel_with(Some(&consent));

        assert!(
            !decision.releases_lock(),
            "a consent minted over T1's conveyed material released T2 — the recipient signed away a \
             transfer he never agreed to abandon. Got {decision:?}"
        );
        // And it must be named as what it is: the signature is GOOD, the transfer under it moved.
        // Reporting it as RecipientSignatureInvalid would tell an honest sender its recipient sent a
        // bad signature, and reporting it as RecipientConsentRequired would tell it no signature
        // arrived. Both send the sender back for a token it already has.
        assert_eq!(decision, CancelDecision::RecipientConsentStale);

        // The transfer Bob actually believes in is untouched, and he can still claim it.
        c.claim("bob").expect("T2 survives the stale consent");
        assert_eq!(c.paid, vec!["bob"]);
    }

    /// The same binding, from the honest side: a consent minted over the material CURRENTLY in the
    /// mailbox still works. The fix must not break the cooperative flow it exists to protect.
    #[test]
    fn consent_minted_over_the_current_material_still_cancels() {
        let mut c = Coordinator::default();
        c.open("bob").unwrap();
        c.convey();
        let consent = c.mint_consent("bob").unwrap();
        assert_eq!(c.cancel_with(Some(&consent)), CancelDecision::Allow);
    }

    /// A LEGACY token — one bound to the coin and the key but to no transfer material, which is
    /// exactly the shape this endpoint shipped with — is refused, not honoured. An unbound consent
    /// is indistinguishable from one minted for a superseded transfer, so it fails closed on the
    /// same rule.
    #[test]
    fn a_consent_bound_to_no_transfer_material_is_refused() {
        let mut c = Coordinator::default();
        c.open("bob").unwrap();
        c.convey();
        let unbound = Consent { recipient: "bob", digest: None };
        let d = c.cancel_with(Some(&unbound));
        assert!(!d.releases_lock());
        assert_eq!(d, CancelDecision::RecipientConsentStale);
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
            CancelDecision::RecipientConsentStale,
        ];
        let mut codes: Vec<&str> = all.iter().map(|d| d.code()).collect();
        codes.sort();
        let n = codes.len();
        codes.dedup();
        assert_eq!(codes.len(), n, "decision codes must be distinct");
        assert!(all.iter().all(|d| !d.message().is_empty()));
    }

    // ==========================================================================================
    // The transfer-instance binding, at the level of the digest itself.
    // ==========================================================================================

    const SID_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const SID_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const KEY_A: &str = "02aaaa000000000000000000000000000000000000000000000000000000000000";
    const KEY_B: &str = "02bbbb000000000000000000000000000000000000000000000000000000000000";

    /// The property the whole fix rests on: a token bound to transfer A does not verify for transfer
    /// B. Every one of the three inputs moves the digest on its own.
    #[test]
    fn the_digest_separates_every_input() {
        let base = transfer_consent_digest(SID_A, KEY_A, "msg-1");
        assert_ne!(base, transfer_consent_digest(SID_B, KEY_A, "msg-1"), "coin must separate");
        assert_ne!(base, transfer_consent_digest(SID_A, KEY_B, "msg-1"), "recipient must separate");
        assert_ne!(
            base,
            transfer_consent_digest(SID_A, KEY_A, "msg-2"),
            "THE ONE THAT MATTERS: a re-address to the SAME key mints a fresh x1, so the conveyed \
             ciphertext changes and the digest must change with it"
        );
        // Deterministic, and a 32-byte hash rendered as hex.
        assert_eq!(base, transfer_consent_digest(SID_A, KEY_A, "msg-1"));
        assert_eq!(base.len(), 64);
        assert!(base.chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// The fields are length-prefixed, so no shifting of a boundary between two adjacent fields can
    /// produce the same preimage. Without this a coin id ending in "x" plus a key starting with "y"
    /// would collide with the same characters split one place over.
    #[test]
    fn the_digest_is_not_confusable_across_field_boundaries() {
        assert_ne!(
            transfer_consent_digest("ab", "cd", "ef"),
            transfer_consent_digest("a", "bcd", "ef"),
        );
        assert_ne!(
            transfer_consent_digest("ab", "cd", "ef"),
            transfer_consent_digest("ab", "c", "def"),
        );
        assert_ne!(transfer_consent_digest("", "", "abc"), transfer_consent_digest("abc", "", ""));
    }

    /// A consent's signed bytes are bound to the digest through the ENDPOINT string, so the sender
    /// leg, the recipient leg, and a recipient leg for a different transfer are all domain-separated
    /// from one another.
    #[test]
    fn the_consent_endpoint_carries_the_binding_and_stays_separated() {
        let d1 = transfer_consent_digest(SID_A, KEY_A, "msg-1");
        let d2 = transfer_consent_digest(SID_A, KEY_A, "msg-2");
        let e1 = recipient_consent_endpoint(&d1);
        let e2 = recipient_consent_endpoint(&d2);

        assert_ne!(e1, e2, "different transfers must not share signed bytes");
        assert_ne!(e1, CANCEL_SENDER_ENDPOINT);
        assert_ne!(e1, CANCEL_RECIPIENT_ENDPOINT, "the bound leg is not the unbound leg");
        assert_ne!(e1, "withdraw/complete");
        assert!(
            e1.starts_with(CANCEL_RECIPIENT_ENDPOINT),
            "the recipient leg must stay recognisably the recipient leg"
        );
    }

    /// Every way a binding can fail to be established is STALE, never silently accepted. The
    /// `None`-supplied case is the legacy token shape and is the one an old client produces.
    #[test]
    fn every_unestablished_binding_is_stale() {
        let d = transfer_consent_digest(SID_A, KEY_A, "msg-1");
        let other = transfer_consent_digest(SID_A, KEY_A, "msg-2");

        assert!(!consent_binding_is_stale(Some(&d), Some(&d)), "the matching case must pass");

        assert!(consent_binding_is_stale(Some(&d), Some(&other)), "superseded transfer");
        assert!(consent_binding_is_stale(None, Some(&d)), "legacy token bound to nothing");
        assert!(consent_binding_is_stale(Some(&d), None), "row has no conveyed material to bind to");
        assert!(consent_binding_is_stale(None, None), "neither side established anything");
        assert!(consent_binding_is_stale(Some(""), Some("")), "empty is not a binding");
        assert!(consent_binding_is_stale(Some(&d), Some("")), "empty row digest never matches");

        // Two sides that both failed to establish a binding must not "match" each other. Equality
        // alone would let any agreed-upon junk through; only a well-formed digest counts.
        assert!(consent_binding_is_stale(Some("none"), Some("none")), "junk == junk is not consent");
        assert!(consent_binding_is_stale(Some("zz"), Some("zz")));
        let truncated = &d[..63];
        assert!(consent_binding_is_stale(Some(truncated), Some(truncated)), "wrong length");
        let non_hex = format!("{}g", &d[..63]);
        assert!(consent_binding_is_stale(Some(&non_hex), Some(&non_hex)), "non-hex");
    }

    /// Stale is its own decision with its own code, and specifically is NOT the two refusals that
    /// would send an honest sender back for a token it already holds.
    #[test]
    fn stale_is_named_apart_from_missing_and_invalid() {
        let s = TransferCancelState {
            recipient_consent: RecipientConsent::Stale,
            ..conveyed_unclaimed()
        };
        let d = decide_transfer_cancel(&s);
        assert_eq!(d, CancelDecision::RecipientConsentStale);
        assert!(!d.releases_lock());
        assert_ne!(d, CancelDecision::RecipientConsentRequired);
        assert_ne!(d, CancelDecision::RecipientSignatureInvalid);
        assert_ne!(d.code(), CancelDecision::RecipientConsentRequired.code());
        assert_ne!(d.code(), CancelDecision::RecipientSignatureInvalid.code());
    }

    /// A stale consent must not become a back door around any TERMINAL state: the ordering of the
    /// guards has to keep claimed/batched/latched winning over it, exactly as it does for the other
    /// consent values.
    #[test]
    fn stale_consent_does_not_reorder_the_terminal_guards() {
        let stale = |extra: TransferCancelState| TransferCancelState {
            recipient_consent: RecipientConsent::Stale,
            ..extra
        };
        assert_eq!(
            decide_transfer_cancel(&stale(TransferCancelState {
                key_updated: true,
                ..conveyed_unclaimed()
            })),
            CancelDecision::AlreadyClaimed
        );
        assert_eq!(
            decide_transfer_cancel(&stale(TransferCancelState {
                is_batched: true,
                ..conveyed_unclaimed()
            })),
            CancelDecision::Batched
        );
        assert_eq!(
            decide_transfer_cancel(&stale(TransferCancelState {
                claim_in_flight: true,
                ..conveyed_unclaimed()
            })),
            CancelDecision::ClaimInFlight
        );
        assert_eq!(
            decide_transfer_cancel(&stale(TransferCancelState {
                already_cancelled: true,
                ..conveyed_unclaimed()
            })),
            CancelDecision::AlreadyCancelled
        );
        // And an UNCONVEYED transfer is unaffected: no consent is required there, so a stale one is
        // simply irrelevant rather than a refusal. Regressing this would break the common cancel.
        assert_eq!(
            decide_transfer_cancel(&stale(opened_not_conveyed())),
            CancelDecision::Allow
        );
    }

    #[test]
    fn endpoint_strings_are_domain_separated() {
        assert_ne!(CANCEL_SENDER_ENDPOINT, CANCEL_RECIPIENT_ENDPOINT);
        assert_ne!(CANCEL_SENDER_ENDPOINT, "withdraw/complete");
        assert_ne!(CANCEL_RECIPIENT_ENDPOINT, "withdraw/complete");
    }
}

// ==================================================================================================
// THE INVARIANT THE COORDINATOR'S SENDER-AUTH ORDERING RESTS ON.
//
// `POST /transfer/cancel` authenticates the SENDER leg against the key that OPENED the transfer
// (recorded on the row by migration 0011) rather than against the coin's LIVE auth key, because
// claiming rotates the live key to the recipient's — which used to leave a sender unable to
// authenticate about its own transfer, and made `AlreadyClaimed` unreachable for the ordinary case
// it exists to describe.
//
// That ordering is safe only because of a property of THIS function, so the property is pinned here
// rather than argued in a comment over there:
//
//     EVERY decision that releases the pending-transfer lock requires `key_updated == false`.
//
// The coordinator's side of the argument is that the coin's auth key and `key_updated` move in ONE
// transaction (`update_statechain`), so `key_updated == false` implies the live key has not moved
// since the row was opened, i.e. the recorded opener IS the live key. Put together: wherever a
// lock-RELEASING decision is possible, the two keys are the same key, and the set of callers who can
// release the lock is byte-for-byte the set that could release it before. A superseded key can only
// ever reach a decision that REFUSES.
//
// If a future decision ever releases the lock on a claimed row, this test fails — and the
// coordinator's ordering must be revisited before it ships.
// ==================================================================================================
#[cfg(test)]
mod lock_release_requires_an_unclaimed_row_tests {
    use super::*;

    /// Every reachable coordinator state, exhaustively: 2^6 flag combinations x 4 consent values.
    fn every_state() -> Vec<TransferCancelState> {
        let mut out = Vec::new();
        for bits in 0u8..64 {
            for consent in [
                RecipientConsent::Absent,
                RecipientConsent::Valid,
                RecipientConsent::Invalid,
                RecipientConsent::Stale,
            ] {
                out.push(TransferCancelState {
                    row_exists: bits & 1 != 0,
                    key_updated: bits & 2 != 0,
                    already_cancelled: bits & 4 != 0,
                    is_batched: bits & 8 != 0,
                    message_posted: bits & 16 != 0,
                    claim_in_flight: bits & 32 != 0,
                    recipient_consent: consent,
                });
            }
        }
        out
    }

    /// **THE INVARIANT.** No claimed row is ever releasable.
    #[test]
    fn every_lock_releasing_decision_requires_an_unclaimed_row() {
        let mut releasing = 0usize;
        for state in every_state() {
            let d = decide_transfer_cancel(&state);
            if d.releases_lock() {
                releasing += 1;
                assert!(
                    !state.key_updated,
                    "{d:?} releases the pending-transfer lock on a CLAIMED row ({state:?}). The \
                     coordinator authenticates a cancellation's sender leg against the key that \
                     OPENED the transfer, and that is only as strict as the live-key check because \
                     a claimed row can never be released. Revisit \
                     `endpoints::transfer_sender::transfer_cancel`'s ordering before shipping this."
                );
                assert!(state.row_exists, "{d:?} released a lock that does not exist ({state:?})");
            }
        }
        assert!(releasing > 0, "the sweep must actually reach the releasing decision");
    }

    /// And the corollary the ordering actually depends on: on a CLAIMED row the answer is always
    /// `AlreadyClaimed`, whatever else is true of it — so the only thing a superseded opener key can
    /// ever obtain is the 410 it is entitled to.
    #[test]
    fn a_claimed_row_answers_already_claimed_whatever_else_is_true_of_it() {
        for state in every_state().into_iter().filter(|s| s.row_exists && s.key_updated) {
            assert_eq!(
                decide_transfer_cancel(&state),
                CancelDecision::AlreadyClaimed,
                "{state:?}"
            );
        }
    }

    // ==============================================================================================
    // THE FOUR-ROW TABLE, EACH ROW REACHED.
    //
    // The module docstring specifies cancellation as four rows. Row 3 was unreachable in the
    // coordinator for the ordinary case (a claim rotated the auth key out from under the sender), so
    // each row is now pinned as a state that a real coordinator read can produce, with the decision
    // it must produce. `cancel_status`'s side of it — the HTTP status each row maps to — is pinned in
    // `server::endpoints::transfer_sender::cancel_table_row_tests`.
    // ==============================================================================================

    /// ROW 1: opened, message never posted. The sender alone may withdraw it.
    #[test]
    fn row_1_opened_never_conveyed_is_the_senders_alone() {
        let s = TransferCancelState { row_exists: true, ..TransferCancelState::empty() };
        assert_eq!(decide_transfer_cancel(&s), CancelDecision::Allow);
    }

    /// ROW 2: message posted, unclaimed. The recorded recipient must co-sign.
    #[test]
    fn row_2_conveyed_and_unclaimed_needs_the_recipient() {
        let s = TransferCancelState {
            row_exists: true,
            message_posted: true,
            ..TransferCancelState::empty()
        };
        assert_eq!(decide_transfer_cancel(&s), CancelDecision::RecipientConsentRequired);
    }

    /// ROW 3: claimed. Never cancellable — and, since migration 0011, actually REACHABLE by the
    /// sender that opened the transfer, which is the only party that ever asks.
    #[test]
    fn row_3_claimed_is_terminal_and_says_so() {
        let s = TransferCancelState {
            row_exists: true,
            key_updated: true,
            message_posted: true,
            ..TransferCancelState::empty()
        };
        assert_eq!(decide_transfer_cancel(&s), CancelDecision::AlreadyClaimed);
        assert!(!decide_transfer_cancel(&s).releases_lock());
    }

    /// ROW 4: batched (lightning latch). Never cancellable; the latch's preimage decides whether the
    /// payment happened, and recipient consent is not the right authorization for that.
    #[test]
    fn row_4_batched_is_terminal_and_consent_cannot_override_it() {
        for consent in [RecipientConsent::Absent, RecipientConsent::Valid] {
            let s = TransferCancelState {
                row_exists: true,
                is_batched: true,
                message_posted: true,
                recipient_consent: consent,
                ..TransferCancelState::empty()
            };
            assert_eq!(
                decide_transfer_cancel(&s),
                CancelDecision::Batched,
                "a batched transfer must not be cancellable even WITH consent ({consent:?})"
            );
        }
    }
}
