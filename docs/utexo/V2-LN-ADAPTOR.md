# V2 Lightning swaps via adaptor signatures — design + blocker

**Status (2026-07-23): DESIGNED + ADVERSARIALLY REVIEWED. BLOCKED for the primary direction on an
external Lightning-layer dependency (PTLC). LN stays on the V1 lane; the V1 lane therefore cannot be
deleted yet.**

This records the design (7-agent design + adversarial-review workflow) for making a **V2 (TES-R)
laddered coin** LN-latchable, which today is refused outright (`clients/libs/rust/src/transfer_sender.rs`
~`:253-266`, the "sdk53 guard"). It is the linchpin for deleting the V1 LN lane. The Mercury-side
cryptography is feasible and cheap; the atomicity for the primary (external-pay) direction is **not
shippable on the pinned LN stack**.

## The wall

The conveyed receiver-paying state `S'` (`clients/libs/rust/src/tesr.rs` `presign_receiver_state`
~`:719-747`) is simultaneously (a) the ownership/value proof the SSP must read to validate *before*
paying the Lightning invoice, and (b) a fully-signed, **broadcastable** exit paying the receiver.
Because these are the same artifact, "SSP validates pre-pay" and "a rolled-back receiver holds a
broadcastable claim" are the *same capability* — so a swap that rolls back lets whoever saw `S'` steal.
Four non-adaptor attempts (documented in `V2-LATCH-FIX.md`) all hit this.

## The fix (Mercury side) — adaptor pre-signature, client-side only

Turn `S'` from a *complete* signature into an **adaptor pre-signature** locked on an EC point `T`: the
SSP can *verify* it (validates ownership+value) but **cannot broadcast** it until it learns the discrete
log `t` with `T = t·G`. The Lightning settlement is engineered to reveal exactly that `t`.

- **Blind-MuSig2 already supports it, client-side.** The stack threads an optional `adaptor` pubkey
  through every layer; Mercury passes `None` at `lib/src/transaction.rs:603`
  (`calculate_musig_session` ~`:567-660`). `Some(T)` folds `T` into the aggregate nonce
  (`session_impl.h:731-738`) so `R_final = R + T` and the SE signs the *adapted, blinded* challenge —
  **oblivious to `T`, no lockbox/enclave change** (`lockbox/src/enclave.cpp:190`; final nonce stripped
  at `transaction.rs:630,637`). Complete with `adapt(pre_sig, t, parity)` (`rust-secp256k1-zkp
  musig.rs:673`); recover with `extract_adaptor` (`:854`).
- **Same-secret construction** (because a SHA256 preimage is *not* a curve discrete log): mint the swap
  secret as a scalar `t ∈ Z_n`, and publish **both** `payment_hash = SHA256(t)` (classic BOLT11) **and**
  `T = t·G`. Knowledge of `t` is then simultaneously the HTLC preimage and the adaptor completion scalar.
  The coordinator mint (`server/src/endpoints/lightning_latch.rs` `create_pre_image` ~`:61-111`) changes
  from "random 32-byte preimage → return hash" to "random scalar `t` → return `(SHA256(t), t·G)`".

## Adversarial findings — what MUST be fixed before this is safe

1. **FATAL — user double-spends the exact-coin PAY path (fixable, but not with the adaptor alone).**
   `presign_receiver_state` co-signs `S'` on a *clone* and **never terminalizes the coin**
   (no `set_spend_budget`; `transfer_sender.rs:302-305` keeps the caller's coin untouched), and the pay
   path reserves ≥2δ runway. So the coin stays fully signable: a malicious user requests **one more**
   co-sign for a self-paying state `S*` at `cur_csv − 2δ` — *lower* CSV than the SSP-paying `S'` at
   `cur_csv − δ` — broadcasts trigger + `S*`, which matures **first** and turns the SSP's completed `S'`
   into a rejected double-spend. **Theft: user keeps the coin AND the SSP paid.** The adaptor only makes
   `S'` non-broadcastable-without-`t`; it does nothing to stop a *lower already-complete* rival.
   **FIX (reuses this session's in-ladder-split mechanism):** terminalize the latched coin
   (`set_spend_budget(sid, finalized+1)`, consumed by `S'`) so `S'` is the SE's only remaining permissible
   co-sign, AND add a fail-closed `get_spend_budget` **terminality check to the SSP pre-pay gate**,
   mirroring `verify_child_bundle`'s `child_terminal` check (`tesr.rs` ~`:1197-1210`, `verify_conveyed_child`
   ~`:537-550`). Also close the **dangling sign/first** window (`server/src/endpoints/sign.rs:244-286`):
   the terminality gate must precede any `S'` sign/first, ≤1 dangling session.
2. **HIGH — the SSP pre-pay "verify `S'` is a valid adaptor pre-sig on `T`" has no primitive.** The SSP
   holds a conveyed *aggregate* pre-sig, not partial sigs + the live session, so the only verifiers
   (`session_impl.h:1132,1197`) don't apply. It needs **new code**: recompute the receiver-paying sighash,
   lift the x-only fin-nonce with correct parity, subtract the registered `T`, and check
   `s'·G == fin_nonce − T + e·P`. The "just flip `None→Some(T)`" framing omits this load-bearing capability.
3. **MEDIUM — `T`/invoice binding.** `T` is chosen client-side, so a sender can lock `S'` on a `T' = t'·G`
   it controls, unrelated to the invoice. The pre-pay gate must check the pre-sig's `T` **equals the
   latch's registered invoice `T`** (in addition to the existing `SHA256(t)==payment_hash` at
   `lightning_latch.rs:241-244`).
