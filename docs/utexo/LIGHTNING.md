# Lightning — the HODL-invoice latch

> ## ⚠️ Direction of travel: ONE COIN TYPE
>
> The sats lanes below run on the **laddered** shape and are unaffected. The **colored** lane (§9
> residual 6) rides an *un-laddered* carrier, and that shape is being removed — the decided direction
> is a single coin type. When CTES-R colours the tiers, the colored LN lane moves onto the ladder too
> and the "rides an un-laddered coin by design" note below stops being true.
>
> Status: gate passed ([CTESR-GATE.md](CTESR-GATE.md)), foundation landed, **colouring not yet
> wired** — so the description below is accurate as-built.

**Normative.** This is how Lightning works: both directions, exact and non-exact amounts, running on
the TES-R ladder over the pinned `rgb-lightning-node` `pr-90` (LDK 0.2.2) stack. No new cryptographic
primitive, no enclave change for the core.

> **Two designed items were never built** and are flagged in place rather than quietly dropped: the
> `key_updated` tightening of `get_preimage` (§3) and the `lock_expiry` clock reconciliation (§9
> residual 3). Residual 3 is **open and uncovered** — treat it as the top open item of this design.

## 1. What ships

| Direction | Amount | Entry point | Evidence |
|---|---|---|---|
| PAY (coin → LN) | exact | `pay_lightning_invoice` | `sdk63` |
| PAY | non-exact | `pay_lightning_invoice_inladder` | `sdk65` |
| RECEIVE (LN → coin) | exact | `create_receive` | `sdk64` |
| RECEIVE | non-exact | `create_receive` → in-ladder split | `sdk67` |
| PAY rollback | non-exact | booking rolled back, whole parent recovered | `sdk66` |
| PAY rollback | exact | `reclaim_lightning_payment` | `sdk68` |

`sdk53` pins that the old refusal guard — which blocked latching a laddered coin at all — is gone.
`tb04` covers the raw latch primitive; `sdk24`/`sdk25` cover the cancel and delayed-claim paths.

Both lanes run on **laddered** coins. The colored (RGB) lane rides an **un-laddered** carrier by
design — a plain tier spend would destroy the allocation (terminal freeze, [PROTOCOL.md](PROTOCOL.md)
§5.10, pinned by `sdk52`). That is a current shape, not a leftover lane; see
[README](README.md) "One protocol, two coin shapes".

## 2. Feasibility — verified against `rgb-lightning-node` `pr-90` (LDK 0.2.2 fork)

| Primitive | Status | Evidence |
|---|---|---|
| **HODL invoice** (hold → settle/cancel) | **AVAILABLE** | `InvoiceType::Hodl` `src/ldk.rs:209`; `/lninvoice` with external `payment_hash`; settle `/claimhodlinvoice` (`routes.rs:2408`); refund `/cancelhodlinvoice` (`routes.rs:2334`); harness `claim_hodl(hash, preimage)` `rln.rs:230` |
| BOLT11 + preimage | AVAILABLE | `/lninvoice`, `/sendpayment`; `sdk21`/`sdk63`/`sdk64` green |
| Keysend (spontaneous) | AVAILABLE | `/keysend` `routes.rs:3363` |
| Custom TLV records | ABSENT (not exposed) | `spontaneous_empty()` `routes.rs:3441` — would need a new API field |
| **PTLC / payment points** | **ABSENT** | no matches in `src/`; `payment_point` is a BOLT3 basepoint only |

Any design coupling the coin to a **SHA256 preimage** (HODL, plain preimage swap, keysend) is fully
supported. This design uses only HODL + BOLT11-preimage.

**Why not adaptor signatures.** An adaptor-signature design would additionally remove the
SSP-broadcast-on-rollback trust in the PAY direction (soundness against a *Byzantine* operator). It
is unshippable: it requires an external PTLC, the one primitive the pinned stack does not expose.
And on every *shippable* (Mercury-minted / hash-only) lane the adaptor buys **zero** trustlessness.
The rejected writeup treated the PAY-direction wall — a conveyed receiver-paying state `S'` is
simultaneously the ownership proof the SSP reads pre-pay *and* a broadcastable exit — as a defect
adaptors would fix. It is not a defect this design introduces; see §4.

## 3. RECEIVE (LN → coin)

The SSP fronts **its own** coin under a HODL invoice.

1. SSP mints a coin transfer to the user, latched under `batch_id`; the SE mints the preimage
   (`post_paymenthash`, `server/src/endpoints/lightning_latch.rs:61`) and returns `hash`.
