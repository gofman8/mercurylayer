# V2 Server change — preimage-gated Model A co-sign (provisional / batch-tagged finality)

> ## ⛔ VERDICT: FLAWED — DO NOT IMPLEMENT AS WRITTEN (adversarial review, 5 lenses + synthesis)
>
> The batch-tagged-finality mechanism below **targets the wrong counter** and would **introduce a
> fund-loss double-spend**. Two FATALs, both confirmed against the code:
>
> 1. **`num_sigs` is the enclave `sig_count`, not `count_finalized_signatures`.** The receiver reads
>    `num_sigs` from the lockbox `signature_count/{id}` endpoint (`transfer_receiver.rs:46-67`, →
>    `lockbox/src/server.cpp:135`), incremented unconditionally on every partial signature and blind to
>    `batch_id`. Tagging rows in the server DB changes nothing `verify_bundle` reads, so the rollback-
>    brick is **unfixed** — and §2.3/§7 delete the sdk53 guard, re-enabling the brick.
> 2. **The single-use/budget fork guards DO read `count_finalized_signatures`** (`sign.rs:83,97`).
>    Excluding provisional rows there lets an owner tag a *real* terminal spend with a fabricated,
>    unvalidated `batch_id` → the row is skipped → the `finalized >= 1` / budget guard never fires →
>    **N conflicting broadcastable spends of a one-shot RGB node (INV-19 fork, fund-loss).** The
>    `sign.rs:295-299` comment exists precisely to prevent this under-counting; the design re-opens it.
>
> Plus two SERIOUS (once `num_sigs` is DB-sourced): promote is not atomic with `update_unlock_transfer`
> (crash → valid coin rejected); `unlock_by_preimage` has no expiry guard and the promote races the
> cleanup DELETE.
>
> **Consequence for the plan:** a "server + DB only, no enclave rebuild" fix is **impossible** — the
> counter `verify_bundle` consumes is the enclave's. The real options are enumerated in
> **§8 Corrected options** at the bottom. The sdk53 refusal **stays** until one of them ships and an
> E2E proves rollback→re-transfer balances against the enclave-sourced counter. Everything below the
> line is the REJECTED design, kept for the record.
>
> ---

Status: **REJECTED design (see verdict above), kept for the record.** Governs the one change that
unblocks full V1 removal (V2DEF-6). Everything else in the V2 migration is done and validated
(sdk40–53, 14 green E2Es); this is the last dependency.

## 1. The problem (why LN swaps can't use V2 coins today)

Every LN/SSP swap routes through `transfer_sender::execute`, which for a V2 coin pre-signs the Model A
receiver-paying state `S'` (`presign_receiver_state` → `cosign_tier` → `/sign/first` + `/sign/second`).
That co-sign is **unconditional**: it increments the SE's finalized-signature count
(`count_finalized_signatures` = `COUNT(*) WHERE statechain_id=X AND partial_sig_issued=true`) the moment
it is issued, and the sender records nothing locally.

- **Success path** (batch commits): the receiver adopts the augmented bundle (`current = S'`), the
  sender loses the coin, and `verify_bundle` balances (`num_sigs == v1_backups + tiers + superseded`).
  Proven by sdk49/sdk47.
- **Rollback path** (LN preimage never revealed, batch expires): the sender keeps the coin, but the SE's
  `num_sigs` has silently incremented by one orphan (`S'`). The sender's next V2 transfer builds a fresh
  `S''` (another co-sign); the new receiver's `verify_bundle` now sees `num_sigs` one (or more) higher
  than the disclosed tier count and **rejects** — the coin's future V2 transfers are bricked.

Interim mitigation shipped (sdk53): `execute` refuses a latched transfer of a V2 coin *before any server
interaction*. Safe, but it means **LN swaps run on V1 coins only** — so V1 cannot be removed.

## 2. The fix — batch-tagged provisional finality

