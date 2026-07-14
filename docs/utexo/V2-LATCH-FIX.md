# V2 LN-latch atomicity — complete design (delivery-gate + reconcile-recovery)

Status: **design, pause-before-coding** (per decision). Full standalone spec for making LN-latched V2
transfers atomic. Supersedes `V2-SERVER-LATCH.md` (rejected: wrong counter) and `V2-LATCH-RECOVERY.md`
§1–8 (rejected alone: theft window).

**Review history:** review 1 (server-DB) → FATAL. review 2 (pure client) → FATAL theft window. review 3
(this doc's v1) → **SOUND-WITH-EDITS, no FATAL** — the approach is validated; 7 SERIOUS + 2 MINOR were
precise, bounded edits. **All 7 SERIOUS edits are now applied inline (tagged [S1]–[S7], [M1]/[M2]);** the
two core re-specs (S1 whole-message withholding, S2 state-only renew) need a confirmation review before
implementation. No code until that confirmation returns SOUND.

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

> **Revised per review 3 (SOUND-WITH-EDITS): S1, S3, S5, S6 applied below.** The prior "strip the
> `tesr_ladder` field" construction was unbuildable — the whole `TransferMsg` is one opaque ECIES blob the
> server cannot parse.

> **Revised per confirmation review (round 4): C1 SSP descriptor, polarity, plain-batch belt applied.**

### 2.1 Withhold the WHOLE message until unlock [S1] + a pre-pay coin descriptor for the SSP [C1]
The whole `TransferMsg { …, tesr_ladder }` is ECIES-encrypted to the receiver's auth key as **one opaque
blob** (`lib/src/transfer/sender.rs:132-151`); the server stores/serves exactly that ciphertext
(`get_statechain_transfer_messages` → `get_msg_addr:108-116`) and cannot decrypt or drop a field. So the
gate is **whole-message withholding**: `get_msg_addr` joins `statechain_transfer → lightning_latch` and,
for any coin whose latch is **live and un-committed, withholds the entire `encrypted_transfer_msg` while
`locked2 == true`**, serving it **iff `locked2 == false`** (the commit path clears `locked2=false`;
`insert_new_transfer` sets `locked2 = is_lightning_latch = true`, `transfer_sender.rs:99-100`; withhold
*during* lock, serve *after* commit — never the inverse). Gate on `locked2` only, NOT
`is_all_coins_unlocked` (circular — it needs the decrypted msg to clear `locked`, deadlocking the latch).

**SSP pre-payment descriptor [C1 — resolves the pay-invoice deadlock].** In the pay-invoice swap the SSP
**is** the receiver and must run its mandatory value/asset gate (`execute_pay` → `peek_pending_transfers`
→ `get_msg_addr`, audit [3]/[4]) BEFORE it pays and BEFORE commit — but whole-message withholding hides
the coin from that peek. Resolution: the sender also posts an **owner-signed coin descriptor**
`{statechain_id, batch_id, funding_amount, recipient_pubkey, rgb_consignment?}` signed by the sender's
auth key, served pre-commit via a new `/transfer/coin_descriptor/<batch_id>` (or an extra field on
`batch_statechains`). The SSP validates value ≥ invoice + recipient == its key + consignment against the
descriptor and pays; the descriptor carries **no `tesr_ladder`/`S'`**, so it is not a broadcastable claim.
The full msg (with `S'`) is released only at commit, giving the SSP its unilateral-exit ladder *after*
paying. **Trust note:** the SSP's pre-pay guarantee is value+recipient+consignment (owner-signed) plus
SE-liveness for the post-commit ladder — acceptable because the SSP is the operator-aligned swap
counterparty, not an arbitrary user; a fully-trustless receiver would need an adaptor-signature on `S'`
(out of scope, tracked as a future hardening). The descriptor is amount/recipient/consignment only — it
reveals nothing the `batch_statechains` mapping doesn't already, and no pre-signed state.

Because the server cannot tell a V1 blob from a V2 blob (both opaque), add a **plaintext
`protocol_version` marker** to `transfer_update_msg` so the gate scopes to V2 only; absent that, accept
that V1-latch msgs are also withheld until commit and carry a **V1-latch regression test** (the V1
receiver re-polls `get_msg_addr` post-commit — confirm `execute()` does). A withheld-msg receiver's
`validate_encrypted_message` errors "missing TES-R ladder" and waits
(`clients/libs/rust/src/transfer_receiver.rs:597`). A V2 transfer with **no** latch (`batch_id=None`) is
served normally (sdk49). **Latch-before-msg / plain-batch belt [S3, scoped]:** withhold a V2 blob whose
`statechain_id` has a **live LN latch** (`is_lightning_latch` batch) that is not yet committed OR whose LN
latch row does not yet exist for its `batch_id`; a **plain** (non-LN) batch (`is_lightning_latch=false`,
`locked2=false`, no `lightning_latch` row) is served normally — the belt applies only to LN-latch batches,
so it cannot brick a plain V2 batch transfer.

### 2.2 Finality = one DB compare-and-swap on `resolved` [S3]
Do **not** gate commit on a wall-clock compare (two decoupled clocks let a late `unlock_by_preimage`
commit after the client saw the latch expired → clawback fund-loss). Add a `resolved` column
(`NULL | 'committed' | 'expired'`) and make it the single serialization point:
- **Commit** (any `locked2`-flip path — `unlock_by_preimage` AND `transfer_unlock` AND `transfer_preimage`)
  runs, in one transaction: `UPDATE lightning_latch SET resolved='committed' WHERE batch_id=$1 AND resolved
  IS NULL AND expires_at > now()` and flips `locked2` **iff that UPDATE hit the row**.
- **Expiry sweeper**: `UPDATE … SET resolved='expired' WHERE resolved IS NULL AND expires_at <= now()`
  (eager, not lazy).
- **Status** `/transfer/latch_status/<batch_id>` and recovery read `resolved` only:
  `LIVE (NULL)` / `SETTLED('committed'|'expired')` / `UNKNOWN (DB error)`. Recovery acts only on
  `resolved='expired'`; `UNKNOWN` ⇒ wait. "Expired is final" is now a CAS immune to clock skew.

### 2.3 Concurrency: refuse a *conflicting* transfer of a live-latched coin [S6]
Define **`live := row exists AND resolved IS NULL AND expires_at > now()`** everywhere. The initiation
transfer of the swap (`start_lightning_swap` inserts the latch **then** calls `execute(.., Some(batch))`,
`lightning.rs:69-85`) must NOT be refused — so refuse a transfer only when the coin has a **live** latch
with `batch_id ≠ new_batch_id` (or `None`) **and** a transfer msg for that live batch already exists. The
client also refuses a second transfer of a coin with an unresolved local marker. Commit sets
`resolved='committed'` (2.2) so a received coin unfreezes immediately; the eager sweeper resolves
rolled-back rows to `'expired'` so they neither block re-transfer nor strand recovery.

### 2.4 Reap timing [S5]
Never reap a `resolved='committed'` row before its `expires_at` — reap on `resolved='expired' OR (resolved
='committed' AND expires_at < now())`. Otherwise a committed row reaped at unlock collapses `validate_batch`
to the short `batch_timeout` (120s) < LN settlement time, stranding the receiver's finalize. Simpler
alternative: make `validate_batch` return `Success` whenever `is_all_coins_unlocked` regardless of an
absent latch clock (a fully-unlocked batch is terminal).

## 3. (B) Client recovery — the reconcile primitive

> **Revised per review 3: S2 (state-only renew), S4 (serialized), M1 (refused-renew fallback) applied.**

The core operation is **`reconcile(coin)`**, keyed on the authoritative enclave count, not on marker
presence. It runs under a **per-coin persisted advisory lock** (same shape as `acquire_signfirst_lock`,
`sign.rs:23`) so two concurrent runs (a maintenance pass + a transfer-triggered recovery, or two wallet
instances) cannot each renew and double-count [S4]. The whole fold→renew→persist is one critical section;
idempotent and crash-re-runnable:

```
reconcile(coin):                                # holds per-coin advisory lock
  bundle = load(coin);  se = signature_count(coin)          # live enclave truth (fail-safe: unreachable ⇒ return WAIT)
  if disclosed(bundle) == se: drop_marker(coin); return OK   # already balanced
  # deficit orphan co-signs exist. Fold each by the CSV its marker recorded:
  for each marker m of coin (a presign records {orphan_txid, orphan_csv, pre_presign_sig_count}):
      if se <= m.pre_presign_sig_count: discard m            # phantom: co-sign never landed (S-phantom)
      elif orphan not already in superseded_states:
          push {txid,csv,out_value} to superseded_states; persist(bundle)   # write-ahead disclosure
  if disclosed(bundle) < se: return NEEDS_EXIT_OR_REFRESH     # orphans at UNKNOWN csv (marker lost) — never guess
  # Every orphan disclosed. Restore maturity dominance with a STATE-ONLY renew [S2]:
  target = (min superseded state csv) - δ
  if target < d_floor: return NEEDS_EXIT_OR_REFRESH          # no runway (guarded up front by §3.3, belt-and-suspenders)
  r = renew_state_only(bundle, target)                        # ONE co-sign; re-sign only the state, NOT the extension
  if r == SE_REFUSED: return NEEDS_EXIT_OR_REFRESH            # budget/single-use/epoch refused the renew (M1)
  persist(bundle)                                             # disclose the +1 before returning
  assert verify_bundle(bundle, signature_count(coin))         # count AND maturity (states + extensions)
  drop_marker(coin); return OK
```

**Why state-only [S2].** A latched presign lowered only a *state* (`presign_receiver_state` pushes only
`superseded_states`, never a superseded extension — `tesr.rs:307`), so only a *state* needs re-signing to
regain maturity dominance. Reusing raw `renew()` (always +2, extension+state) is wrong here: on the common
first-transfer rollback `superseded_extensions` is empty so `min(∅)−δ` is undefined, and on mainnet
(`δ==δ_e==36`) it would install a new extension *equal to* a superseded one, failing the extended step-4
check. So recovery adds a **`renew_state_only(bundle, csv_d)`** primitive: build+co-sign a fresh state at
`csv_d` spending the current extension's output, push the old state to `superseded_states`, leave the
extension untouched. +1 co-sign, +1 disclosure — balanced.

### 3.1 Write-ahead discipline (kills every crash-brick finding)
Two rules make the enclave count and the disclosed set never diverge unrecoverably:
1. **Write-ahead marker on EVERY latched co-sign [C5]**: before *any* co-sign in a latched flow — the
   presign of `S'` AND each recovery `renew_state_only` co-sign — persist a marker
   `{batch_id, cosign_txid, cosign_csv, out_value, pre_cosign_sig_count, kind}`. The tier txid/csv are
   deterministic pre-witness (`lib/src/tesr.rs`), so no signature is needed. If the co-sign then fails (SE
   409/network/crash), `reconcile` reads the live count: if it did **not** advance past
   `pre_cosign_sig_count`, the co-sign never landed ⇒ phantom ⇒ discard without folding (fixes phantom-fold
   overcount). If it DID advance but the tx was never persisted (crash mid-recovery), the co-signed tier is
   an orphan the client can no longer re-sign (MuSig2 nonce guard, `sign.rs`); `reconcile` folds it into
   `superseded_states` by its marker CSV and renews below it — **uniformly, exactly like a presign orphan**.
   So a crashed recovery renew is just another orphan the next `reconcile` pass absorbs; the loop converges
   (each pass discloses the outstanding orphan + renews strictly below), degrading to `NEEDS_EXIT_OR_REFRESH`
   only when state runway is finally exhausted — which is the honest "safe exit/refresh", not a brick.
2. **Write-ahead disclosure**: every SE co-sign (the presign, and each of recovery's renew co-signs) is
   immediately followed by persisting its disclosure into the bundle *before* the next co-sign or convey.
   So `disclosed` is at most one co-sign behind `se`, and `reconcile` closes that residue on re-entry. A
   mid-recovery crash therefore never leaves an un-disclosed co-sign that a later run double-counts.

### 3.2 verify_bundle maturity extension — PER-PREVOUT [C4]
Ordinary DW renewals create superseded extensions, and today `verify_bundle` step 4 race-checks only
states. Extend it to race-check superseded extensions too — **but per-prevout, not flat**. A flat "reject
any superseded ext CSV ≤ current ext CSV" wrongly rejects a **rolled-over** ladder: `rollover`
(`tesr.rs:243-272`) installs a fresh deepest extension at a *different prevout* (the self-split output) and
never clears `superseded_extensions`, so on regtest a renewed→rolled-over coin has superseded exts {12,9}
vs a fresh deepest ext at 12, and `12 ≤ 12` falsely rejects. Superseded level-0 extensions spend the
*trigger* output; they only race the level-0 current extension, not the level-1 extension (a different
prevout). **Correct check:** for each superseded extension, compare its CSV **only against the current
extension that shares its `input[0].previous_output`** (i.e. the extension at the same ladder level);
require the current one strictly lower. Superseded extensions of earlier (rolled-over) levels are counted
(step 3) but race-checked only against the extension they actually contend with. This is why recovery is
state-only (§3): it never adds a superseded extension, so it never trips even a flat check — but the
per-prevout fix is needed so a *rolled-over* coin can be latch-recovered without `reconcile`'s
`assert verify_bundle` panicking. Pure client, backward-compatible.

### 3.3 Presign guards up front (near-floor is a THEFT edge; budgeted/epoch coins) [S2/M1]
- **State runway:** if `S'` would land within δ of the floor, recovery cannot place the owner state
  strictly below it, and — with the receiver holding `S'` on the theft path — that is unrecoverable theft,
  not benign "exit-only". **Refuse the latched presign up front** unless `state_csv(k) − 2δ ≥ d_floor`
  (reserve ≥ 2δ of *state* runway). Recovery is state-only, so it consumes no *extension* runway — the
  extension-floor concern does not arise. Repeated rollbacks on one coin each consume one state level;
  when runway is exhausted the coin degrades gracefully to `refresh` (§3.4), never to theft.
- **Budget/epoch:** recovery's renew is an SE co-sign that the untouched fork guards can refuse
  (single-use, `sig_budget`, epoch-deadline — `sign.rs:83-124`). So also **refuse the latched presign up
  front** on a single-use / budgeted / epoch-deadlined coin (a carrier is already never laddered, sdk52).
  Then recovery's renew can always proceed; if it is nonetheless refused, `reconcile` returns
  `NEEDS_EXIT_OR_REFRESH` rather than asserting.

Same shape as the sdk53 guard but scoped: a coin that fails either guard does its LN swap only after a
`refresh`. This bounds the edges instead of racing them.

### 3.4 Mnemonic-restore [S7 — corrected: not "exitable", `import_recovery_bundle` is the sole recovery]
`backup_txs` hold BOTH the `tesr-<id>` bundle AND the marker, and **neither is derived from the 12-word
mnemonic** (`sqlite_manager.rs`). So a mnemonic-**only** restore recovers *nothing* off-chain: no coin, no
ladder, no marker — `F` is a bare `P2TR(A)` whose un-broadcast trigger is gone. The earlier "funds safe,
exitable" claim was **false** for mnemonic-only restore. Correct policy:
- The **recovery-bundle export** (existing `export_recovery_bundle`) is the ONLY recovery for a latch-
  orphaned V2 coin, and it MUST include markers. Make export **mandatory/automatic** for any coin that
  enters a latched transfer (write-ahead with the marker), so a latched coin always has an off-wallet
  recovery artifact.
- With the recovery bundle imported, `reconcile` runs normally. Without it, the coin is unrecoverable by
  mnemonic alone — a property already true of every V2 coin (the ladder is off-chain), now made explicit
  and forced for latched coins. Never claim mnemonic-only exitability.

## 4. Commit / rollback / crash state machine

| Event | Server state | Client action | Result |
|---|---|---|---|
| presign+convey | ladder withheld (2.1); marker written first (3.1) | — | receiver has no ladder yet |
| **commit** (preimage→unlock) | `locked2`, ladder released | receiver fetches+adopts+finalizes; sender's coin transferred; sender's marker resolves as "not owned" → drop | atomic transfer (=sdk49) |
| **rollback** (expiry, `SETTLED_FINAL`) | ladder never released | sender `reconcile`s: fold orphan, renew-below, persist | coin whole, transferable |
| crash after co-sign, before marker | count +1, marker present (written first) | `reconcile` sees deficit, folds by marker CSV | recovered |
| crash after marker, co-sign failed | count unchanged, marker present | `reconcile` sees no deficit vs pre_presign_sig_count → discard phantom marker | no overcount |
| crash mid-recovery-renew | count +1, renew co-sign has its own write-ahead marker [C5] | `reconcile` re-run folds the orphaned renew tier by its marker CSV + renews below it (uniform with a presign orphan) | recovered (or safe exit/refresh at runway exhaustion) |
| **mnemonic-only** restore (no recovery bundle) | count +N, no coin/ladder/marker | nothing off-chain survives; only `import_recovery_bundle` recovers | coin needs its recovery bundle (true of every V2 coin) — NOT mnemonic-exitable |

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
