# Lightning — the HODL-invoice latch

**Normative.** This is how Lightning works: both directions, exact and non-exact amounts, running on
the TES-R ladder over the pinned `rgb-lightning-node` `pr-90` (LDK 0.2.2) stack. No new cryptographic
primitive, no enclave change for the core.

> **Two designed items are NOT built** and are flagged in place rather than quietly dropped: the
> `key_updated` tightening of `get_preimage` (§3) and the `lock_expiry` clock reconciliation (§9
> residual 3). Residual 3 is **open and uncovered** — it is the top open item of this design.

## 1. What ships

| Direction | Amount | Entry point | Evidence |
|---|---|---|---|
| PAY (coin → LN) | exact | `pay_lightning_invoice` | `sdk63` |
| PAY | non-exact | `pay_lightning_invoice_inladder` | `sdk65` |
| RECEIVE (LN → coin) | exact | `create_receive` | `sdk64` |
| RECEIVE | non-exact | `create_receive` → in-ladder split | `sdk67` |
| PAY rollback | non-exact | booking rolled back, whole parent recovered | `sdk66` |
| PAY rollback | exact | `reclaim_lightning_payment` | `sdk68` |

`sdk53` pins that a **laddered** coin can be latched. `tb04` covers the raw latch primitive;
`sdk24`/`sdk25` cover the cancel and delayed-claim paths.

**The EXACT sats lane no longer mints its input, and that is now load-bearing.** `ensure_exact_coin`
used to fall back to the plain off-chain split; that split is DELETED with the un-laddered shape,
because it spent the coin's funding output `F` directly and a prior owner's retained, un-timelocked
trigger spends the same `F` ([B1]). So `ensure_exact_coin` returns an exact CONFIRMED coin if the
wallet happens to hold one and otherwise **refuses**, and the one-call PAY API
(`pay_lightning_invoice`) falls back to the non-exact in-ladder lane
(`largest_laddered_coin_for_pay` → `pay_lightning_invoice_inladder`,
`clients/libs/rust-sdk/src/ssp.rs`), the same way `create_receive` does on the receive side. That is
REQ-42's fallback doing exactly what it was specified for: the in-ladder lane carves its piece as a
DESCENDANT of the trigger rather than a rival for `F`, which is what makes it safe where the deleted
route was not.

**Coin shapes.** Both sats lanes run on **laddered** coins. Under CTES-R the colored (RGB) lane rides
a laddered carrier too — a COLOURED ladder, every tier carrying a valid RGB state transition, so a
tier spend moves the allocation instead of destroying it (terminal freeze,
[PROTOCOL.md](PROTOCOL.md) §5.10; `sdk52` pins that a carrier is never given a *plain* ladder, `sdk74`
/ `sdk75` the coloured one). Whether that applies is a property of the NETWORK:
`SdkConfig::colored_ladder` reads the compiled-in enclave attestation pin, so it is on for regtest and
off for mainnet/testnet/signet until an enclave is provisioned there (`SPEC.md` §0.4 V-6). Where it is
off, a carrier keeps the flat signed-once shape and this document's flat-lane statements are the ones
that apply. See [PROTOCOL.md](PROTOCOL.md), "One protocol, ONE coin SHAPE".