A co-sign made as the Model A presign of a **latched** transfer is *issued* (so the client can build and
convey the augmented bundle) but does **not count** toward `num_sigs` until the batch commits. On commit
it is promoted to a normal finalized sig; on rollback it is dropped. This is a pure **server + DB**
change — the enclave co-signing primitive is untouched (no enclave rebuild), and `S'`'s bytes are
identical to any other tier co-sign (SE blindness preserved).

### 2.1 Data model

Add a nullable column to the finalized-signature table:

```sql
-- migration 000X_provisional_cosign.sql
ALTER TABLE statechain_signature_data ADD COLUMN batch_id TEXT DEFAULT NULL;
CREATE INDEX idx_sig_batch_id ON statechain_signature_data (batch_id) WHERE batch_id IS NOT NULL;
```

`batch_id IS NULL` ⟺ a committed (counted) signature — the entire pre-existing corpus, so the migration is
backward-compatible with zero data change. `batch_id = X` ⟺ provisional, pending batch `X`.

### 2.2 Counting rule (the linchpin)

`count_finalized_signatures` changes to exclude provisional rows:

```sql
SELECT COUNT(*) FROM statechain_signature_data
WHERE statechain_id = $1 AND partial_sig_issued = true AND batch_id IS NULL
```

Consequence: a provisional `S'` is invisible to `verify_bundle`'s `num_sigs` until it commits. The
receiver only ever runs `verify_bundle` **after** the batch unlocks (they cannot claim before), so at
their check time the sig has been promoted and counts — balanced. On rollback it never counts — the
sender's future transfers balance. **This is the whole safety argument.** (Every other reader of
`count_finalized_signatures` — single-use `finalized >= 1`, budget `finalized >= budget`, terminal query
— also correctly ignores an un-committed provisional sig, which is what we want: a provisional presign is
not yet a real spend.)

### 2.3 Tagging a co-sign as provisional

Thread the batch id through the sign round so `sign_second` tags atomically with finality:

- **mercurylib**: `PartialSignatureRequestPayload { …, #[serde(default)] batch_id: Option<String> }`
  (serde-default ⇒ every existing caller is unchanged).
- **client** `cosign_tier` gains an optional `batch_id`; `presign_receiver_state` forwards the transfer's
  `batch_id`; `transfer_sender::execute` passes `batch_id` into the presign for a latched transfer (and
  removes the sdk53 refusal). Non-latched presign passes `None` (unchanged, still commits immediately —
  sdk49 semantics preserved).
- **server** `sign_second`: `set_partial_sig_issued(pool, nonce, statechain_id, batch_id)` writes
  `partial_sig_issued = true, batch_id = $batch_id` in the same UPDATE. When `batch_id` is `None` the
  behaviour is byte-identical to today.

Only the Model A presign carries a `batch_id`. The transfer's *own* key-rotation co-sign path and all V1
co-signs pass `None` and commit immediately, exactly as now.

### 2.4 Commit and rollback points

- **Commit** = the batch unlocks (the LN preimage is revealed): the existing `unlock_by_preimage` (and the
  classic `transfer_preimage`/sender-confirm path) already iterate the batch's statechain_ids to flip
  `update_unlock_transfer`. Add, in the same loop, a **promote**:
  ```sql
  UPDATE statechain_signature_data SET batch_id = NULL
  WHERE statechain_id = $1 AND batch_id = $2
  ```
  Idempotent; promoting an already-null row is a no-op.
- **Rollback** = the batch expires without unlocking. A provisional row that is never promoted **already
  never counts** (2.2), so correctness needs no action. For hygiene, a lazy cleanup on the expiry path
  (or the existing latch-expiry cron) deletes provisional rows of expired batches:
  ```sql
  DELETE FROM statechain_signature_data WHERE batch_id = $1 AND partial_sig_issued = true
  ```
  Cleanup is **not** on the safety-critical path — the counting rule is.

## 3. Why this is atomic and safe

1. **No orphan on rollback.** A rolled-back presign is provisional forever ⇒ never counted ⇒ the sender's
   coin still balances for a future transfer. The interim sdk53 refusal is removed because the failure it
   guarded against can no longer occur.
