# V2 LN-latch atomicity — complete design (delivery-gate + reconcile-recovery)

Status: **design, pause-before-coding** (per decision). This is the full, standalone spec for making
LN-latched V2 transfers atomic. It supersedes the sketches in `V2-SERVER-LATCH.md` (rejected: wrong
counter) and `V2-LATCH-RECOVERY.md` §1–8 (rejected alone: theft window). It pre-addresses every FATAL and
SERIOUS from both adversarial reviews (mapping in §10). **No code until this doc is reviewed SOUND.**

## 0. The one invariant everything serves

`verify_bundle` accepts a coin iff **(count)** `se_num_sigs == disclosed(bundle)` where
`disclosed = v1_backups + exit_tiers.len() + superseded_states.len() + superseded_extensions.len()`, AND
**(maturity)** the current state's CSV is strictly below every superseded *state* CSV, and the current
extension's CSV is strictly below every superseded *extension* CSV. `se_num_sigs` is the **enclave**
`sig_count` (`transfer_receiver.rs:46-67` → lockbox), incremented on every co-sign, never rolled back.
So the client's job is only ever: **disclose every co-sign the enclave made, and keep the owner-paying
current the soonest-maturing.** Two problems break this for a latched transfer — the receiver getting `S'`
early (theft), and orphan co-signs the client fails to disclose (brick). This design fixes both.

## 1. Two mechanisms

- **(A) Server delivery-gate** — a rolled-back receiver must never obtain `S'`. Withhold the V2 bundle
  from the receiver until the batch commits (unlock), using the gate the key-rotation already uses.
- **(B) Client reconcile-recovery** — with (A) in place, an orphaned presign on rollback is held only by
  the sender, and is repaired locally by reconciling the disclosed count up to the live enclave count and
  renewing the owner state below every orphan.

Neither changes the enclave. (A) is the same trust surface the latch already has (the operator already
gates receiver progress on the preimage); (B) is pure client.

## 2. (A) Server delivery-gate

### 2.1 Withhold the ladder until unlock
`get_msg_addr` (`transfer_receiver.rs:92-116`) currently returns the transfer msg — including the
ECIES-encrypted `tesr_ladder` — with no latch gate. Change: if the coin is a member of a **live,
not-yet-unlocked** latch batch, return the msg with `tesr_ladder` **omitted** (V1 fields only, which are
harmless — they pay the *sender*, not the receiver). Serve the full msg (with ladder) only once the batch
is unlocked (`locked2` flipped by `update_unlock_transfer`). Commit ⇒ receiver fetches ladder, adopts `S'`,
finalizes. Rollback ⇒ receiver never fetched the ladder ⇒ never holds `S'`.

Membership/unlock test: reuse the receiver-side batch state the key-rotation endpoint already consults
(`validate_batch` / `locked2`), not a bespoke query. A V2 transfer with **no** latch (`batch_id=None`)
is served normally (unchanged — this is the sdk49 path).

### 2.2 Authoritative rollback signal (not client-guessed expiry)
Recovery (B) must only fire on a state that can **never** commit. Two server facts break a naive
client-expiry check:
- `unlock_by_preimage` (`lightning_latch.rs:215-261`) has **no expiry guard** (unlike `transfer_preimage`
  at `:141-153`). So a preimage presented after the client thinks the latch expired still commits ⇒ a
  client that renewed-below meanwhile would have clawed back a committed coin (fund-loss). **Fix:** add the
  same fail-closed expiry guard to `unlock_by_preimage`, so "expired" is final.
- Latch rows are **reaped** (`insert_paymenthash` runs `DELETE ... WHERE expires_at < now()`,
  `lightning_latch.rs:13`), so `get_latch_expiry_by_batch_id` returns `None` for BOTH "expired/gone" and
  "transient DB error" — indistinguishable, and the *normal* rolled-back end-state is a reaped row.
  **Fix:** a **resolved/consumed flag** on the latch row; reap only rows that are resolved
  (committed or explicitly expired), and expose a tri-state `/transfer/latch_status/<batch_id>` →
  `LIVE | SETTLED_FINAL | UNKNOWN`. Recovery acts only on `SETTLED_FINAL`+not-committed; `UNKNOWN` ⇒ wait.
  The client also records `expires_at` in its marker at presign time as a secondary witness.

### 2.3 Concurrency: block any transfer of a still-latched coin
`statechain_in_latch_batch` only guards the anti-poisoning path (`transfer_sender.rs:71-78`); a **plain**
re-transfer (`batch_id=None`) of a latched coin is gated only by the short `batch_timeout` (120s), not by
latch liveness. **Fix:** gate `transfer_sender` to refuse **any** transfer of a coin that currently has a
live `lightning_latch` row (per-statechain latch-live query), regardless of the new `batch_id`; and the
client refuses a second transfer of a coin with an unresolved local marker. This makes §4's "do nothing
while live" sound.

## 3. (B) Client recovery — the reconcile primitive

The core operation is **`reconcile(coin)`**, keyed on the authoritative enclave count, not on marker
presence. It is idempotent and crash-re-runnable:

