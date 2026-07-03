-- Terminal-spend enforcement for off-chain tree nodes: once set, the SE refuses any co-signature
-- beyond the budget. Set by the owner right before a structural spend (split/combine), making the
-- node one-shot — Spark's "one spend per node" on a single SE.
ALTER TABLE public.statechain_data ADD COLUMN sig_budget integer NULL;
