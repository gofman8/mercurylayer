-- FATAL-B (in-ladder split decoy-counter defense): bind statechain_id -> aggregate AUTHORITATIVELY.
--
-- The server stored `server_public_key` per coin but NOT the owner's signing share, so it could not
-- compute or record the coin's aggregate key. A receiver therefore had to derive the aggregate a census
-- runs against from the SENDER-supplied `user_public_key` (transfer_msg) — and a rogue-key decomposition
-- (owner_share := A_target - enclave_share) then lets a split child point its parent-reference at a decoy
-- statechain_id whose counter conveniently balances. Recording the owner share and the resulting
-- aggregate x-only key (UNIQUE) lets the receiver verify a coin's on-chain aggregate against an
-- authoritative, per-sid, non-reusable server record, and closes the decoy path (an aggregate can be
-- registered at exactly one sid).
--
-- Nullable + UNIQUE (Postgres permits multiple NULLs) so pre-migration coins and old clients are
-- unaffected; only V2-native deposits that send `user_public_key` get a bound aggregate. The per-co-sign
-- owner-share binding on the enclave lane (which stops a decoy counter being PUMPED under a rogue share)
-- is a separate, enclave-side change tracked with the SGX work.
ALTER TABLE public.statechain_data ADD COLUMN IF NOT EXISTS user_public_key bytea NULL;
ALTER TABLE public.statechain_data ADD COLUMN IF NOT EXISTS aggregate_xonly bytea NULL;
ALTER TABLE public.statechain_data ADD CONSTRAINT statechain_data_aggregate_xonly_ukey UNIQUE (aggregate_xonly);
