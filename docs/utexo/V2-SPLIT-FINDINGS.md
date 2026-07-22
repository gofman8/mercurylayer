# V2 split-transfer design — FLAWED verdict + FATAL holes found in the LIVE V2 lane

## 🔴 B1 — LIVE THEFT VECTOR (was shipped in the V2 default at 434d334; default REVERTED)

**Not a future design constraint — an exploitable theft in the shipped default. No collusion, one SDK call.**

| Fact | Evidence |
|---|---|
| `T` has NO timelock, spends `F`, fully co-signed at `establish` | `lib/src/tesr.rs:145` (`TRIGGER_SEQUENCE=0xFFFF_FFFD`), `:159` `lock_time:0`, asserted `:352-353`; co-signed `clients/libs/rust/src/tesr.rs:130` |
| The split tx spends the SAME `F` | `lib/src/transaction.rs:402-412` builds its input from `coin.utxo_txid/vout`; `clients/libs/rust/src/tesr.rs:125-126` sets `f_txid/f_vout` from the same fields |
| `split_coin` had NO ladder check | `clients/libs/rust-sdk/src/transfer.rs:387-406` — guards were CONFIRMED / !dup / !carrier only |
| Every V2 transfer leaves the SENDER a retained `T` | `presign_receiver_state` clones and never deletes (`tesr.rs:299,302`); there is NO deletion path for `tesr-*` |
| The SDK performs the attack | `unilateral_exit(Some(ids))` filters the explicit-id branch on carrier status only (`wallet.rs:1016-1028`), then `exit_pass` broadcasts `T` with no `outpoint_spent` gate (`tesr.rs:389-406`) |

**Attack:** Alice receives a V2 coin → pays Bob a non-exact amount (⟹ `split_coin`; Bob gets a **V1**
sub-coin, `transfer_sender.rs:312`, funded by the un-broadcast split tx) → Alice `unilateral_exit`s the
PARENT. `T` confirms, `F` is consumed, **Bob's split tx is permanently dead**, Alice's ladder pays her the
full parent value.

**The race is rigged:** `T` is v3/TRUC + P2A — fee-bumpable by anyone, forever. The split tx is v2 with a
frozen fee and no RBF headroom (the parent's SE budget is set to exactly 1 and the split consumes it,
`transfer.rs:458-464`). Alice wins deterministically.

**Bob's due diligence is meaningless:** his `terminal_parents` check (`transfer_sender.rs:290-294`) returns
true. He cannot see `T` — the ladder is never conveyed on the V1 lane and the SE has never seen it
(`tesr.rs:47`).

**The code's load-bearing claim is FALSE for V2** (`transfer.rs:455-457`: *"No later withdraw/transfer/
backup of the parent can be signed — the branch cannot be double-spent even by a malicious sender"*). It
rests on the V1 premise that every spend of `F` needs a FRESH SE co-sign. V2 breaks it at the root: `T` was
co-signed at `establish`, long before `set_spend_budget`. **A budget bounds future co-signs; it cannot
retract an issued signature.** Strictly WORSE than V1, where the parent's backup is locktimed above the
branch and the branch always matures first (INV-4).

### Status
- **Default REVERTED to V1** (V2 opt-in via env) — exposure closed.
- **HF-1 landed**: `split_coin` refuses a laddered coin (hard error beats silently voiding the receiver).
- **Follow-ups still open**: HF-2 (teach the planner `splittable`, so HF-1 doesn't turn into a hard
  transfer() failure when a safe coin exists); HF-3 (same gate in `mint_piece`, `transfer.rs:365-380`);
  HF-4 (`unilateral_exit` explicit-id branch must filter on `CoinStatus` — a WITHDRAWN parent must not be
  exitable; stops the SDK being the weapon and kills the accidental-loss variant); HF-5 (delete the false
  claim at `transfer.rs:455-457`).

### THE FIX — in-ladder split (V2-DESIGN §5.4), not a gate
The gate is a feature amputation: `presign_receiver_state` runs on EVERY V2 transfer, so essentially every
circulating V2 coin is conveyed-ladder ⟹ V2 coins could not pay non-exact amounts (violating REQ-2). Its
escape hatch (`reanchor`, 112 vB + a confirmation per non-exact payment) negates V2's "0 vB rent / ~320×
footprint win" — and may not even work (does `withdraw` on a conveyed V2 coin have SE sign-budget headroom?
external review F2 / migration 0008 — MUST be checked).