2. **No premature ownership.** The receiver cannot `verify_bundle`-accept before unlock (claim is gated by
   the existing latch); the promote runs *at* unlock, in the same DB step that opens the claim window, so
   the receiver never sees an un-promoted `S'` at their check. Order: unlock ⟹ promote ⟹ receiver claims &
   verifies. (Promote and `update_unlock_transfer` must be in one transaction — see R-1.)
3. **Current-owner-wins preserved.** `S'` sits at CSV `owner_current − δ`; the CSV-race check in
   `verify_bundle` is unchanged. A provisional `S'` that never commits also never reaches the receiver
   (rollback ⇒ bundle not adopted), so no party but the sender ever holds it, and the sender never
   self-harms.
4. **SE blindness preserved.** `batch_id` is server-DB metadata; the sighash the enclave signs is
   unchanged and byte-indistinguishable from any tier/plain co-sign. No enclave rebuild.
5. **V1 untouched.** Every V1 flow passes `batch_id = None`; `count_finalized_signatures` for a V1 coin is
   unchanged (all its rows are null-batch). The migration adds a nullable column with a default.

## 4. Adversarial checklist (to be run as a review workflow before coding)

- **R-1 (atomicity of promote+unlock).** If unlock flips `update_unlock_transfer` but the promote fails
  (crash between the two writes), the receiver could claim while `S'` is still provisional ⇒
  `verify_bundle` under-counts ⇒ **rejects a valid coin** (liveness, not theft). MUST be one SQL
  transaction. Fail-closed: if promote fails, the unlock fails too.
- **R-2 (double-promote / replay).** Presenting the preimage twice, or two batches referencing the same
  statechain_id. Promote is idempotent (`SET batch_id=NULL WHERE batch_id=$2`); a sig can belong to only
  one batch (it is written once at `sign_second`). Confirm a statechain_id cannot be latched under two
  live batches simultaneously (existing `statechain_in_latch_batch` guard).
- **R-3 (provisional counted by a sibling reader).** Audit every reader of
  `count_finalized_signatures` and any direct `partial_sig_issued` query (single-use gate, budget gate,
  `get_spend_budget`, terminal receipts) — all must exclude `batch_id IS NOT NULL`, or share the amended
  function. A provisional presign must not, e.g., trip the single-use `>= 1` refusal on the sender's own
  next legitimate co-sign.
- **R-4 (grief: cheap provisional spam).** Can an attacker create provisional rows to bloat the table or
  mislead a counter? Tagging requires a valid presign (auth-sig gated, same as any co-sign) over the
  attacker's own coin; provisional rows are bounded by live batches and reaped on expiry. No amplification
  beyond an ordinary co-sign.
- **R-5 (expiry vs. late unlock race).** Preimage arrives right at expiry: `transfer_preimage` already
  fails-closed after expiry (§ existing audit [2]). Ensure promote is only reachable on the same
  not-yet-expired path, so a sig cannot be promoted after the claim window closed.
- **R-6 (partial batch).** A batch with several statechain_ids where only some are V2. Promote loops all
  ids; a null-batch (V1) row is a no-op. Mixed batches are fine.

## 5. Test plan (live SE + regtest, mirrors the existing LN E2Es)

- **sdk54 — latched V2 transfer COMMITS.** Set up an RLN pair + channel (as sdk18-22); a V2 coin swaps
  Mercury→LN; on preimage reveal the receiver adopts the ladder and `verify_bundle` passes; the coin then
  transfers **again** cleanly (proves no residual count skew). Balances/atomicity as sdk18.
- **sdk55 — latched V2 transfer ROLLS BACK.** Same setup; the LN payment fails / latch expires; the
  sender still owns the coin; a subsequent **plain** V2 transfer of that coin succeeds and the receiver's
  `verify_bundle` balances (the orphan never counted). This is the exact scenario sdk53 currently guards
  against — sdk55 proves the real fix, and the sdk53 refusal is deleted.
