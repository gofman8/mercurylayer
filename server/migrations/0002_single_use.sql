-- Single-use statechain coins (off-chain RGB split/combine tree nodes): once the SE co-signs a
-- terminal spend of such a coin it must refuse any further (conflicting) spend, so SE-honesty is the
-- off-chain double-spend guard. Normal coins keep single_use=false and the existing re-sign behaviour.
ALTER TABLE public.statechain_data ADD COLUMN single_use boolean NOT NULL DEFAULT false;