A split `SP` is a **state tier** spending `X_m.out[0]` at `nSequence Δ_{k+1}`, N child resting outputs,
Σout = Σin − fee_committed, + P2A. `build_trigger` (`lib/src/tesr.rs:236-248`) is the ONLY builder that
touches `f_txid/f_vout`; every other tier spends its parent's output. So an SP is a **DESCENDANT of `T`**:
the *trigger* stops racing it.

> ⚠️ **CORRECTION (split-child-bundle design review):** an earlier version of this section, and the
> doc-comment on `build_split_state`, claimed the in-ladder split **"dissolves"** B1. **That is wrong and
> was corrected.** `SP` stops the *trigger* being a rival, but the **parent's own retained STATE over
> `X_m.out[0]`** becomes the new rival — the parent still holds a co-signed state that spends the same
> outpoint `SP` spends. **B1 is RELOCATED, not dissolved.** The child is only safe if it verifies the
> **parent's full-disclosure census** (parent `num_sigs` == parent's disclosed tiers), which is only
> meaningful if the parent is **terminalized first** and that terminality is actually ENFORCED at
> co-sign time. See "Split-child-bundle design — FLAWED" below: that census depends on server/enclave
> guarantees that do not currently hold.

---

## S1 — FATAL (fund-loss): `verify_bundle`'s exact-count linchpin is paddable
`clients/libs/rust/src/tesr.rs:508-529`.
```
expected = v1_backups + tiers.len() + superseded_states.len() + superseded_extensions.len()
```
- Exit tiers are parsed + structurally checked (`:458-503`). **Superseded entries are only `.len()`-counted**
  — never deserialized, never linked to the ladder, never signature-checked.
- Check 4 is `if let Some(csv) = s.csv { ... }` — an entry with **`csv: None` is silently skipped**.

**Attack.** A sender who has reached `num_sigs = N+1` (one hidden low-CSV state paying themselves) pads one
junk `TesrTier { txid:"", signed_tx:"", out_value:0, csv:None }`. `expected` becomes `N+1` ⟹ check 3 passes;
check 4 skips the `None`. `verify_bundle` **ACCEPTS**. The receiver takes the coin; the sender later
broadcasts the hidden state (which matures first) and takes it back. The doc-comment at `:433-436` claiming
a hidden state is "impossible" is **false**.

**Root cause (mine).** `superseded_states` was added to fix sdk49's count mismatch (full-disclosure
counting) — introducing attacker-controlled terms into the very equation meant to make hidden states
impossible.

**Required fix.** Every term of `expected` must correspond to a *real, co-signed* tier of *this* ladder:
1. **Parse** every superseded entry (reject bad/empty hex) — no unparseable tier may contribute.
2. **CSV required, unconditional**: `let csv = s.csv.ok_or(...)?` then require `csv > final_csv` (states).
3. **Ladder linkage**: each superseded state must spend a disclosed-or-superseded extension's `out[0]`;
   each superseded extension must spend the trigger's or a level state's `out[0]`.
4. **Signature verification** (the real guarantee): parsing does not prove a co-sign — a structurally valid
   but never-co-signed tx would still pad the count. Each counted tier's witness must verify as a valid
   schnorr/MuSig2 signature by the aggregate key `A` over its own sighash. Without this, `expected` remains
   forgeable. (Note the *exit* tiers are not signature-verified today either — same class.)
5. Unit tests: `csv:None`, empty `signed_tx`, off-ladder outpoint, unsigned tier → each must REJECT, and be
   shown ACCEPTED before the patch.

## S2 — FATAL (fund-loss): `v1_backups` is attacker-supplied and unvalidated on the V2 path
`clients/libs/rust/src/transfer_receiver.rs:600-604` passes `transfer_msg.backup_transactions.len()`
straight into `expected`; `:635` gates `validate_signature_scheme` on `protocol_version < 2`, so **nothing
inspects the vector's interior on V2** (only `get_tx0_outpoint` first-by-`tx_n` and
`verify_latest_backup_tx_pays_to_user_pubkey` `.last()`).
- **Count padding**: duplicate `tx1` → `[tx1, tx1, tx2]`. Same prevout ⟹ one group; first/last unchanged.
  `expected` inflates to match a hidden-state `num_sigs`. Middle entries need not be valid txs. Unbounded.
- **Locktime inversion**: build the receiver-paying backup at `L+interval` (not `L−interval`) and retain
  one's own at `L`. INV-5 (`ladder_decrements_by_interval`) is the only enforcement — and it is skipped.