```
reconcile(coin):
  bundle  = load(coin);  se = signature_count(coin)         # live enclave truth
  if disclosed(bundle) == se: drop_marker(coin); return OK   # already balanced
  # deficit = se - disclosed(bundle) orphan co-signs exist. Recover their CSVs:
  for each orphan CSV known from the marker(s) (a presign records {txid, csv}):
      if that tier is not already in superseded_states: push it; persist(bundle)   # write-ahead disclosure
  # If a marker is MISSING (e.g. mnemonic restore) but deficit remains, orphans exist at UNKNOWN CSV:
  if disclosed(bundle) < se: return NEEDS_EXIT_OR_REFRESH     # cannot safely renew-below unknown CSVs
  # Now every orphan is disclosed. Restore maturity dominance:
  renew_below(bundle, target_state_csv = min(all superseded state csv) - δ,
                      target_ext_csv   = min(all superseded ext   csv) - δ)   # raw renew(), persist per co-sign
  assert verify_bundle(bundle, signature_count(coin))  # count AND maturity, both checks
  drop_marker(coin); return OK
```

### 3.1 Write-ahead discipline (kills every crash-brick finding)
Two rules make the enclave count and the disclosed set never diverge unrecoverably:
1. **Write-ahead marker**: before *any* latched presign co-sign, persist a marker
   `{batch_id, orphan_txid, orphan_csv, orphan_out_value, pre_presign_sig_count, expires_at}`. The tier
   txid/csv are deterministic pre-witness (`lib/src/tesr.rs`), so no signature is needed. If the co-sign
   then fails (SE 409/network/crash), `reconcile` reads the live count: if it did **not** advance past
   `pre_presign_sig_count`, the co-sign never landed ⇒ the marker is a phantom ⇒ discard it without
   folding (fixes the phantom-fold overcount).
