-- External review finding 2: /sign/second binds the challenge to the server nonce BEFORE calling
-- the lockbox (that ordering is REQUIRED — the sticky challenge is the MuSig2 nonce-reuse guard),
-- but `challenge IS NOT NULL` was ALSO the only "finalized signature" signal. So a transient
-- lockbox failure AFTER the challenge write consumed the owner's spend budget / single-use quota
-- with no usable signature, forcing a needless unilateral exit.
--
-- Split the two states: `challenge` stays the (pre-enclave) replay/nonce-reuse binding, while
-- `partial_sig_issued` is written ONLY after the lockbox returns a valid partial signature. The
-- spend-budget / single-use counter now counts issued signatures, so a failed issuance no longer
-- burns the owner's cooperative transition.
--
-- Backfill CONSERVATIVELY: every pre-migration row with a bound challenge is treated as issued, so
-- no coin's existing terminal/budget state loosens (an under-count would re-open a terminal node to
-- a second conflicting co-signature — INV-19 fork). New rows default to false and are flipped true
-- only on lockbox success.
ALTER TABLE public.statechain_signature_data ADD COLUMN partial_sig_issued boolean NOT NULL DEFAULT false;
UPDATE public.statechain_signature_data SET partial_sig_issued = true WHERE challenge IS NOT NULL;