2. The user pays a HODL invoice carrying that `hash`; the incoming HTLC is **HELD** at the SSP's node
   (`notify_claimable_hodl_invoice`), not settled.
3. The user completes the key handover (`/transfer/receiver`) — the coin is now the user's.
4. **Only then** does the SE release the preimage to the SSP, which settles the HTLC and gets paid.
   If the user never completes, the SSP calls `/cancelhodlinvoice`, the payer is refunded, and the
   coin was never released.

The SSP owns the coin throughout its risk window and the HTLC is held before release: **the user
cannot be robbed and the SSP cannot be robbed.** RECEIVE needs no operator trust for safety.

### The completion bind that was designed but not built

`get_preimage` (`server/src/database/lightning_latch.rs:60-67`) releases on
`locked = false AND expires_at > now()`. The design proposed additionally requiring
`key_updated = true`, binding the SSP's payout to the coin actually being delivered.

**That must never be applied globally — it deadlocks the `sender-settles-first` lane.** Two shipped
flows retrieve the preimage in opposite orders relative to the receiver's key update:

- **`settle_receive`** (`ssp.rs:639-661`): the SSP calls `confirm_pending_invoice` (clearing
  `locked2`), the receiver claims in the background (clearing `locked` and, with the batch now fully
  unlocked, setting `key_updated = true`), then the SSP **retries** `retrieve_pre_image`. Here
  `key_updated = true` precedes retrieval, so the bind would be safe.
- **`settle_lightning_swap`** (the direct P2P lane, `clients/libs/rust-sdk/src/lightning.rs:103` —
  `confirm_pending_invoice` then `retrieve_pre_image` in one call): the receiver's *first* claim
  clears `locked` but leaves `key_updated = false` (the batch is still `locked2`); the **sender**
  then settles, which clears `locked2` *and* retrieves the preimage — before the receiver completes.
  A global `key_updated` gate deadlocks: settle needs the preimage → the preimage needs
  `key_updated` → `key_updated` needs `locked2 = false` → which settle itself provides. Circular.

**What shipped: nothing.** `get_preimage` is unchanged; no `key_updated` join, scoped or global, was
added. RECEIVE therefore rests on the **coordinated-clock atomicity** already in place
(`ssp.rs:631-638`, audit [2]/[5]): the receiver's claim gate and the SSP's `get_preimage` are bound
to the **same** latch expiry, set shorter than the payer's HODL HTLC. Either the receiver completes
in-window and the SSP can settle, or both fail and the payer is refunded. That is pinned live by
`sdk25` (delayed claim: the latch expires ⟹ both the claim and the preimage retrieval fail) and
`sdk24` (cancel path). The safety claim stands on its own evidence; only the optional strengthening
is absent.

> **The deadlock caveat is itself untested.** Its evidence was `sdk03`, deleted with the pre-TES-R
> lane. `settle_lightning_swap` still exists and still retrieves the preimage before the receiver
> completes; `tb04` exercises the same latch primitives but in the *receiver-completes-first* order,
> so it does not cover this ordering. Anyone adding a `key_updated` gate must scope it to the
> SSP-fronts-the-coin flow, and has **no regression test** to catch the mistake.

## 4. PAY (coin → LN)

The user pays an external merchant BOLT11 (hash `H`) with a coin.

1. User calls `create_external_hash_latch(H)` (`post_paymenthash_external`,
   `lightning_latch.rs:181`); the SE stores `H` and never learns the preimage.
2. User conveys the coin to the SSP (`S'` pays the SSP), latched under `batch_id`, `locked2` pending.
3. **SSP pre-pay census — the load-bearing check.** Before `send_payment`, the SSP decrypts the
   bundle (`peek_pending_transfers`) and runs the full `verify_bundle` /
   `num_sigs == disclosed-ladder` census, reading the **enclave-authoritative** `sig_count`
   (`GET /signature_count`), *not* the coordinator DB. A hidden lower-CSV `S*` inflates `sig_count`
   beyond the disclosed ladder → mismatch → **refuse to pay**, before the irreversible LN leg. The
   pending-transfer lock (`server/src/database/transfer_sender.rs:60-70`) blocks any *new* co-sign
   during the window.
4. SSP pays the merchant; settlement reveals the preimage; `unlock_by_preimage(batch_id, preimage)`
   (`lightning_latch.rs:215`) clears `locked2` and the transfer completes with the SSP owning the coin.