4. **MEDIUM — nonce parity** must be threaded from the folding session to `adapt`/`extract` (a mismatch
   silently produces an invalid witness); **adaptor mode must default `None` at every call site** (an
   ordinary transfer taken as `Some(T)` bricks the receiver's coin).

## THE BLOCKER — the sound direction needs PTLC, which the LN stack lacks

The construction is provably sound **only** where `t` lives *outside* Mercury so the blind SE cannot
release it without a real payment — i.e. the **external-BOLT11 PAY** direction with a **PTLC** invoice
(payment point `Q`; set `T = Q`; settling reveals `q = t`). A hash-only BOLT11 publishes only `SHA256(t)`,
never `T`, so the same-secret point is unrecoverable.

**The pinned Lightning stack (`rgb-lightning-node` / LDK) has no GA PTLC support.** For every
**Mercury-minted** lane (receive, and an "internal pay a Mercury-issued invoice" shim), `t` is generated
and custodied by the blind SE and released by `get_preimage` gated **only** on `locked=false AND
expires_at>now()` (`server/src/database/lightning_latch.rs:60-67`) — with **no check that any external
payment occurred**. So on those lanes the adaptor adds **zero** theft resistance over the pre-existing
lock-bit latch the wall already declared insufficient. The provably-sound lane is unshippable; the
shippable lanes are unsound.

⟹ **V2 LN swaps (the atomic external-pay direction) are BLOCKED on Lightning-layer PTLC support** — an
external dependency, not a Mercury-layer fix. **LN swaps remain on the V1 lane** (V1 coins; the sdk53
guard refuses V2 coins), which is safe and works (sdk03 green). **Deleting the V1 lane, and therefore the
"no V1" docs rewrite, are transitively BLOCKED** until PTLC (or an equivalent point-revealing LN
construction) is available.

## Implementation targets (for when PTLC lands)
- Coordinator: `server/src/endpoints/lightning_latch.rs:61-111` mint `t` as a scalar, return `(SHA256(t), t·G)`; store `T` on the latch; `server/src/database/lightning_latch.rs:54-88` custody.
- Client: thread `Option<PublicKey>` adaptor from `presign_receiver_state` (`tesr.rs:719-747`) → `cosign_tier` → `calculate_musig_session` (`lib/src/transaction.rs:603` `None→Some(T)`); **terminalize** the latched coin; lift the sdk53 guard (`transfer_sender.rs:253-266`).
- SSP: `clients/libs/rust-sdk/src/ssp.rs:375-525` add the aggregate-adaptor-pre-sig verifier (finding 2), the `T`-binding check (3), the terminality check (1); complete via `adapt` post-settlement.
- Retained: the delivery-gate + `reconcile` core (`V2-LATCH-FIX.md` §2/§3) still handles the brick half (fold the orphan adaptor co-sign into `superseded_states`).
- E2E (needs `setup_ln_pair` + a PTLC-capable LN build): sender pays via adaptor-locked `S'`; adversarial: no-terminality double-spend REJECT, `T`-mismatch REJECT, unpaid-completion REJECT.

Relates to `V2-LATCH-FIX.md`, `V2-SPLIT-FINDINGS.md`, and the in-ladder-split terminality mechanism
(`verify_child_bundle`).
