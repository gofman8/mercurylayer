-- Derived-slot vouchers: deposit tokens the SE mints ITSELF (never the token server) for
-- statechain slots created by SE-co-signed flows over an EXISTING statechain — off-chain split
-- pieces/change, combine outputs, refresh re-anchors. Such slots re-house value already inside
-- the SE, so they are not charged the on-chain onboarding fee. `derived_from` records the parent
-- statechain_id whose owner authenticated the request; NULL = a normal onboarding token. The
-- per-parent LIFETIME issuance cap (max_derived_tokens_per_statechain) is counted over this column.
ALTER TABLE public.tokens ADD COLUMN derived_from varchar NULL;
CREATE INDEX IF NOT EXISTS idx_tokens_derived_from ON public.tokens (derived_from);