**Do not delivery-gate the SSP-mediated PAY transfer** — `get_msg_addr` must stay servable so the
census can run. The `locked2` delivery gate is retained **only** for the direct P2P latched lane,
where there is no trusted operator to lean on.

### Serving `S'` pre-pay is the status quo, not a new exposure

The residual — the SSP holds a broadcastable receiver-paying `S'` and is trusted not to broadcast it
on a rolled-back swap — is the trust model the statechain already had. **Verified in code:**
`get_msg_addr` → `get_statechain_transfer_messages`
(`server/src/database/transfer_receiver.rs:122-125`) selects `encrypted_transfer_msg` with **no
`locked`/`locked2` filter**, and the SSP value gate `peek_pending_transfers` decrypts the branch
pre-pay (`clients/libs/rust/src/transfer_receiver.rs`, `sdk37`). The deleted pre-TES-R LN lane did
exactly this. The residual is borne by the **operator**, not by the untrusted user.

## 5. Non-exact amounts — latched child conveyance

Real invoices are non-exact, and splitting a laddered coin yields an in-ladder-split **child**
conveyed via `convey_child_bundle` (a mailbox/adopt path censused by `verify_conveyed_child`), which
the exact lane's `verify_bundle` census does not handle. Three variants were designed and
adversarially reviewed; two were disqualified — `promote-then-standard` needs a forbidden child
reopen or an on-chain re-anchor, and `ssp-change` enlarges the operator-trust bar to the whole
over-collateralized coin.

**Why latched-child-conveyance is the smallest sound change:** `convey_child_bundle` already calls
`get_new_x1`, so the child gets a real `statechain_transfer` row — the exact lane's HODL latch
(`create_external_hash_latch`, `unlock_by_preimage`, `is_all_coins_unlocked`, the `locked`/`locked2`
bits) reuses **verbatim** on the child sid. Critically there is **no reopen, no `sig_count`
decrement, and no new SE co-sign on the child at claim** — a handover rotates key shares, it does not
add a state — so the orphan-`S'` brick of §7 is sidestepped and no SGX signing change is needed.

Received split children are **first-class** ([CHILDREN.md](CHILDREN.md), `sdk60`, `sdk17`): the SSP
adopting the latched piece becomes a genuine co-owner of `A_child` and the sender is permanently
locked out. The latched piece is additionally kept **terminal** (§6), so in practice the SSP's only
spend of it remains the pre-signed exit.

**Trust delta versus the exact lane: none in magnitude.** On rollback the SSP can broadcast the
piece's `SP → ext_child → state_child` to take the *piece* (`invoice + fee`, at most the exact lane's
whole-coin exposure), at the same operator-trust bar. The change slice is trustless (self-owned,
un-conveyed, unilaterally exitable), and there is no double recovery — piece and change share
`X_m.out[0]`. RECEIVE adds **zero** trust: the piece pays the user, who censuses and exits it.

### The value-binding fix (a live theft, closed)

`verify_conveyed_child` returned the *sender-declared* `cb.child_state.out_value`, and
`verify_child_bundle`'s Model-A check bound only `st_out0`'s **key**, never its value —
`verify_tier_cosigned` binds the co-sign to the INPUT amount, not the output split, and the blind SE
co-signs any distribution. So a payer could craft `state_child` paying the receiver a few sats while
declaring a large `out_value` (remainder to a second output back to itself); any value gate trusting
the declared field pays the full invoice for a near-worthless piece. **This was live on the shipped
child census, not only on non-exact LN.** Fixed by binding `st_out0.value == cb.child_state.out_value`
in `verify_child_bundle`, which makes the returned value trustworthy. Adversarial regression:
`sdk65` case A.

### Admission floor

A child funds its **own** two tiers plus dust, so the piece must clear
`min_child_value` (1306 sat at 2 sat/vB) — not the old backup-fee floor. Admitting below it
terminalizes the parent and *then* fails, stranding it. Sizing goes through `tier_out_total` /
`committed_fee_for_outputs` / `min_child_value` (`clients/libs/rust-sdk/src/transfer.rs`,
`mercurylib::tesr`).

## 6. Terminalization — optional everywhere except the latched piece

The user-double-spend concern is closed by the census (at claim for RECEIVE, at pre-pay for PAY) plus
the pending-transfer lock. Making terminalization *mandatory* would reintroduce a reclaim-bricking
griefing vector: a terminalized coin cannot be reclaimed off-chain on rollback, forcing an on-chain
exit.