**Strict V1→V2 regression**: V1 rejects both (`:615` + `validate_signature_scheme`). Root cause (mine):
V2DEF-1 gated the V1 structural check off for V2 without replacing it.

**Required fix.** Validate the backup vector structurally on the V2 path too (count, per-group uniqueness,
decrementing-locktime), or derive `v1_backups` from an authoritative source rather than the sender's vector.

## B0 — FIXED (commit 5c42667): root-only laddering
`claim()`'s establish loop filtered only `CONFIRMED/!dup/!single_use/!carrier` — no root check — so a split
**sub-coin** got laddered under the V2 default; its `F` is an un-broadcast split output ⟹ `exit_pass`
broadcasts a trigger with no prevout ⟹ silent stall, `wait_blocks:0` forever ⟹ unexitable via the SDK.
Fixed by requiring F on-chain (electrum `transaction_get`), fail-closed. (An earlier `branch-<id>` proxy
broke root laddering because `get_backup_txs` does `fetch_one(..)?` — a missing row returns `Err`, conflating
"root" with "db error"; sdk48 caught it.)

## B1 — structural constraint on V2 split-transfer (not yet addressed)
The conveyed trigger has **no timelock** (`TRIGGER_SEQUENCE = 0xFFFF_FFFD`, `lib/src/tesr.rs:145`), so every
prior owner of a V2-adopted coin holds a free, immediate spend of `F`. A branch-conveyed sub-coin's root hop
is the split tx, which **also** spends `F` ⟹ unwinnable fee race whose loser is the sub-coin's receiver, who
cannot detect the exposure at accept time. ⟹ V2 split-transfer must refuse to ladder a sub-coin whose parent
ladder was ever conveyed, or give the trigger a CSV. Also: tiers are v3/TRUC and the split tx is v2, so a v3
trigger cannot relay while the split tx is unconfirmed — **F must confirm, not merely be broadcast**.

## Split-child-bundle design — FLAWED (all 3 review lenses); 7 FATALs, several LIVE beyond splits
The in-ladder split's child bundle spans two aggregates (ancestors under `A_parent`, child tiers under
`A_child`). Designing + reviewing it exposed that the split's safety depends on server/enclave guarantees
that **do not currently hold**. Verdict: **FLAWED — not implementable until these land.**

1. **S1 (FIXED 9d63f15) — `sign/second` was ungated.** Every fork gate (`single_use`, `sig_budget`,
   `epoch_deadline`) lived only in `sign/first`; `sign/second` re-checked none. A durable null-challenge
   session opened before terminalization could be completed after ⟹ a 2nd co-sign of a terminal node
   (INV-19 fork). **Live on the V1 single_use lane, not just V2.** Fixed by replicating the gate block in
   `sign/second`, fail-closed. Needs a **server rebuild + redeploy**.
2. **The enclave has NO notion of terminality.** `grep sig_budget|single_use|terminal` over `lockbox/src`
   + `enclave/App` = ZERO hits. The lockbox's only per-node state is `generated_public_key.sig_count`, a
   monotone counter with no policy — it will co-sign any tier forever when asked (`server.cpp:126-141`).
   Every terminal guarantee lives in the COORDINATOR's Postgres ⟹ "the blind SE cannot fork an off-chain
   tree" is only as strong as the coordinator. Architectural; recorded in the trust model. Not client-fixable.
3. **The census counts partial-sig ISSUANCES, not tiers.** `update_sig_count` fires inside
   `get_partial_signature` (`lockbox/src/server.cpp:135`) BEFORE the sig is returned, while the finality
   marker `set_partial_sig_issued` lands later (`sign.rs:300`). Any retried signing round (lost reply, 502,
   DB error) permanently OVER-counts ⟹ the full-disclosure census never balances ⟹ the coin BRICKS. A
   **benign** failure mode, not only adversarial — it defeats my full-disclosure counting on flaky networks.
4. **Terminalize-then-verify ordering bricks the parent.** A split terminalizes the parent (step 3) but
   only self-verifies its census (step 5). `set_sig_budget` is monotonic; if step 5 finds the census
   doesn't balance (per #3, for a benign reason), the parent is ALREADY irreversibly terminal — cannot
   renew, re-split, or recover.
