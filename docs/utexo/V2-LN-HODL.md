# V2 Lightning swaps via HODL-invoice latch — the adopted design (pivot off adaptor-sig)

**Status (2026-07-23): ADOPTED. Supersedes the adaptor-sig approach (`V2-LN-ADAPTOR.md`), which is
BLOCKED on external PTLC that the pinned LN stack does not expose. This design is FEASIBLE on the
pinned stack today, needs no new cryptographic primitive, and lifts LN swaps onto V2 (TES-R) coins at
**no worse than the existing V1/statechain operator-trust bar**.**

Decision produced by an understand → design(×4) → adversarial-verify(×4 lenses) → synthesize workflow
(`wf_d84cbc10-6c5`), then the two load-bearing claims were re-verified directly against the code.

---

## 0. The reframe that unblocks everything

The adaptor-sig writeup treated the PAY-direction "wall" (a conveyed receiver-paying state `S'` is
simultaneously the ownership proof the SSP reads pre-pay AND a broadcastable exit) as a defect that
adaptor signatures would fix. **That is only true on a PTLC-external lane, which is unshippable.** On
every *shippable* (Mercury-minted / hash-only) lane the adaptor buys **zero** trustlessness
(`V2-LN-ADAPTOR.md` §"THE BLOCKER").

The wall is **not a new V2 problem — it is the existing V1 trust model.** V1 LN-pay (SDK05, shipped)
already hands the SSP a decryptable, broadcastable receiver-paying backup *before* it pays, and trusts
the co-located operator not to broadcast it on a rolled-back swap. **Verified in code:**
`get_msg_addr` → `get_statechain_transfer_messages`
(`server/src/database/transfer_receiver.rs:122-125`) selects `encrypted_transfer_msg` with **no
`locked`/`locked2` filter**, and the SSP value-gate `peek_pending_transfers` decrypts the branch
pre-pay (`clients/libs/rust/src/transfer_receiver.rs`, sdk37). So serving `S'` pre-pay is the **status
quo**, not a regression this design introduces.

Therefore the correct bar is **not** "break the wall." It is: *retire the sdk53 V2-refusal guard
(`clients/libs/rust/src/transfer_sender.rs:253-266`) and delete the V1 LN lane, at no worse trust than
V1, on the pinned stack, with no missing LN primitive.* PTLC is the only absent primitive, and this
design needs none.

## 1. Feasibility — verified against `rgb-lightning-node` `pr-90` (LDK 0.2.2 fork)

| Primitive | Status | Evidence |
|---|---|---|
| **HODL invoice** (hold → settle/cancel) | **AVAILABLE** | `InvoiceType::Hodl` `src/ldk.rs:209`; `/lninvoice` with external `payment_hash`; settle `/claimhodlinvoice` (`routes.rs:2408`); refund `/cancelhodlinvoice` (`routes.rs:2334`); harness `claim_hodl(hash, preimage)` `rln.rs:230` |
| BOLT11 + preimage | AVAILABLE | `/lninvoice` `/sendpayment`, sdk05/06/21 green |
| Keysend (spontaneous) | AVAILABLE | `/keysend` `routes.rs:3363` |
| Custom TLV records | ABSENT (not exposed) | `spontaneous_empty()` `routes.rs:3441` — needs new API field |
| **PTLC / payment points** | **ABSENT** | no matches in `src/`; `payment_point` = BOLT3 basepoint only — **the adaptor blocker** |

Any design coupling the coin to a **SHA256 preimage** (HODL, plain preimage swap, keysend) is fully
supported. This design uses only HODL + BOLT11-preimage. **Not blocked.**

## 2. The design — HODL-invoice latch, split by direction

### RECEIVE (LN → statecoin): sound, ships first, no enclave change
The SSP fronts **its own** statecoin under a HODL invoice.
1. SSP mints a statecoin transfer to the user, latched under `batch_id`; the SE mints the preimage
   (`post_paymenthash`, `server/src/endpoints/lightning_latch.rs:61`) and returns `hash`.
2. User pays a HODL invoice with that `hash`; the incoming HTLC is **HELD** at the SSP's node
   (`notify_claimable_hodl_invoice`), not settled.
3. The user completes the key-handover (`/transfer/receiver`) → the coin is now the user's.
4. **Only then** does the SE release the preimage to the SSP, which settles the HTLC and gets paid.
   If the user never completes, the SSP `/cancelhodlinvoice` → the payer is refunded; the coin was
   never released.

The SSP owns the coin throughout its risk window and the HTLC is held before release — **the user
cannot be robbed and the SSP cannot be robbed.** (All four adversarial lenses agree on RECEIVE.)