**One carve-out ships.** The LN-latched piece is the single coin that stays terminalized, because it
is *deliberately* left unclaimed until a Lightning preimage lands — precisely the window the
pending-transfer lock does not cover (the lock expires with the batch, and the receiver cannot
complete the handover until the latch releases). So for the latched lane, and only there, the piece
keeps `set_spend_budget(piece_sid, 0)`: the SE will co-sign nothing further over it, closing the
post-expiry rival window permanently (`clients/libs/rust-sdk/src/transfer.rs`, the `[LN carve-out]`
block in `in_ladder_pay`).

> **Ordering constraint:** `set_spend_budget` authenticates with the piece's own auth key, so it must
> run *before* conveyance, while the sender still holds it.

This is a local exception, not a return to mandatory terminalization: the parent and the change slice
are untouched, and rollback still recovers the whole parent (`sdk66`) because the split itself is
un-broadcast.

## 7. Failure and rollback

On an **un-laddered** coin, `reclaim_lightning_payment` (a self-transfer) returns a fully re-usable
coin. On a **laddered** coin the naive self-transfer **bricks**: the failed latch already co-signed
the orphan `S'` (`sig_count` +1), so the reclaim's own presign leaves the disclosed ladder one tier
short of the enclave `sig_count` and `verify_bundle` rejects. Handled by direction:

- **Non-exact PAY** (`sdk66`): the split is un-broadcast, so `pay_lightning_invoice_inladder` rolls
  back the optimistic booking and the user recovers the **whole parent**.
- **Exact whole-coin PAY** (`sdk68`): `reclaim_lightning_payment` detects a laddered coin and restores
  it locally as exitable instead of attempting the bricking self-transfer.

Both leave the coin recoverable via `unilateral_exit`, on-chain, at the same operator-trust bar the
success path uses.

**Residual:** re-transfer stays orphan-bricked until a `refresh()` re-anchor. Restoring full
*off-chain* reuse needs a **scoped coordinator-side `sig_count` reconcile**: on an authenticated
reclaim of a rolled-back latch, the SE decrements the coin's `sig_count` by exactly the orphan `S'`
it co-signed for that batch — bounded, single-use, batch-scoped — so the reclaim's self-transfer
census balances again. That is a server workstream needing its own security review (a
coordinator-authoritative count decrement must be tightly scoped so it can never hide a real state).
`refresh()` works today but is an on-chain re-anchor, i.e. the same cost class as an exit. Optional
enclave terminalization (§10) does **not** help here — terminalizing the latched coin makes rollback
worse, since the coin's only permitted spend becomes the SSP-paying `S'`.

**Coverage note:** the un-laddered self-transfer branch of `reclaim_lightning_payment` has no live
E2E — its old one (`sdk18`) was deleted with the pre-TES-R lane, and since every plain deposit is now
laddered, the branch remains only for the un-laddered carrier shape. The laddered branch is `sdk68`;
the non-exact rollback is `sdk66`.

## 8. Trust assumption

> **RECEIVE (LN → coin)** needs no operator trust for safety: the SSP fronts its own coin, the
> inbound HTLC is HELD before the coin is released, and the SE-minted preimage is released only
> inside a latch window that expires BEFORE the payer's HODL HTLC — the receiver's claim gate and the
> SSP's `get_preimage` are bound to that same latch clock, so either the receiver completes in-window
> (and the SSP can settle) or both fail and the payer is refunded, the coin never having left the SSP
> (`sdk25`, `sdk24`). *(The stronger `get_preimage`-gated-on-`key_updated` bind was designed but not
> built — see §3.)*
>
> **PAY (coin → LN)** rests on the existing statechain operator-trust model: after pre-pay validation
> the SSP — co-located with the coordinator and blind SE as one operator trust domain — holds a
> broadcastable receiver-paying state `S'`, and the design trusts the operator not to broadcast it on
> a rolled-back swap. The **untrusted counterparty (the user) cannot be robbed** — its coin is frozen
> by the pending lock, reclaimable after `batch_timeout`, and `locked2` clears only on the genuine
> merchant preimage — and **cannot rob the SSP**, because the pre-pay `num_sigs` census, read from
> the enclave-authoritative `sig_count` rather than the coordinator DB, rejects any coin carrying a
> hidden lower-CSV state before the invoice is paid. This is **no worse than the statechain trust
> model already in force**. Adaptor signatures would additionally have removed the
> SSP-broadcast-on-rollback trust, but only on a PTLC-external lane that is unshippable on the pinned
> LDK 0.2.2 stack.

Drop-in counterpart in [TRUST-MODEL.md](TRUST-MODEL.md).

## 9. Residual risks