- **sdk56 — mixed/regression.** A V1 LN swap still works unchanged; a V2 non-latched transfer (sdk49)
  still works; single-use/budget gates still fire correctly with a provisional row present.
- Re-run sdk40–53 (no regression) + the V1 LN suite (sdk18-22).

## 6. Deploy

Server rebuild + migration. Per ops constraint: **never `--force-recreate mercury-server`** — `docker cp`
the new binary + apply the migration, then restart the container. Stage on regtest, run §5, only then
consider mainnet. The enclave/lockbox image is **not** rebuilt (no enclave change).

## 7. After this lands

Removing the sdk53 refusal + green sdk54/55/56 closes the LN-latch dependency. Then V2DEF-5 (flip
`UTEXO_PROTOCOL_DEFAULT` to 2, migrate the suite) and V2DEF-6 (delete V1, purge docs) can proceed with no
remaining functional gap.

---

## 8. Corrected options (post-review — supersedes §2–§7)

The review proved the counter `verify_bundle` reads is the **enclave** `sig_count`, incremented on every
co-sign with no rollback. Any fix must reconcile *that* counter with a rolled-back presign. Four ways:

- **Option A — client-side rollback-recovery (no server/enclave change). RECOMMENDED.**
  Full-disclosure counting already reconciles `verify_bundle`'s `expected` with the enclave `sig_count`
  (that is how sdk49 balances). So an orphaned `S'` is fixed *on the client* by disclosing it. Flow:
  a latched presign records a local `pending-latch{batch_id, S'}` marker. Before the coin's next
  transfer (or on a maintenance pass), the sender resolves each marker: batch **committed** ⇒ the coin
  is gone, drop the marker; batch **expired/rolled-back** ⇒ fold `S'` into `superseded_states` **and**
  renew the coin to a state strictly *below* `S'` (CSV `≤ owner_current − 2δ`) so the CSV-race check
  still holds, then drop the marker. After recovery `expected == enclave sig_count` again and the coin
  transfers. **Safety:** between rollback and recovery the coin fails *safe* — a transfer attempt makes
  the receiver's `verify_bundle` reject (num_sigs > disclosed), never a fund loss; unilateral exit is
  fine because the never-conveyed `S'` is held only by the sender, whose own state matures first. No
  enclave rebuild, no server-trust change, no new fork-guard surface. Cost: a stateful client recovery
  path + a renewal co-sign on the rare rollback. This sidesteps **every** finding above (they were all
  specific to the server-DB mechanism).
- **Option B — enclave rebuild (batch-aware `sig_count`).** Make the enclave hold a presign provisional,
  promote on unlock, skip on rollback. True server-side gating, enclave stays the trust anchor — but an
  SGX enclave rebuild + redeploy, the highest-risk change in the system. Contradicts the "no enclave"
  premise entirely.
- **Option C — re-source `num_sigs` to the server DB.** Serve `num_sigs` from `count_finalized_signatures`
  (batch-aware) instead of the enclave, split into `count_finalized` (receiver-facing, excludes
  provisional) vs `count_issued` (fork-gates, counts all), validate `batch_id` membership at
  `sign_second`, forbid tagging single_use/budgeted coins, and make promote+unlock transactional with an
  expiry guard (findings 1–4). **Weakens the trust model:** the operator (not the tamper-resistant
  enclave) becomes the authority on the anti-theft count — a malicious operator could under-report to
  hide a co-signed state. Large surface, security regression. Not recommended.
- **Option D — keep the sdk53 guard permanently.** LN swaps stay on V1 coins; the SDK keeps a V1
  "swap coin" lane even after V2 default. Zero new risk, but V1 is not fully removed (contradicts the
  V2DEF-6 goal).

**Recommendation: Option A.** It achieves full V1 removal, keeps the enclave as the trust anchor, needs
no server/enclave surgery, and fails safe. Its own design + adversarial review should precede code.
