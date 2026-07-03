-- Stage 4: per-coin epoch deadline (unix seconds). The SE refuses to co-sign any NEW spend of a coin
-- once its own clock passes this deadline, so the owner must transact or exit before then. Unilateral
-- exit needs no SE co-signature. NULL = no epoch (the coin is co-signable indefinitely), so existing
-- rows and non-epoch coins are unaffected.
ALTER TABLE public.statechain_data ADD COLUMN epoch_deadline bigint;