**The core security fix (closes the real flaw):** today `get_preimage`
(`server/src/database/lightning_latch.rs:60-67`) releases on `locked=false AND expires_at>now()` —
it does **not** check the transfer actually completed. Tighten it to also require
`key_updated = true` (join `lightning_latch.batch_id → statechain_transfer.key_updated`), binding the
SSP's ability to get paid to the coin having genuinely been delivered to the user. This is exactly the
"release `t` only on real completion, not on a bare lock-bit + expiry" property.

### PAY (statecoin → LN): sound at the V1 bar, ships second, no enclave change
The user pays an external merchant BOLT11 (hash `H`) with a statecoin.
1. User `create_external_hash_latch(H)` (`post_paymenthash_external`, `lightning_latch.rs:181`); SE
   stores `H`, never learns the preimage.
2. User conveys the coin to the SSP (`S'` pays the SSP), latched under `batch_id`, `locked2` pending.
3. **SSP pre-pay census (the load-bearing fix):** before `send_payment`, the SSP decrypts the bundle
   (`peek_pending_transfers`) and runs the full `verify_bundle` / `num_sigs == disclosed-ladder`
   census, reading the **enclave-authoritative** `sig_count` (`GET /signature_count`), not the
   coordinator DB. A hidden lower-CSV `S*` inflates `sig_count` beyond the disclosed ladder →
   **mismatch → refuse to pay**, before the irreversible LN leg. The `has_open_transfer` freeze
   (landed, `server/src/database/transfer_sender.rs:60-70`) blocks any *new* co-sign during the
   window.
4. SSP pays the merchant; settlement reveals the preimage; `unlock_by_preimage(batch_id, preimage)`
   (`lightning_latch.rs:215`) clears `locked2` and the transfer completes (SSP owns the coin).

**Do NOT delivery-gate the SSP-mediated PAY transfer** (leave `get_msg_addr` servable so the census
can run) — this is the current V1 behavior. The residual (SSP holds a broadcastable `S'`, trusted not
to broadcast on rollback) is borne by the **operator**, not the untrusted user, and equals the V1 bar.
The `locked2` delivery-gate is retained **only** for the direct P2P latched lane, where there is no
trusted operator to lean on.

### Terminalization → demoted to OPTIONAL defense-in-depth
The adaptor review's FATAL user-double-spend (`V2-LN-ADAPTOR.md` finding 1) is closed here by the
census (at claim for RECEIVE, at pre-pay for PAY) **plus** the `has_open_transfer` freeze. Making
terminalization *mandatory* only reintroduces a reclaim-bricking griefing vector (a terminalized coin
cannot be reclaimed off-chain on rollback → forced on-chain exit). Enclave-enforced *latch-scoped*
terminalization is future hardening (Phase 3) that buys anti-collusion, **not** a precondition for
retiring sdk53.

## 3. Trust assumption (drop-in for TRUST-MODEL.md)

> **RECEIVE (LN → statecoin)** needs no operator trust for safety: the SSP fronts its own coin, the
> inbound HTLC is HELD before the coin is released, and the SE-minted preimage is released only after
> the user's key-handover completes (`get_preimage` gated on `key_updated`).
>
> **PAY (statecoin → LN)** rests on the existing statechain operator-trust model: after pre-pay
> validation the SSP — co-located with the coordinator and blind SE as one operator trust domain —
> holds a broadcastable receiver-paying state `S'`, and the design trusts the operator not to broadcast
> it on a rolled-back swap, exactly as the shipped V1 latch already does. The **untrusted counterparty
> (the user) cannot be robbed** (its coin is frozen by the pending-lock, reclaimable after
> `batch_timeout`, and `locked2` clears only on the genuine merchant preimage) and **cannot rob the
> SSP**, because the pre-pay `num_sigs` census — read from the enclave-authoritative `sig_count`, not
> the coordinator DB — rejects any coin carrying a hidden lower-CSV state before the invoice is paid.
> This is **no worse than the existing V1/statechain trust model.** Adaptor signatures would
> additionally have removed the SSP-broadcast-on-rollback trust (soundness against a *Byzantine*
> operator) — but only on a PTLC-external lane that is unshippable on the pinned LDK 0.2.2 stack.

## 4. Implementation plan (Phases 1–2 are lockbox-Docker + RLN testable; no SGX)

### Phase 1 — RECEIVE on V2
1. `clients/libs/rust/src/transfer_sender.rs:253-266` — lift the sdk53 V2 refusal for the RECEIVE
   latched path.