1. **PAY operator-trust residual** — the SSP holds a broadcastable `S'` post-validation; equals the
   statechain operator bar; the user is fully protected.
2. **Coordinator collusion on the freeze** — the census reads the tamper-resistant enclave
   `sig_count`, but the Postgres freeze itself is coordinator-enforced. Full prevention is §10.
3. **Clock reconciliation — OPEN AND UNCOVERED.** A lapsed freeze mid-HTLC is a free option. The
   planned mitigation (`statechain_transfer.lock_expiry TIMESTAMPTZ`, read by `has_open_transfer` and
   both sign gates instead of the hardcoded window, set at `get_new_x1`, enforcing
   `lock_expiry ≥ HTLC CLTV + grace`) was **never built**, and the test that would have pinned it
   does not exist. `has_open_transfer` (`server/src/database/transfer_sender.rs:60-78`) still uses the
   hardcoded `INTERVAL '1 hour'`, narrowed to `batch_time + batch_timeout` for latched transfers. The
   risk is bounded in practice because the HTLC CLTVs in use sit inside that window, but the
   invariant is **asserted nowhere**. Top open item of this design.
4. **Capital lockup / griefing** — a frozen coin is tied up for the pending-transfer lock window (the
   batch timeout for latched transfers, otherwise 1 hour); bounded DoS, mitigable with rate limits or
   bonds (see audit [15] auth-nonce griefing).
5. **SE / coordinator liveness at settlement** — fail-closed gates stall rather than mis-resolve;
   RECEIVE forbids `cancel_hodl` after release; PAY retries the handover idempotently.
6. **RGB lane** — the sats lanes ship on this design. The colored bridge exists (`latch_tokens` /
   `latch_tokens_se_preimage`, `clients/libs/rust-sdk/src/tokens.rs`, wired into
   `pay_lightning_invoice` and `create_receive`) with the `validate_pending_token` value gate; the LN
   half is exercised by `sdk23`, and a full cross-rail swap remains a documented follow-up.

## 10. Optional hardening (anti-collusion; needs SGX)

A latch-scoped enclave gate in `lockbox/src/enclave.cpp` + `enclave/App/statechain/sign.cpp` that
refuses every co-sign of a latched coin except the single pre-signed `S'` (or one recovery
`renew_state_only`). This promotes anti-hidden-`S*` from coordinator-enforced (census + freeze) to
enclave-enforced, surviving a colluding coordinator. Develop and E2E on the lockbox Docker container,
then mirror into `enclave/App/` and rebuild on Linux + SGX — **both trees, or the shipping lane is
unprotected.** Not a precondition for anything above.

---

## Design record

The decision came from an understand → design(×4) → adversarial-verify(×4 lenses) → synthesize
workflow (`wf_d84cbc10-6c5`), after which the two load-bearing claims were re-verified directly
against the code. The non-exact variant selection came from a second 3-variant, 4-lens workflow
(`wf_ddcd3049-7ac`).

The adaptor-signature design that preceded this one was deleted: it was blocked on an external PTLC
the pinned LN stack does not expose (§2), and on every shippable lane it bought no trustlessness.

Defects found and fixed while landing:

- **D1** — the piece-admission floor reused the old backup-fee floor (442 sat) although a child funds
  its own two tiers plus dust; admitting below `min_child_value` terminalized the parent and then
  failed, stranding it (§5).
- **D3** — the one-call PAY API (`pay_lightning_invoice`) minted its coin via `ensure_exact_coin`,
  which cannot split a laddered coin exactly, so it refused *every* laddered coin, i.e. every coin.
  It now falls back to the non-exact in-ladder lane (`largest_laddered_coin_for_pay` →
  `pay_lightning_invoice_inladder`, `clients/libs/rust-sdk/src/ssp.rs`), the same way `create_receive`
  does on the receive side.
- **D4** — a child routed to a *unilateral* exit was booked `WITHDRAWING`, but a unilateral exit
  produces no cooperative withdrawal tx, so status polling errored forever.

Two designed items were deliberately shipped without: the `key_updated` completion bind (§3, with the
deadlock analysis that any future implementer must respect) and the `lock_expiry` clock
reconciliation (§9 residual 3). Both are documented above at the point where they matter rather than
recorded only here.

Related: [CHILDREN.md](CHILDREN.md) (the first-class children the non-exact lane conveys, and the
pending-transfer lock this design builds on), [PROTOCOL.md](PROTOCOL.md) §5.10 (terminal freeze),
[TRUST-MODEL.md](TRUST-MODEL.md).
