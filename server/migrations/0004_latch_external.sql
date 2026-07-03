-- Lightning latch v2: externally-supplied payment hashes (submarine-swap style).
-- pre_image stays NULL for external latches; unlock happens by presenting the preimage.
ALTER TABLE public.lightning_latch ADD COLUMN payment_hash varchar NULL;