> **The colored PAY lane has a real hole, and the deletions above narrowed rather than caused it.**
> An RGB Lightning pay reaches the latch through `latch_tokens` → `colored_transfer`
> (`clients/libs/rust-sdk/src/tokens.rs`), and exactly one fork of that function accepts a latch: a
> **single coloured ROOT carrier** holding the whole amount, which routes to
> `colored_in_ladder_transfer` (wired under [P3], returning the batch id and any SE hash to the
> caller). Every other fork refuses a latched colored send by name — a coloured **CHILD** carrier
> ("not yet wired to the CTES-R child lane") and a **multi-carrier** payment ("not yet wired to the
> CTES-R in-ladder lane") — and `pay_lightning_invoice_inladder` refuses outright when the quote
> carries an `asset_id` ("RGB non-exact in-ladder Lightning pay is not supported yet"). A coloured
> child is what the change leg of every coloured payment and every received coloured piece IS, so the
> refusals are not corner cases: a wallet that has made one coloured payment can hold its whole
> balance in a shape no LN lane will latch. This is the LN lane's to close, not the split lane's.

## 2. Feasibility — verified against `rgb-lightning-node` `pr-90` (LDK 0.2.2 fork)

| Primitive | Status | Evidence |
|---|---|---|
| **HODL invoice** (hold → settle/cancel) | **AVAILABLE** | `InvoiceType::Hodl` (`src/ldk.rs`); `/lninvoice` with external `payment_hash`; settle `/claimhodlinvoice`, refund `/cancelhodlinvoice` (`src/routes.rs`); harness `claim_hodl(hash, preimage)` (`rln.rs`) |
| BOLT11 + preimage | AVAILABLE | `/lninvoice`, `/sendpayment`; `sdk21`/`sdk63`/`sdk64` green |
| Keysend (spontaneous) | AVAILABLE | `/keysend` (`src/routes.rs`) |
| Custom TLV records | ABSENT (not exposed) | `spontaneous_empty()` (`src/routes.rs`) — would need a new API field |
| **PTLC / payment points** | **ABSENT** | no matches in `src/`; `payment_point` is a BOLT3 basepoint only |

Any design coupling the coin to a **SHA256 preimage** (HODL, plain preimage swap, keysend) is fully
supported. This design uses only HODL + BOLT11-preimage.

**Why not adaptor signatures.** An adaptor-signature design would additionally remove the
SSP-broadcast-on-rollback trust in the PAY direction (soundness against a *Byzantine* operator). It
is unshippable: it requires an external PTLC, the one primitive the pinned stack does not expose.
And on every *shippable* (Mercury-minted / hash-only) lane the adaptor buys **zero** trustlessness.
The PAY-direction wall — a conveyed receiver-paying state `S'` is simultaneously the ownership proof
the SSP reads pre-pay *and* a broadcastable exit — is a property of the statechain trust model, not a
defect this design introduces; see §4.

## 3. RECEIVE (LN → coin)

The SSP fronts **its own** coin under a HODL invoice.

1. SSP mints a coin transfer to the user, latched under `batch_id`; the SE mints the preimage
   (`post_paymenthash`, `server/src/endpoints/lightning_latch.rs`) and returns `hash`.
2. The user pays a HODL invoice carrying that `hash`; the incoming HTLC is **HELD** at the SSP's node
   (`notify_claimable_hodl_invoice`), not settled.
3. The user completes the key handover (`/transfer/receiver`) — the coin is now the user's.
4. **Only then** does the SE release the preimage to the SSP, which settles the HTLC and gets paid.
   If the user never completes, the SSP calls `/cancelhodlinvoice`, the payer is refunded, and the
   coin was never released.

The SSP owns the coin throughout its risk window and the HTLC is held before release: **the user
cannot be robbed and the SSP cannot be robbed.** RECEIVE needs no operator trust for safety.

### The completion bind is designed, not built

`get_preimage` (`server/src/database/lightning_latch.rs`) releases on
`locked = false AND expires_at > now()`. The design proposes additionally requiring
`key_updated = true`, binding the SSP's payout to the coin actually being delivered.

**That must never be applied globally — it deadlocks the `sender-settles-first` lane.** Two shipped
flows retrieve the preimage in opposite orders relative to the receiver's key update:

- **`settle_receive`** (`clients/libs/rust-sdk/src/ssp.rs`): the SSP calls `confirm_pending_invoice`
  (clearing `locked2`), the receiver claims in the background (clearing `locked` and, with the batch
  now fully unlocked, setting `key_updated = true`), then the SSP **retries** `retrieve_pre_image`.
  Here `key_updated = true` precedes retrieval, so the bind would be safe.
- **`settle_lightning_swap`** (the direct P2P lane, `clients/libs/rust-sdk/src/lightning.rs` —
  `confirm_pending_invoice` then `retrieve_pre_image` in one call): the receiver's *first* claim
  clears `locked` but leaves `key_updated = false` (the batch is still `locked2`); the **sender**
  then settles, which clears `locked2` *and* retrieves the preimage — before the receiver completes.
  A global `key_updated` gate deadlocks: settle needs the preimage → the preimage needs
  `key_updated` → `key_updated` needs `locked2 = false` → which settle itself provides. Circular.

**Nothing of this is built.** `get_preimage` carries no `key_updated` join, scoped or global.
RECEIVE therefore rests on the **coordinated-clock atomicity** in place today (`settle_receive`,
`clients/libs/rust-sdk/src/ssp.rs`): the receiver's claim gate and the SSP's `get_preimage` are bound
to the **same** latch expiry, set shorter than the payer's HODL HTLC. Either the receiver completes
in-window and the SSP can settle, or both fail and the payer is refunded. That is pinned live by
`sdk25` (delayed claim: the latch expires ⟹ both the claim and the preimage retrieval fail) and
`sdk24` (cancel path). The safety claim stands on its own evidence; only the optional strengthening
is absent.

> **The deadlock caveat is itself untested.** `settle_lightning_swap` retrieves the preimage before
> the receiver completes, and no live E2E covers that ordering — `tb04` exercises the same latch
> primitives but in the *receiver-completes-first* order. Anyone adding a `key_updated` gate must
> scope it to the SSP-fronts-the-coin flow, and has **no regression test** to catch the mistake.

## 4. PAY (coin → LN)

The user pays an external merchant BOLT11 (hash `H`) with a coin.

1. User calls `create_external_hash_latch(H)` (`post_paymenthash_external`,
   `server/src/endpoints/lightning_latch.rs`); the SE stores `H` and never learns the preimage.
2. User conveys the coin to the SSP (`S'` pays the SSP), latched under `batch_id`, `locked2` pending.
3. **SSP pre-pay census — the load-bearing check.** Before `send_payment`, the SSP decrypts the
   bundle (`peek_pending_transfers`) and runs the full `verify_bundle` /
   `num_sigs == disclosed-ladder` census, reading the **enclave-authoritative** `sig_count`
   (`GET /signature_count`), *not* the coordinator DB. A hidden lower-CSV `S*` inflates `sig_count`
   beyond the disclosed ladder → mismatch → **refuse to pay**, before the irreversible LN leg. The
   pending-transfer lock (`has_open_transfer`, `server/src/database/transfer_sender.rs`) blocks any
   *new* co-sign during the window.
4. SSP pays the merchant; settlement reveals the preimage; `unlock_by_preimage(batch_id, preimage)`
   (`server/src/endpoints/lightning_latch.rs`) clears `locked2` and the transfer completes with the
   SSP owning the coin.

**Do not delivery-gate the SSP-mediated PAY transfer** — `get_msg_addr` must stay servable so the
census can run. The `locked2` delivery gate is retained **only** for the direct P2P latched lane,
where there is no trusted operator to lean on.

### Serving `S'` pre-pay is the statechain's existing bar, not a new exposure

The residual — the SSP holds a broadcastable receiver-paying `S'` and is trusted not to broadcast it
on a rolled-back swap — is the trust model the statechain already has. **Verified in code:**
`get_msg_addr` → `get_statechain_transfer_messages`
(`server/src/database/transfer_receiver.rs`) selects `encrypted_transfer_msg` with **no
`locked`/`locked2` filter**, and the SSP value gate `peek_pending_transfers` decrypts the branch
pre-pay (`clients/libs/rust/src/transfer_receiver.rs`, `sdk37`). The residual is borne by the
**operator**, not by the untrusted user.

## 5. Non-exact amounts — latched child conveyance

Real invoices are non-exact, and splitting a laddered coin yields an in-ladder-split **child**
conveyed via `convey_child_bundle` (a mailbox/adopt path censused by `verify_conveyed_child`), which
the exact lane's `verify_bundle` census does not handle. Two alternatives are disqualified —
`promote-then-standard` needs a forbidden child reopen or an on-chain re-anchor, and `ssp-change`
enlarges the operator-trust bar to the whole over-collateralized coin.

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

### The value binding

`verify_tier_cosigned` binds the co-sign to the INPUT amount, not the output split, and the blind SE
co-signs any distribution. So the value a child census returns must never be the *sender-declared*
`cb.child_state.out_value`: a payer could otherwise craft `state_child` paying the receiver a few sats
while declaring a large `out_value` (remainder to a second output back to itself), and any value gate
trusting the declared field would pay a full invoice for a near-worthless piece. `verify_child_bundle`
therefore binds `st_out0.value == cb.child_state.out_value`, which makes the value returned by
`verify_conveyed_child` trustworthy. This holds for the whole child census, not only for non-exact LN.
Adversarial regression: `sdk65` case A.

### Admission floor

A child funds its **own** two tiers plus dust, so the piece must clear
`min_child_value = 2·(committed_fee(rate) + 240) + 330` — **1 560 sat** at the shipped
`committed_fee_rate = 3.0` — and not the 442-sat backup-fee floor that bounds a plain transfer. The
floor is a function of the rate: quoting one of these numbers without its rate is quoting a rate
([SPEC.md](SPEC.md), [CHILDREN.md](CHILDREN.md)). Admitting below `min_child_value` terminalizes the
parent and *then* fails, stranding it. Sizing goes through `tier_out_total` /
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

This is a local exception, not mandatory terminalization: the parent and the change slice are
untouched, and rollback still recovers the whole parent (`sdk66`) because the split itself is
un-broadcast.

## 7. Failure and rollback

On a coin carrying **no ladder** — the flat carrier residual — `reclaim_lightning_payment` (a
self-transfer) returns a fully re-usable coin. On a **laddered** coin the naive self-transfer **bricks**: the failed latch already co-signed
the orphan `S'` (`sig_count` +1), so the reclaim's own presign leaves the disclosed ladder one tier
short of the enclave `sig_count` and `verify_bundle` rejects. Handled by direction:

- **Non-exact PAY** (`sdk66`): the split is un-broadcast, so `pay_lightning_invoice_inladder` rolls
  back the optimistic booking and the user recovers the **whole parent**.
- **Exact whole-coin PAY** (`sdk68`): `reclaim_lightning_payment` detects a laddered coin and restores
  it locally as exitable instead of attempting the bricking self-transfer.

Both leave the coin recoverable via `unilateral_exit`, on-chain, at the same operator-trust bar the
success path uses. A child routed to a **unilateral** exit is booked `WITHDRAWING` with neither a
withdrawal tx nor a withdrawal address — its funding `SP.out[j]` is un-broadcast, so there is no
confirmed outpoint for a cooperative withdraw to spend and progress is tracked by the pre-signed exit
chain rather than by watching one txid. Status polling accepts that shape
(`clients/libs/rust/src/coin_status.rs`).

**Residual:** re-transfer stays orphan-bricked until a `refresh()` re-anchor. Restoring full
*off-chain* reuse needs a **scoped coordinator-side `sig_count` reconcile**: on an authenticated
reclaim of a rolled-back latch, the SE decrements the coin's `sig_count` by exactly the orphan `S'`
it co-signed for that batch — bounded, single-use, batch-scoped — so the reclaim's self-transfer
census balances again. That is a server workstream needing its own security review (a
coordinator-authoritative count decrement must be tightly scoped so it can never hide a real state).
`refresh()` works today but is an on-chain re-anchor, i.e. the same cost class as an exit. Optional
enclave terminalization (§10) does **not** help here — terminalizing the latched coin makes rollback
worse, since the coin's only permitted spend becomes the SSP-paying `S'`.

**Coverage note:** the flat self-transfer branch of `reclaim_lightning_payment` has **no live
E2E**. Since every plain deposit is laddered, the branch exists only for the flat carrier residual —
narrower since the CTES-R flip, and now reachable only on a network with no pinned enclave identity or
on a carrier the coloured builder cannot take. The laddered branch is `sdk68`; the non-exact rollback is `sdk66`.

## 8. Trust assumption

> **RECEIVE (LN → coin)** needs no operator trust for safety: the SSP fronts its own coin, the
> inbound HTLC is HELD before the coin is released, and the SE-minted preimage is released only
> inside a latch window that expires BEFORE the payer's HODL HTLC — the receiver's claim gate and the
> SSP's `get_preimage` are bound to that same latch clock, so either the receiver completes in-window
> (and the SSP can settle) or both fail and the payer is refunded, the coin never having left the SSP
> (`sdk25`, `sdk24`). *(The stronger `get_preimage`-gated-on-`key_updated` bind is designed but not
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
   `lock_expiry ≥ HTLC CLTV + grace`) is **not built**, and no test pins it. `has_open_transfer`
   (`server/src/database/transfer_sender.rs`) uses the hardcoded `INTERVAL '1 hour'`, narrowed to
   `batch_time + batch_timeout` for latched transfers. The risk is bounded in practice because the
   HTLC CLTVs in use sit inside that window, but the invariant is **asserted nowhere**. Top open item
   of this design.
4. **Capital lockup / griefing** — a frozen coin is tied up for the pending-transfer lock window (the
   batch timeout for latched transfers, otherwise 1 hour); bounded DoS, mitigable with rate limits or
   bonds.
5. **SE / coordinator liveness at settlement** — fail-closed gates stall rather than mis-resolve;
   RECEIVE forbids `cancel_hodl` after release; PAY retries the handover idempotently.
6. **RGB lane** — the sats lanes ship on this design. The colored bridge exists (`latch_tokens` /
   `latch_tokens_se_preimage`, `clients/libs/rust-sdk/src/tokens.rs`, wired into
   `pay_lightning_invoice` and `create_receive`) with the `validate_pending_token` value gate; the LN
   half is exercised by `sdk23`, and a full cross-rail swap is **not built** — it remains a follow-up.
   **Since CTES-R the bridge's reach is narrower than "the colored lane", and §1 states the shape:**
   `colored_transfer` accepts a latch only from a single coloured ROOT carrier holding the whole
   amount. A coloured CHILD carrier and a multi-carrier payment each refuse a latched send by name,
   and `pay_lightning_invoice_inladder` refuses any quote carrying an `asset_id`. Closing this is LN
   work — wiring the latch through the coloured child and multi-carrier legs — not split-lane work.

## 10. Optional hardening (anti-collusion; needs SGX)

A latch-scoped enclave gate in `lockbox/src/enclave.cpp` + `enclave/App/statechain/sign.cpp` that
refuses every co-sign of a latched coin except the single pre-signed `S'` (or one recovery
`renew_state_only`). This promotes anti-hidden-`S*` from coordinator-enforced (census + freeze) to
enclave-enforced, surviving a colluding coordinator. Develop and E2E on the lockbox Docker container,
then mirror into `enclave/App/` and rebuild on Linux + SGX — **both trees, or the shipping lane is
unprotected.** This is **design, not built**, and is not a precondition for anything above.

---

Related: [CHILDREN.md](CHILDREN.md) (the first-class children the non-exact lane conveys, and the
pending-transfer lock this design builds on), [PROTOCOL.md](PROTOCOL.md) §5.10 (terminal freeze),
[TRUST-MODEL.md](TRUST-MODEL.md).