5. **B-11: child `backups == []` contradicts the receiver bootstrap.** `get_tx0_outpoint` does
   `backup_transactions.first().ok_or(NoBackupTransactionFound)` — with no backups the receiver errors
   BEFORE `verify_bundle` is ever reached, and loses the tx0/auth-sig bootstrap.
6. **§2.2(4) sid↔key binding is rogue-key-forgeable.** `create_aggregated_address` is plain EC addition
   (`deposit/mod.rs:157`) with no keyagg coeffs / PoP, and `/info/statechain/<sid>` never returns the
   registered owner pubkey. So `owner_pubkey := P_target − E(sid)` is a free forgery ⟹ the ancestor census
   runs against an attacker-tuned decoy counter.
7. **Cross-segment census laundering.** `superseded_*`/`backups` live INSIDE a segment struct, so which
   counter an artifact debits is decided by which field the sender fills — while the verification key is
   prevout-derived. The two are never required to agree; per-segment dedup doesn't fire.

## Verdict / order of work
1. **DONE** — default reverted to V1 (exposure closed); B0, S1(verify_bundle), S2, S-1, S-2 fixed +
   attack-proven (sdk54/sdk55); B1 gated (HF-1/3/4/5); `sign/second` gates fixed (9d63f15, needs redeploy).
2. **BLOCKING for V2-as-default (server/enclave workstream, NOT a client patch):**
   - a census that counts TIERS, not partial-sig issuances (fix the `update_sig_count` vs
     `set_partial_sig_issued` ordering, or make the client census tolerant of issuance-count drift);
   - terminality the enclave can ENFORCE (or an explicit trust-model statement that it is coordinator-only);
   - the split-child-bundle redesign per findings 4–7 (bind sid↔key authoritatively; child bootstrap with
     backups; segment-scoped census).
3. **THE HONEST STATE:** the in-ladder split cannot be made sound purely client-side. B1 (relocated to the
   parent-state rival) is only closed by a census that rests on #2. Until #2 lands, V2 stays opt-in and
   splits of laddered coins stay refused (HF-1). Do NOT flip the default.
4. Follow-on: adaptor-sig LN (V2-LATCH-FIX.md) for the LN lane.

---

## O-1 RESOLUTION (2026-07-21) — V2 CAN be made sound; the count census is the right mechanism

The O-1 counter-machine review (the missing TESR-3 foundation) settled the central question:

> **A blind SE CAN support a receiver-verifiable "no hidden state" property — by COUNTING signing rounds,
> not by seeing messages. Counting is orthogonal to blindness.**

- A **label** census (`{level,m,k}`, as V2-DESIGN:208 specified) is genuinely impossible — a blind SE
  cannot verify a declared coordinate matches the signed message (Theorem 1). That machine is dead; do NOT
  build it.
- The shipped `verify_bundle` uses a **COUNT** census (`num_sigs == v1_backups + tiers + superseded`),
  which is sound: the SE observes each signing round regardless of blindness. The retry-brick is **a
  missing cache, not an information barrier**. The CTL/semi-blind redesign was REJECTED (it would trade
  away blindness for a guarantee obtainable free).

### Fixes landed + VERIFIED on the live stack (regtest + lockbox)
- **S3 rollover equal-CSV twin** + **verify_bundle transitive-death** (a8e05a7): rollover now decrements
  the self-split CSV (mirrors presign); the orphan check now accepts transitively-dead superseded tiers
  (renewed coins) while rejecting real orphans/threats via a non-confirmability fixpoint. sdk43 now runs
  verify_bundle (it never did); sdk54 (incl. new **attack G**, 88e2a50) + sdk55/46/48 reject every attack;
  sdk44/47/49/50 green.

### Fixes landed, compile-clean, awaiting a coordinated redeploy / SGX build to ACTIVATE
- **SGX secnonce single-use** (9cfe48f) — P0 key-extraction on the production lane; needs Linux+SGX build
  + the reuse-over-two-messages attack test. See docs / memory `sgx-lane-untested-gap`.
- **sign/second fork gates** (9d63f15), **get_statechain_info NULL-challenge panic** (42a39fe),
  **sign/first fail-open lock → fail-closed** (549d9d2) — all server-side; activate on redeploy.
- **S7 nodejs/web fail-closed on V2 coins** (ca76dfc).