2. **Write-ahead disclosure**: every SE co-sign (the presign, and each of recovery's renew co-signs) is
   immediately followed by persisting its disclosure into the bundle *before* the next co-sign or convey.
   So `disclosed` is at most one co-sign behind `se`, and `reconcile` closes that residue on re-entry. A
   mid-recovery crash therefore never leaves an un-disclosed co-sign that a later run double-counts.

### 3.2 renew-below mechanics
Recovery calls **raw `renew()`** (never `renew_auto`, which resets state CSV to D0 and breaks maturity),
with an explicit state CSV `= (min superseded state CSV) − δ` **and** extension CSV `= (min superseded
ext CSV) − δ` — strictly below *every* superseded tier of its kind (preserves the DW monotonic-extension
invariant). `renew()` is always +2 co-signs (extension+state), both disclosed. `verify_bundle` step 4 is
**extended** to also reject any superseded *extension* whose CSV ≤ the current extension CSV (today it only
race-checks states).

### 3.3 Floor-scoped presign guard (near-floor is a THEFT edge, not liveness)
If `S'` lands within δ of the floor, recovery cannot place the owner state strictly below it, and — with
the receiver holding `S'` per the theft path — that is unrecoverable theft, not a benign "exit-only". So
**refuse the latched presign up front** unless `state_csv(k) − 2δ ≥ d_floor` (reserve ≥ 2δ of runway).
Same shape as the sdk53 guard but floor-scoped: a coin too near its floor does its LN swap only after a
`refresh` restores runway. This bounds the edge instead of racing it.

### 3.4 Mnemonic-restore
`backup_txs` (holding both `tesr-<id>` and the marker) are **not** restored from the 12-word mnemonic
(`sqlite_manager.rs`), so a restored wallet loses its markers. Mitigation: (a) the recovery bundle export
(existing feature) MUST include markers, exported atomically with the marker write; (b) absent that, a
restored still-owned coin whose `se_num_sigs` exceeds its disclosed tier count has orphans at **unknown**
CSV ⇒ `reconcile` returns `NEEDS_EXIT_OR_REFRESH` (funds safe, coin exitable/refreshable) rather than
guessing a CSV. Never silently brick.

## 4. Commit / rollback / crash state machine

| Event | Server state | Client action | Result |
|---|---|---|---|
| presign+convey | ladder withheld (2.1); marker written first (3.1) | — | receiver has no ladder yet |
| **commit** (preimage→unlock) | `locked2`, ladder released | receiver fetches+adopts+finalizes; sender's coin transferred; sender's marker resolves as "not owned" → drop | atomic transfer (=sdk49) |
| **rollback** (expiry, `SETTLED_FINAL`) | ladder never released | sender `reconcile`s: fold orphan, renew-below, persist | coin whole, transferable |
| crash after co-sign, before marker | count +1, marker present (written first) | `reconcile` sees deficit, folds by marker CSV | recovered |
| crash after marker, co-sign failed | count unchanged, marker present | `reconcile` sees no deficit vs pre_presign_sig_count → discard phantom marker | no overcount |
| crash mid-recovery-renew | count +1/+2, each disclosed write-ahead | `reconcile` re-runs from persisted bundle, closes residual deficit | recovered |
| mnemonic restore, orphan outstanding | count +1, no marker | `reconcile` → NEEDS_EXIT_OR_REFRESH | funds safe, exitable |

## 5. Why it is safe
- **Theft window closed:** on rollback the receiver never obtains `S'` (2.1), so only the sender holds it,
  and after recovery the sender's current matures first. Pre-recovery the sender is also safe: a still-live
  latch blocks re-transfer (2.3), and only the sender holds `S'`, so a unilateral exit of `S_k` is
  unraced.
- **Never bricks, never over-counts:** `reconcile` is anchored to the authoritative enclave count with
  write-ahead disclosure, so disclosed converges to `se_num_sigs` from below and never overshoots.
- **Rollback is authoritative:** recovery fires only on `SETTLED_FINAL`+not-committed (2.2), so it cannot
  claw back a coin that still commits.
- **No enclave change, no trust regression:** `num_sigs` stays enclave-sourced; the fork guards
  (single-use/budget, which read `count_finalized_signatures`) are untouched; (A) reuses the latch's
  existing preimage gate.

## 6. Test plan (live SE + regtest RLN pair)
- **sdk54** commit: latched V2 swap; receiver cannot fetch the ladder pre-unlock (assert stripped), can
  post-unlock; adopts + re-transfers cleanly.
- **sdk55** rollback→reconcile→re-transfer: latch expires; sender still owns; `reconcile` folds+renews
  below; a later plain V2 transfer balances against the live enclave count.
- **sdk55b** near-floor: latched presign refused when runway < 2δ; coin exits/refreshes.
- **sdk55c** crash seams: inject failure at each row of §4; assert `reconcile` converges (no brick, no
  overcount) — driven by directly manipulating the marker/bundle + re-running reconcile.
- **sdk56** regression: V1 LN swap unchanged; sdk49 unchanged; single-use/budget gates unaffected;
  re-run sdk40–53.

## 7. Rollout order (each gated on its own green E2E)
1. Server 2.1 delivery-gate + 2.2 authoritative signal + 2.3 concurrency (one server build + migration).
2. `verify_bundle` superseded-extension race-check (3.2) — pure client, backward-compatible.
3. Client write-ahead marker + `reconcile` + floor-scoped guard (3.1–3.4).
4. sdk54/55/55b/55c/56 green → remove the sdk53 refusal.

Until step 4, the sdk53 refusal stays and LN swaps run on V1 (safe today).

## 8. Residual open questions for the §9-design review
- Q1: (2.1) Does stripping `tesr_ladder` from `get_msg_addr` interact with the receiver's *claim* flow that
  may need other msg fields pre-unlock? Confirm the V1 fields alone are enough for the receiver to do
  nothing-but-wait, and that the ladder is the only receiver-usable secret.
- Q2: (2.2) Adding a `resolved` flag + changing the reap predicate — does any other consumer depend on the
  current reap-on-insert behaviour? Enumerate readers of `lightning_latch`.
- Q3: (2.3) Refusing any transfer of a live-latched coin — does the legitimate SSP flow ever need to
  transfer a coin that is *its own* latch member mid-swap? Confirm no deadlock with the SSP settle path.
- Q4: (3.1) Is `signature_count/{id}` always reachable/consistent at reconcile time, and what does
  `reconcile` do if it is unavailable (fail-safe = do nothing, retry)?
- Q5: Multi-coin batches (some V2, some V1, some carriers) — per-coin markers + per-coin delivery-gate;
  confirm no cross-coin coupling.
- Q6: RGB carriers are never latched-with-a-ladder (sdk52/sdk53 sibling invariant) — confirm the
  delivery-gate + reconcile never touch a carrier.

## 9. Findings-addressed matrix
- FATAL theft window → §2.1 (withhold ladder) + §2.3 (block re-transfer while live).
- FATAL presign→marker crash → §3.1 rule 1 (write-ahead marker) + reconcile-against-count.
- SERIOUS phantom-fold overcount → §3.1 rule 1 (pre_presign_sig_count discriminator).
- SERIOUS commit-after-expiry clawback → §2.2 (expiry guard on unlock_by_preimage; SETTLED_FINAL only).
- SERIOUS mid-recovery crash drift → §3.1 rule 2 (write-ahead disclosure) + reconcile re-run.
- SERIOUS recovery non-atomic / idempotency → §3 reconcile keyed on live count, not marker presence.
- SERIOUS near-floor theft edge → §3.3 (reserve 2δ; refuse presign up front).
- SERIOUS recovery-renew not atomic → §3.1 rule 2 (persist per co-sign).
- SERIOUS mnemonic-restore marker loss → §3.4 (export markers; else NEEDS_EXIT_OR_REFRESH).
- SERIOUS superseded-extension not race-checked → §3.2 (extend verify_bundle step 4).
- SERIOUS reaped-latch None-conflation → §2.2 (resolved flag + tri-state status).
- SERIOUS statechain_in_latch_batch gap → §2.3 (gate on live-latch, not batch_timeout).
