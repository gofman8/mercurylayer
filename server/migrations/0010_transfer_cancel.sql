-- Transfer cancellation: let a sender withdraw an opened transfer that nobody has claimed, so the
-- coin becomes spendable again, WITHOUT reopening the hole the pending-transfer lock closes.
--
-- The lock (has_open_transfer) is the only thing stopping a still-owner sender from co-signing a
-- rival state while a conveyed-but-unclaimed recipient holds claimable material. Cancellation
-- releases that lock, so the authorization for it is decided by lib/src/transfer/cancel.rs: the
-- sender alone may withdraw a transfer whose mailbox message was never posted; once the message IS
-- posted, the RECORDED recipient must co-sign, because the coordinator cannot tell "not downloaded"
-- from "downloaded and about to claim" (get_msg_addr is unauthenticated and non-destructive).
--
-- Three pieces of state:
--
-- 1. `cancelled_at` — the tombstone flag on the live row. Every reader of an OPEN transfer
--    (has_open_transfer, has_open_transfer_to_other_auth, get_x1pub, get_auth_pubkey_and_x1,
--    get_statechain_transfer_messages, exists_msg_for_same_statechain_id_and_new_user_auth_key)
--    filters on `cancelled_at IS NULL`, so a cancelled transfer stops holding the lock, stops
--    serving its message, and stops being claimable. The cancel also NULLs `x1` and
--    `encrypted_transfer_msg`, so even a reader that forgot the filter cannot complete the handover.
--
-- 2. `claim_started_at` — the claim latch. POST /transfer/receiver stamps it atomically BEFORE
--    calling the enclave, and the cancel refuses while it is fresh. Without this, a recipient could
--    co-sign a cancellation and then race a claim: the enclave rotates its share on keyupdate and
--    cannot be told to roll back, so a cancel that won the coordinator race but lost the enclave
--    race would either brick the coin or mark a LATER recipient's row as claimed. Both guards are
--    single atomic UPDATEs on the same row with complementary WHERE clauses, so under READ
--    COMMITTED exactly one of {claim, cancel} can win. The window is short (see
--    CLAIM_LATCH_SECS) and /transfer/receiver releases the latch explicitly on its error paths, so
--    a failed claim does not wedge retries.
--
-- 3. `statechain_transfer_cancelled` — the durable record, because `insert_new_transfer` DELETEs
--    the live row when the coin is re-addressed and would otherwise erase the recipient's only
--    error signal. It serves two purposes:
--      * a recipient that proves it holds the recorded key gets a typed 410 "cancelled" instead of
--        a bare 404 that is indistinguishable from an idle mailbox;
--      * `POST /transfer/sender` refuses to re-address a coin to a key that already had a cancelled
--        transfer of it. Transfer addresses are single-use by construction (new_transfer_address
--        mints a fresh coin each call), so this costs an honest sender nothing, and it removes the
--        only way stale conveyed material (blinded against the OLD x1) could ever meet a fresh x1 —
--        which would rotate the enclave share to a wrong value and brick the coin.
--
-- No enclave change: cancellation is coordinator-only bookkeeping. The SE is never told that a
-- transfer existed, was cancelled, or which leg is which — it stays blind.

ALTER TABLE public.statechain_transfer ADD COLUMN IF NOT EXISTS cancelled_at TIMESTAMPTZ NULL;
ALTER TABLE public.statechain_transfer ADD COLUMN IF NOT EXISTS claim_started_at TIMESTAMPTZ NULL;

CREATE TABLE IF NOT EXISTS public.statechain_transfer_cancelled (
    id serial4 NOT NULL,
    statechain_id varchar NOT NULL,
    new_user_auth_public_key bytea NOT NULL,
    -- whether claimable material had actually been posted when the cancel landed (i.e. whether the
    -- recipient's co-signature was required); kept for audit, not read by any authorization path.
    message_was_posted boolean NOT NULL DEFAULT false,
    recipient_consented boolean NOT NULL DEFAULT false,
    cancelled_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT statechain_transfer_cancelled_pkey PRIMARY KEY (id)
);

CREATE INDEX IF NOT EXISTS idx_statechain_transfer_cancelled_sid
    ON public.statechain_transfer_cancelled (statechain_id);
CREATE INDEX IF NOT EXISTS idx_statechain_transfer_cancelled_sid_key
    ON public.statechain_transfer_cancelled (statechain_id, new_user_auth_public_key);