### BLOCKED (not a client patch; needs infra or protocol design)
- **Keystone — retry-safety response cache** (the last thing gating V2-as-default): lockbox caches the
  partial sig keyed on the challenge (return cached on retry, no re-sign / no increment; nonce-reuse guard
  preserved); coordinator caches likewise; client persists the session and retries `sign/second` — never
  restarts `sign/first`. This makes the count census retry-safe (a lost response no longer bricks a coin).
  BLOCKED because the lockbox is C++ that cannot be built on the dev host (heavy vcpkg/google-cloud-cpp);
  needs a CI/Linux build to implement+verify. The client-retry half has only marginal value without the
  cache and is deferred to land together.
- **S5 — presign abandonment burns/bricks ladder rungs**: presign co-signs S' on a CLONE (num_sigs++)
  without updating the sender's persisted bundle, so an abandoned/failed transfer leaves num_sigs above
  what any future bundle discloses ⟹ every future receiver rejects (count mismatch) ⟹ brick; a malicious
  receiver can trigger it in one round-trip. The sound fix is a receiver-liveness gate (don't co-sign for a
  no-show) or a reclaim operation (re-sign the owner state one rung lower, disclosing the abandoned S' as
  superseded) — a protocol feature coupled to the keystone's idempotent presign, NOT to be improvised.
- **S1 — lockbox port 18080 published + unauthenticated** (confirmed live: `0.0.0.0:18080`): anyone
  reaching it calls sign/first+sign/second directly, bypassing every coordinator gate and voiding the
  census. Fix = authenticate the lockbox↔coordinator channel (shared secret/mTLS; lockbox rebuild) +
  unpublish the host port on co-located topologies. BLOCKED on lockbox rebuild + a deploy/topology call.

**Do NOT flip the default to V2 until the keystone lands and is verified.** With it, V2's count census is
sound and retry-safe, and the in-ladder split (B1) rides on top of a trustworthy parent census.

---

## KEYSTONE DONE (2026-07-22) + the V2-completion gate chain

The keystone LANDED and is VERIFIED (see the memory + commits 11edbae/5b5b698/ea09c01): the lockbox caches
the partial sig keyed on the session and increments sig_count atomically; the client retries the same
sign/second. sdk56 proves the signing round is idempotent under retry (num_sigs counted once across 3
replays). Server + lockbox redeployed from latest source; full V2 suite green.

**To reach "V2 default + V1 deleted" the remaining gates are, in order:**

1. **S5 (presign-abandonment brick) — NEEDS A SERVER-SIDE PROTOCOL CHANGE (design ruling, workflow
   wab9jyo5b).** A purely client-side fix is IMPOSSIBLE — three independent FATALs, S-1 dispositive:
   the V2 transfer path co-signs a RECEIVER-PAYING V1 BACKUP on every attempt (transfer_sender.rs:278 ->
   create_backup_tx_to_receiver), whose locktime is IDENTICAL across abandons; splicing journaled backups
   into the conveyed list hits INV-5 (ladder_decrements_by_interval, receiver.rs:274) — which cannot loosen
   without reopening the S2 hidden-state defense — and NOT splicing leaves v1_backups short => verify_bundle
   num_sigs mismatch. Both horns BRICK. FIX = **receiver-liveness gate**: add a receiver-signed commitment
   to the transfer protocol; the SE co-signs BOTH the V1 backup AND S' ONLY against that commitment, so a
   no-show strands nothing (this is the work already flagged at transfer_sender.rs:257). Latent note
   (limitation, independent of S5): the receiver-paying V1 backup a V2 coin co-signs descends from tx0's
   output, which T also spends — a latent on-chain rival of T on EVERY successful V2 transfer; deserves its
   own verification pass.
2. **In-ladder split (B1)** — enables V2 non-exact payments (split_coin refuses laddered coins today,
   transfer.rs:426). Redesign in progress (workflow w7s7jxly2) now that the parent census is trustworthy.
3. **Adaptor-sig LN = "finish lightning swaps"** — LN rides the V1 batch_id latch (lightning.rs); deleting
   V1 breaks it. Needs adaptor signatures (server/protocol).
4. Then: flip deposit_protocol_default -> 2; delete V1 (verifier lane, validate_signature_scheme,
   v1_backups, batch_id latch, migrate tests); rewrite all docs/utexo/*.

**Every remaining gate is server/SE/protocol work, not a client patch** — V2's safety rests on the blind
SE (counting + gating). The build/deploy cycle is proven (incremental Docker rebuild + docker cp + restart),
so it is feasible; it is a real backend program, done gate by gate with adversarial review + live-stack
verification. Do NOT flip default or delete V1 until gates 1-3 land + are verified.
