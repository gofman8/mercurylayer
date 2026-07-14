# V2 LN-latch atomicity — client-side rollback recovery (Option A)

Status: **design, pre-implementation.** Chosen path (the server-DB design in `V2-SERVER-LATCH.md` was
reviewed FLAWED — it targets the enclave-sourced counter and breaks the fork guards). This achieves
atomic LN-latched V2 transfers with **no server or enclave change**, keeping the enclave `sig_count` as
the authoritative anti-theft counter. Implement only after adversarial review of THIS doc.

## 1. Premise (what the review established)

`verify_bundle`'s `num_sigs` is the **enclave** `sig_count` (`signature_count/{id}` → lockbox),
incremented on every co-sign, never rolled back. `verify_bundle` accepts iff
`num_sigs == v1_backups + exit_tiers + superseded_states + superseded_extensions` — i.e. the client's
**full-disclosure** count must equal the enclave count (this is exactly how sdk49 balances). Therefore an
orphaned co-sign is reconciled **by disclosing it**, on the client, with no counter surgery.

## 2. The orphan, precisely

A latched V2 transfer pre-signs the receiver-paying state `S'` at CSV `c' = state_csv(k) − δ` (one δ below
the sender's current state `S_k`, so `S'` matures first — Model A). The enclave counts that co-sign
immediately. Two outcomes:

- **Commit** (LN preimage revealed): the receiver adopts the augmented bundle (`current = S'`), the sender
  loses the coin. `num_sigs` balances at the receiver. (= sdk49, now with a batch_id.)
- **Rollback** (latch expires unrevealed): the sender keeps the coin; the enclave `sig_count` is
  permanently `+1` for the never-adopted `S'`; the sender's local bundle still has `current = S_k` and no
  record of `S'`. A later transfer discloses tiers that omit `S'` ⇒ `num_sigs > disclosed` ⇒ the next
  receiver **rejects** (coin bricked). This is the whole problem.

## 3. Recovery (the fix)

On a detected rollback, the sender makes the coin whole again, entirely locally:

1. **Disclose** `S'`: push it into `bundle.superseded_states`.
2. **Renew strictly below** `S'`: advance the coin to a fresh owner-paying current state `S_{k+2}` at CSV
   `state_csv(k) − 2δ` (one δ below `S'`), superseding `S_k` as usual. Now the owner-paying current is the
   soonest-maturing state again, so the CSV-race invariant holds:
   `current.csv (k+2) < S'.csv (k+1) < S_k.csv (k)`, and `current.csv <` every superseded csv.
3. After (1)+(2): `disclosed = v1_backups + {T,X,S_{k+2}} + superseded{S_k, S'}` and this equals the
   enclave `sig_count` (which counted establish + `S'` + the renewal). Balanced. The coin transfers.

`num_sigs` reconciliation, concretely (regtest, one level, δ=6): establish counts 3 tier co-signs (+tx1);
`S'` +1; the recovery renewal +1 (or +2 if it re-signs the extension too — the disclosed set includes
`superseded_extensions`, so either balances as long as EVERY re-sign is disclosed). Invariant: **disclose
every co-sign the enclave made.**

### 3.1 Near-floor edge (must be handled, not hand-waved)

Renewal is bounded: `k+2` may exceed the coin's runway (`state_csv` would fall below the floor / `m_max`).
If `S'` sits so near the floor that no owner state can be placed strictly below it, the coin **cannot be
re-transferred over R′** (no CSV dominance is expressible), though it remains **exitable** (only the
sender holds `S'`, who won't self-harm; the sender's own state exits fine). Policy: on this edge, recovery
returns a typed `LatchRecoveryFloorExhausted` and the SDK surfaces "exit or refresh this coin" — it is NOT
silently bricked, and no fund is at risk. A `refresh` (on-chain re-anchor, existing feature) resets the
ladder to a fresh runway, after which the coin is transferable again. Rollover does NOT help (it resets to
*higher* CSV, later maturity, so `S'` would still win) — recovery is renew-below-only.

## 4. Detecting commit vs rollback

The sender persists a **pending-latch marker** at presign time and resolves it lazily:

- **Marker** `pendinglatch-<statechain_id>` = `{ batch_id, orphan: TesrTier(S'), presign_state_csv }`,
  stored via the existing raw-backup mechanism (like `tesr-<id>`), written *before* conveying.
- **Resolution** (in `transfer_sender::execute` before any new presign, and in a `recover_latches()`
  maintenance pass):
  - If the coin is **no longer owned** by this wallet (transferred away) ⇒ the batch **committed** ⇒ drop
    the marker (nothing to recover).
  - If the coin is **still owned** AND the batch is **no longer live** (expired) ⇒ **rollback** ⇒ run §3
    recovery, then drop the marker.
  - If the coin is still owned AND the batch is **still live** ⇒ in-flight; do nothing (a concurrent
    re-transfer is anyway refused server-side by `statechain_in_latch_batch`).
- **Batch-liveness check**: query the latch expiry (`get_latch_expiry_by_batch_id` is already exposed via
  the paymenthash path; add a read-only `/transfer/latch_status/<batch_id>` if no client route exists —
  a pure read, not a co-sign, so no trust change). Fail-safe: if liveness can't be determined, treat as
  in-flight (do nothing) — never recover a still-live batch (that would fold an orphan that may yet
  commit).

Ownership is the primary signal; expiry is the tie-breaker that lets `recover_latches()` act proactively
without waiting for the next transfer.

## 5. Why it is safe

- **Fails safe before recovery.** Between rollback and recovery the coin's local `current = S_k` (owner-
  paying). A transfer attempt discloses tiers omitting `S'` ⇒ the receiver's `verify_bundle` rejects ⇒ no
  ownership moves, no fund loss (liveness only). A unilateral exit broadcasts `S_k`; `S'` is held only by
  the sender (rollback ⇒ never conveyed), so nobody races it. So the pre-recovery window is never a theft
  window.