2. `server/src/database/lightning_latch.rs:60-67` — tighten `get_preimage`:
   `locked=false AND key_updated=true AND expires_at>now()` (join to `statechain_transfer`).
3. `clients/libs/rust-sdk/src/ssp.rs` (`settle_receive`) — add the symmetric user-side value gate
   (received coin value ≥ HODL invoice amount).
4. **Clock fix (shared with Phase 2):** add `statechain_transfer.lock_expiry TIMESTAMPTZ`;
   `has_open_transfer` reads it instead of the hardcoded `INTERVAL '1 hour'`; both sign gates
   (`server/src/endpoints/sign.rs`) read it; set at `get_new_x1`; enforce
   `lock_expiry ≥ HTLC CLTV + grace`.
5. **`sdk60_hodl_v2_receive`** (`UTEXO_PROTOCOL_DEFAULT=2`): happy path (HODL held → user key-update →
   preimage released → `claim_hodl`); rollback (payer never settles → `cancel_hodl` refunds → coin
   reclaimable off-chain, proving no mandatory-terminalization brick).

### Phase 2 — PAY on V2
1. `clients/libs/rust-sdk/src/ssp.rs` (`execute_pay`) — **before `send_payment`**, fetch enclave
   `sig_count`, decrypt via `peek_pending_transfers`, run the `num_sigs == disclosed-ladder` census;
   refuse to pay on mismatch.
2. Do **not** gate `get_msg_addr` for this lane; keep `locked2` delivery-gate scoped to the direct P2P
   lane only.
3. `clients/libs/rust/src/transfer_sender.rs:253-266` — lift sdk53 for the PAY latched path; up-front
   guards: reserve ≥2δ runway; refuse latched presign on `single_use`/`epoch`/budget coins.
4. Reuse unchanged: `create_external_hash_latch`, `unlock_by_preimage`, the pending-lock freeze.
5. **`sdk61_hodl_v2_pay`:** happy path; **hidden-`S*`** — user co-signs a lower-CSV state omitted from
   the disclosed ladder → assert the pre-pay census REJECTS *before* `send_payment`; **rollback** —
   merchant never settles → `reclaim_lightning_payment` succeeds after `batch_timeout`.
6. **`sdk62_hodl_clock`:** `get_new_x1` rejects `lock_expiry < HTLC CLTV + grace`.

### Phase 3 — OPTIONAL hardening (anti-collusion; needs SGX)
`lockbox/src/enclave.cpp` + `enclave/App/statechain/sign.cpp` — a latch-scoped enclave gate that
refuses every co-sign of a latched coin except the single presigned `S'` (or one recovery
`renew_state_only`), promoting anti-hidden-`S*` from coordinator-enforced (census+freeze) to
enclave-enforced (survives a colluding coordinator). Develop + E2E on the lockbox Docker container,
then mirror into `enclave/App/` and rebuild on Linux+SGX (see the sgx-lane gap note — BOTH trees or
the shipping lane is unprotected). **Not required to retire sdk53 at the V1 bar.**

## 5. Residual risks (document; none are Phase 1–2 blockers)
1. **PAY operator-trust residual** — SSP holds a broadcastable `S'` post-validation; equals the V1 bar; the user is fully protected.
2. **Coordinator collusion on the freeze** — the census reads the tamper-resistant enclave `sig_count`; the Postgres freeze itself is coordinator-enforced. Full prevention is Phase 3.
3. **Clock reconciliation is correctness-critical** — `lock_expiry ≥ HTLC CLTV + grace` enforced at `get_new_x1`, tested by sdk62; a lapsed freeze mid-HTLC is a free option.
4. **Capital lockup / griefing** — a frozen coin is tied up for `lock_expiry` (now up to the HTLC CLTV); bounded DoS; mitigate with rate-limits/bonds (see audit [15] auth-nonce griefing).
5. **SE/coordinator liveness at settlement** — fail-closed gates stall (never mis-resolve); RECEIVE forbids `cancel_hodl` after release; PAY retries handover idempotently.
6. **RGB lane** — sats lanes ship on this plan; the colored `latch_tokens_se_preimage` bridge is a documented follow-up (`validate_pending_token` value gate exists).

**Bottom line:** HODL-invoice latch, census-in-pre-pay, terminalization optional — retires the sdk53
guard and deletes V1 for LN swaps, shippable on the pinned stack today, no PTLC, no enclave change for
the core, at provably no worse than the V1 trust model. Relates to `V2-LN-ADAPTOR.md` (superseded),
`V2-LATCH-FIX.md`, `V2-CHILD-FIRSTCLASS.md` (the pending-lock this builds on).