- **Sound after recovery.** Full-disclosure count equals the enclave count (§3); CSV-race invariant holds
  (§3 step 2). Identical safety to sdk49.
- **No new server/enclave surface.** The fork guards (single-use/budget) and `num_sigs` are untouched;
  the enclave stays the trust anchor. The only server addition is an optional read-only latch-status route.
- **Idempotent.** Recovery keyed on the marker; a second pass finds no marker (or a still-owned coin whose
  orphan is already superseded) and no-ops.

## 6. Client changes

- **`transfer_sender::execute`**: replace the sdk53 refusal. For a latched V2 transfer: (a) resolve any
  existing marker (§4), (b) presign `S'`, (c) write the marker, (d) convey. Non-latched path unchanged.
- **`tesr.rs`**: `recover_latch(cc, coin, bundle, marker) -> Result<TesrBundle | FloorExhausted>` doing
  §3 (fold + renew-below, with the floor guard); marker read/write helpers.
- **SDK `wallet.rs`**: `recover_latches()` maintenance pass over markers (emits `WalletEvent::LatchRecovered`
  / `LatchRecoveryDeferred`); optionally fold into the existing maintenance loop.
- Remove the sdk53 guard ONLY once recovery + sdk55 are green.

## 7. Test plan (live SE + regtest, real RLN pair as sdk18-22)

- **sdk54 — latched V2 transfer COMMITS.** V2 coin swaps Mercury→LN; on preimage reveal the receiver
  adopts the ladder, `verify_bundle` passes, and the receiver re-transfers cleanly. Marker dropped.
- **sdk55 — latched V2 transfer ROLLS BACK → recover → re-transfer.** V2 coin; latch expires unrevealed;
  the sender still owns the coin; `recover_latches()` folds `S'` + renews below it; a subsequent plain V2
  transfer succeeds and the new receiver's `verify_bundle` balances against the live enclave counter.
  This is the exact scenario sdk53 guards — sdk55 proves the real fix; then delete the sdk53 refusal.
- **sdk55b — near-floor edge.** Drive the coin near its renewal floor, roll back a latch; recovery returns
  FloorExhausted; the coin still exits unilaterally; a `refresh` restores transferability.
- **sdk56 — regression.** V1 LN swap unchanged; sdk49 unchanged; single-use/budget gates unaffected
  (no server change). Re-run sdk40–53.

## 8. Open questions for the adversarial review

- OQ-1: Can the pre-recovery window (still-owned coin with an un-disclosed orphan) be exploited by the
  FAILED RECEIVER if they somehow obtained `S'` (e.g. the batch briefly exposed the conveyed msg before
  rollback)? Establish that a rolled-back batch never lets the receiver decrypt/adopt.
- OQ-2: Multiple concurrent latched transfers of DIFFERENT coins (fine, per-coin markers) vs the same coin
  (must be impossible — `statechain_in_latch_batch`). Confirm.
- OQ-3: Recovery renewal is itself an SE co-sign — can it be blocked (SE refuses, coin single-use/budgeted,
  epoch passed)? A carrier is never latched (sdk53's sibling invariant + sdk52), but confirm a plain V2
  coin under recovery can always renew, or the FloorExhausted/exit fallback covers it.
- OQ-4: Does `presign_state_csv` in the marker suffice to reconstruct the renew-below target after an
  intervening legitimate renewal changed `current` between presign and recovery? (Ordering of markers vs
  renewals.)
- OQ-5: Crash between "write marker" and "convey" (orphan exists at enclave, marker exists, transfer not
  sent) vs between "convey" and "commit". Enumerate and show each resolves to commit-or-rollback correctly.
