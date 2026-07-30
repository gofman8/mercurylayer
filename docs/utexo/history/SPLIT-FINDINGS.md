# Split-transfer design — FLAWED verdict + FATAL holes found in the LIVE laddered lane
### [HISTORICAL — the verdict was acted on; the split SHIPPED. Read the STATUS block first.]

> ## STATUS (2026-07-29) — HISTORICAL RECORD. Every finding below is CLOSED or superseded.
>
> This file is the audit trail of the split-transfer review — the B1 theft vector, the S1/S2 census
> holes, the seven split-child-bundle FATALs — as they were found. It is kept for its reasoning, not as
> a description of the live system. What shipped:
>
> - **There is ONE protocol.** `deposit_protocol_version` and the `UTEXO_PROTOCOL_DEFAULT` env are
>   DELETED; `claim()` establishes a TES-R ladder for every fresh confirmed ROOT coin, unconditionally.
>   The "flip the default / delete the pre-TES-R lane" gate chain at the bottom of this file is DONE.
>   Nothing is opt-in; zero tests pin the deleted lane.
> - **Read the two "lanes" below as HISTORY, not as two live things.** What this file calls the laddered
>   lane is simply *the* protocol (TES-R). The pre-TES-R design — every coin anchored to a decrementing
>   absolute-locktime backup chain from its deposit height — survives only as a coin SHAPE, the UN-LADDERED
>   shape, and that shape is LOAD-BEARING, not legacy: an **RGB carrier** is deliberately never laddered
>   (a plain tier spend would
>   destroy the allocation — terminal-freeze, PROTOCOL.md §5.10; sdk52), and a **split sub-coin whose
>   funding is un-broadcast cannot root a trigger** [B0, below]. Those coins keep the signed-once backup
>   and transfer by backup-chain handover.
> - **B1 is CLOSED** by the in-ladder split — sdk58 (control ACCEPT + **11** adversarial REJECTs) and
>   sdk59 (end-to-end non-exact payment over the SDK). Its *relocated* form (the parent's own retained
>   state, see the CORRECTION below) is closed by the parent's ENFORCED terminality + the full-disclosure
>   census, made retry-safe by the keystone (sdk56).
> - **Received split children are FIRST-CLASS** (`docs/utexo/CHILDREN.md`), which SUPERSEDES
>   this file's "exit-only claim" model: the claim completes the standard SE key handover — `A_child` is
>   INVARIANT across the rotation, which is what keeps the pre-signed exit chain valid, and the sender is
>   permanently locked out — and a child pays onward off-chain WHOLE (`child_retransfer`) or SPLIT
>   (`child_in_ladder_pay`, a depth-2 `ancestors` chain). Each hop costs exactly one co-signature and
>   discloses exactly one superseded state, which the receiver's census counts and proves out-raced.
>   sdk60 (alice→bob→carol, funding outpoint unspent throughout) + sdk17 (partial second hop).
> - **Lightning rides the ladder in BOTH directions** via the HODL latch (`LIGHTNING.md`; sdk63/sdk64/
>   sdk67, sdk65 non-exact, sdk66/sdk68 failure paths) — *not* the adaptor signatures gate 4 of this file
>   assumed. The LN-latched piece is the ONE case that stays terminalized (it sits unclaimed past the
>   pending-transfer lock's window).
>
> Per-finding status is inline below. Three items still carry something, all flagged in place: **S5**
> (a recoverable re-transfer brick, never fund loss), the **lockbox port 18080** deployment item, and the
> **SGX lane** — every test here runs against the dev lockbox, so the production `enclave/App` signing lane
> is still unexercised (`sgx-lane-untested-gap`).

## 🔴 B1 — LIVE THEFT VECTOR (was shipped in the laddered default at 434d334; default REVERTED)

**Not a future design constraint — an exploitable theft in the shipped default. No collusion, one SDK call.**

| Fact | Evidence |
|---|---|
| `T` has NO timelock, spends `F`, fully co-signed at `establish` | `lib/src/tesr.rs:145` (`TRIGGER_SEQUENCE=0xFFFF_FFFD`), `:159` `lock_time:0`, asserted `:352-353`; co-signed `clients/libs/rust/src/tesr.rs:130` |
| The split tx spends the SAME `F` | `lib/src/transaction.rs:402-412` builds its input from `coin.utxo_txid/vout`; `clients/libs/rust/src/tesr.rs:125-126` sets `f_txid/f_vout` from the same fields |
| `split_coin` had NO ladder check | `clients/libs/rust-sdk/src/transfer.rs:387-406` — guards were CONFIRMED / !dup / !carrier only |
| Every transfer of a laddered coin leaves the SENDER a retained `T` | `presign_receiver_state` clones and never deletes (`tesr.rs:299,302`); there is NO deletion path for `tesr-*` |
| The SDK performs the attack | `unilateral_exit(Some(ids))` filters the explicit-id branch on carrier status only (`wallet.rs:1016-1028`), then `exit_pass` broadcasts `T` with no `outpoint_spent` gate (`tesr.rs:389-406`) |

**Attack:** Alice receives a laddered coin → pays Bob a non-exact amount (⟹ `split_coin`; Bob gets an
**un-laddered** sub-coin, `transfer_sender.rs:312`, funded by the un-broadcast split tx) → Alice `unilateral_exit`s the
PARENT. `T` confirms, `F` is consumed, **Bob's split tx is permanently dead**, Alice's ladder pays her the
full parent value.

**The race is rigged:** `T` is v3/TRUC + P2A — fee-bumpable by anyone, forever. The split tx is v2 with a
frozen fee and no RBF headroom (the parent's SE budget is set to exactly 1 and the split consumes it,
`transfer.rs:458-464`). Alice wins deterministically.

**Bob's due diligence is meaningless:** his `terminal_parents` check (`transfer_sender.rs:290-294`) returns
true. He cannot see `T` — a ladder is never conveyed with an un-laddered coin and the SE has never seen it
(`tesr.rs:47`).

**The code's load-bearing claim is FALSE for a laddered coin** (`transfer.rs:455-457`: *"No later withdraw/transfer/
backup of the parent can be signed — the branch cannot be double-spent even by a malicious sender"*). It
rests on the un-laddered premise that every spend of `F` needs a FRESH SE co-sign. A ladder breaks it at the
root: `T` was co-signed at `establish`, long before `set_spend_budget`. **A budget bounds future co-signs; it
cannot retract an issued signature.** Strictly WORSE than the un-laddered shape, where the parent's backup is
locktimed above the branch and the branch always matures first (INV-4).

### Status
- ~~**Deposits STILL un-laddered by default** (laddering opt-in via `UTEXO_PROTOCOL_DEFAULT=2`) — exposure closed.~~
  **SUPERSEDED (2026-07-29): there is one protocol.** Every fresh confirmed root coin is laddered by
  `claim()`; the `UTEXO_PROTOCOL_DEFAULT` / `deposit_protocol_version` escape hatch is deleted. The
  exposure is closed by the in-ladder split *itself*, not by a default — `transfer()` never routes a
  laddered coin into the naive `split_coin`.
- **HF-1 landed**: `split_coin` refuses a laddered coin (the direct API); `transfer()` no longer hits it
  — a laddered non-exact payment now routes to the in-ladder split (below).
- **✅ IN-LADDER SPLIT LANDED + PROVEN (commit 019637b, 6fd77cf)** — the real B1 fix (see next section).
  `transfer()` on a laddered coin runs `in_ladder_pay`: `SP` is a STATE tier spending `X_m.out[0]`
  (a descendant of `T`, never a rival for `F`), the PIECE child pays the recipient (Model A) and is
  conveyed as a `ChildTesrBundle` to their mailbox, the CHANGE child pays self. The receiver's `claim()`
  adopts it via `verify_child_bundle` — the 8-check Stage-2 predicate that binds `A_parent` to the
  on-chain `F`, checks the parent+child **exact-equality censuses**, and requires BOTH sids **terminal**
  (queried from `/statechain/spend_budget`, fail-closed) so no rival state can still be co-signed. Proven:
  **sdk58** (control ACCEPT + 9 adversarial REJECTs incl. non-terminality) and **sdk59** (Alice pays Bob
  a non-exact amount over the SDK, Bob adopts + unilaterally exits, funds land at Bob's key).
  - **REFINED since (CHILDREN.md):** the **child**-terminality half was DROPPED — freezing the
    child is what made it exit-only. The **parent**'s terminality is still REQUIRED and fail-closed
    (`verify_child_bundle` [F2]); the child's census is instead made durable by the receiver COMPLETING
    the key handover in the same claim (permanent lockout) with the coordinator's pending-transfer lock
    covering the census→completion gap. The verifier still TOLERATES a terminal child — the LN-latched
    lane deliberately keeps one. sdk58 now runs **11** adversarial REJECTs against that predicate.
- **HF-2 mooted**: the planner no longer needs a `splittable` hint — `transfer()` splits laddered coins
  in-ladder instead of hard-failing.
- ~~**Follow-ups still open before flipping the default**~~ — **ALL CLOSED; the default no longer exists
  (one protocol). Statuses inline:**
  - > 🔁 **SUPERSEDED (2026-07-23 → shipped) — the exit-only model below is NOT what ships.** Received
    > children are FIRST-CLASS (`docs/utexo/CHILDREN.md`): the conveyance carries the standard
    > key-handover material, the receiver completes `/transfer/receiver` (SE rotates its share, `A_child`
    > INVARIANT so the pre-signed exit chain stays valid, sender's auth rotated out ⟹ permanently locked
    > out), and the child is then re-spendable OFF-CHAIN — whole (`child_retransfer`) or split
    > (`child_in_ladder_pay`). The reasoning below is kept because its *rejection* of the reopen still
    > stands, and because it names the exact gap the pending-transfer lock had to close.
    > Proven by **sdk60** (alice→bob→carol off-chain, funding outpoint unspent throughout) and **sdk17**
    > (partial second hop). What remains true from the old model: a child's on-chain **withdraw** IS its
    > unilateral exit (multi-block by CSV) — and [D4] that path used to mark the child `WITHDRAWING`
    > though no withdrawal tx exists, so status polling errored forever; fixed during the migration.
  - **Received-child first-classness — SOUND MODEL = exit-only claim** (adversarial review 2026-07-22,
    workflow wf_ead9b877-719). A received in-ladder split child is an EXIT-ONLY claim: funding =
    un-broadcast `SP.out[j]`, and the receiver holds no SE co-signing key — only the pre-signed exit chain
    that (Model A) pays the receiver's own key. **Co-op WITHDRAW of a child = its unilateral exit** (the
    exit IS the withdrawal; `withdraw()` routes a child to `unilateral_exit` → `exit_child_pass`,
    multi-block by CSV). `transfer()` **excludes** child claims from spend selection — off-chain
    re-transfer of an un-materialized child is not supported; materialize (broadcast SP) first, then it is
    a normal on-chain-funded coin.
    - ❌ **The "SE handover + budget-reopen" idea to make a child first-class off-chain is UNSOUND — do
      NOT build it.** `arm_child_reopen(sid, new_auth)` with a sender-supplied target lets a malicious
      sender arm the reopen to its OWN key, self-complete the handover, reopen the budget for itself, and
      co-sign a no-CSV key-spend of `SP.out[j]` that beats the receiver's CSV-delayed exit — **B1
      re-armed, theft**. Even binding the marker to the real recipient, a child's headless off-chain
      funding has no on-chain root, so a second-hop re-transfer cannot be census-verified downstream.
      - **This rejection STANDS — the reopen was never built.** First-classness came from the *normal*
        transfer instead: the SE key HANDOVER (permanent, monotone — no budget is ever loosened, so no
        INV-19 fight) plus a **temporary pending-transfer lock** in the coordinator that refuses the
        still-owner's co-signs while a transfer of that sid is open. Both halves of the objection are
        answered: the sender cannot self-complete (the lock also rejects re-addressing an open pending
        row), and a child's headless funding IS census-verifiable downstream because the conveyance
        carries the whole `ancestors` chain back to the on-chain `F` (each segment terminal + its own
        exact-equality census). sdk60/sdk17.
    - ~~⟹ Laddering by default makes received non-exact payments exit-only claims (re-spend = materialize) —
      a **product decision** (sdk01's instant co-op-withdraw assumption becomes a multi-block chain op),
      not just an engineering gate.~~ **MOOT: received non-exact payments are first-class, so re-spend is
      an off-chain hop, not a materialization.** Only an on-chain *withdraw* of a child is still the
      multi-block unilateral exit; sdk01's plain co-op withdraw is untouched.
  - **Census baseline generalization — PARTIAL (multi-hop done; root assumption remains, fails CLOSED).**
    The multi-hop path no longer depends on one baseline: each conveyed segment carries its own
    (`CHILD_V2_BASELINE = 0` for a never-on-chain-funded child slot, `PARENT_V2_BASELINE = 1` for the
    on-chain-rooted parent), which is what lets sdk17 split an already-RECEIVED child. Still hardcoded at
    `clients/libs/rust/src/tesr.rs:437`: the ROOT segment is assumed to be a **fresh deposit** (one
    signed-once backup before establish), so splitting a root parent with a longer backup history still
    fails the census — fail-CLOSED (safe, functionally blocked), and no live test exercises it.
  - HF-3 (same gate in `mint_piece`), HF-4 (landed: `unilateral_exit` filters `CoinStatus`), HF-5 (landed:
    the false claim quoted above is gone from `transfer.rs`; the surviving comment at the `split_coin`
    guard states the B1 reasoning instead).

### THE FIX — in-ladder split (PROTOCOL.md §5.4), not a gate
The gate is a feature amputation: `presign_receiver_state` runs on EVERY transfer of a laddered coin, so
essentially every circulating laddered coin is conveyed-ladder ⟹ laddered coins could not pay non-exact
amounts (violating REQ-2). Its escape hatch (`reanchor`, 112 vB + a confirmation per non-exact payment)
negates TES-R's "0 vB rent / ~320× footprint win" — and may not even work (does `withdraw` on a conveyed
laddered coin have SE sign-budget headroom? external review F2 / migration 0008 — MUST be checked).

> **Outcome:** the amputation never shipped and the open question is MOOT as an escape hatch — a non-exact
> payment routes in-ladder (sdk59), never through a re-anchor. Re-anchoring survives as the separate
> `refresh()` feature (sdk30), where the same fee window bit twice during the migration: [D1] the in-ladder
> split's admission guard used the old signed-once backup floor (442 sat) even though a child funds its OWN
> two tiers + dust, so a payment admitted below `min_child_value` (1306 sat at 2 sat/vB) terminalized the
> parent and THEN failed, stranding it; and [D2] `refresh_sponsored` sized its rebate into that same dead
> window, so a sponsored refresh failed *after* the user had paid the on-chain fee. Both fixed.

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

**The relocated B1 is now CLOSED.** The three things the correction demanded all landed: the parent IS
terminalized before the child is conveyed; that terminality is ENFORCED at co-sign time (the budget clamp
in `sign/first` AND `sign/second`, plus the pending-transfer lock) and re-read by the receiver from
`/statechain/spend_budget` fail-closed; and the parent's full-disclosure census is trustworthy because the
keystone made a signing round idempotent under retry (sdk56), so the count can no longer drift benignly.
sdk58 rejects a non-terminal parent among its 11 cases.

---

## S1 — FATAL (fund-loss): `verify_bundle`'s exact-count linchpin is paddable — **FIXED**
> **FIXED, all five required items including the hard one (4).** `verify_bundle` now parses every
> superseded entry, demands a CSV unconditionally, links each tier to a prevout **inside this ladder**
> (per-OUTPUT, so a split state can legitimately host a child on each `out[j]`), values it from the PARSED
> transaction rather than the attacker-supplied `out_value`, and **signature-verifies every counted tier**
> against `A` (`verify_tier_cosigned`) — exit tiers included, closing the "same class" note in item 4. The
> race check is per-prevout (S-1/S-2) and covers superseded EXTENSIONS as well as states.
> Item 5's cases are proven at a higher bar than the unit tests asked for: **sdk54** runs them against a
> REAL ladder co-signed by the live SE — A (empty `signed_tx` + `csv:None`), B (real tier replayed with
> `csv:None`), C (well-formed but never-co-signed), D/E (superseded state / **extension** out-racing the
> live tier), F (orphan over an uncontended outpoint), G (superseded state with no disclosed dead parent) —
> each must REJECT. Plus sdk55; the `verify_bundle` unit tests in `tesr.rs` cover accept / hidden-extra-sig
> / undercount / broken prevout link / final-state-not-paying-owner; sdk43/44/46/47/48/49/50 green.

`clients/libs/rust/src/tesr.rs:508-529`.
```
expected = flat_backups + tiers.len() + superseded_states.len() + superseded_extensions.len()
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

## S2 — FATAL (fund-loss): `flat_backups` is attacker-supplied and unvalidated on the laddered path — **FIXED**
> **FIXED — the backup vector is now structurally validated on BOTH coin shapes.** A laddered coin runs
> `validate_backup_chain_v2` (structural chain validation that keeps INV-5, `ladder_decrements_by_interval`,
> against both the duplicate-padding and the locktime-inversion attack) instead of skipping the check; the
> un-laddered shape keeps `validate_signature_scheme` (its `tx_n` still aligns with the SE's per-co-sign
> index, which a ladder's tiers break). The count term feeding `verify_bundle` is therefore no longer
> free-form. `clients/libs/rust/src/transfer_receiver.rs` [S2] block; sdk55.

`clients/libs/rust/src/transfer_receiver.rs:600-604` passes `transfer_msg.backup_transactions.len()`
straight into `expected`; `:635` gates `validate_signature_scheme` on `protocol_version < 2`, so **nothing
inspects the vector's interior once the coin is laddered** (only `get_tx0_outpoint` first-by-`tx_n` and
`verify_latest_backup_tx_pays_to_user_pubkey` `.last()`).
- **Count padding**: duplicate `tx1` → `[tx1, tx1, tx2]`. Same prevout ⟹ one group; first/last unchanged.
  `expected` inflates to match a hidden-state `num_sigs`. Middle entries need not be valid txs. Unbounded.
- **Locktime inversion**: build the receiver-paying backup at `L+interval` (not `L−interval`) and retain
  one's own at `L`. INV-5 (`ladder_decrements_by_interval`) is the only enforcement — and it is skipped.

**Strict regression against the pre-TES-R design**: the un-laddered path rejects both (`:615` +
`validate_signature_scheme`). Root cause (mine): V2DEF-1 gated that structural check off for laddered coins
without replacing it.

**Required fix.** Validate the backup vector structurally on the laddered path too (count, per-group uniqueness,
decrementing-locktime), or derive `flat_backups` from an authoritative source rather than the sender's vector.

## B0 — FIXED (commit 5c42667): root-only laddering
`claim()`'s establish loop filtered only `CONFIRMED/!dup/!single_use/!carrier` — no root check — so a split
**sub-coin** got laddered once deposits were laddered by default; its `F` is an un-broadcast split output ⟹ `exit_pass`
broadcasts a trigger with no prevout ⟹ silent stall, `wait_blocks:0` forever ⟹ unexitable via the SDK.
Fixed by requiring F on-chain (electrum `transaction_get`), fail-closed. (An earlier `branch-<id>` proxy
broke root laddering because `get_backup_txs` does `fetch_one(..)?` — a missing row returns `Err`, conflating
"root" with "db error"; sdk48 caught it.)

> **Standing rule, not a leftover:** this is one of the two reasons the UN-LADDERED coin shape is
> permanent and load-bearing. `claim()` ladders every fresh confirmed ROOT coin, unconditionally — and
> deliberately does NOT ladder (a) a split sub-coin whose funding is un-broadcast [this finding], or (b) an
> RGB carrier, where a plain tier spend would destroy the allocation (terminal-freeze, PROTOCOL.md §5.10;
> sdk52 proves a carrier is never laddered). Those coins keep the signed-once backup and transfer by
> backup-chain handover. Do not "finish the migration" by laddering them.

## B1 — structural constraint on laddered split-transfer (~~not yet addressed~~ **ADDRESSED**)
> **ADDRESSED by the in-ladder split, and the constraint's second half is now a standing design rule.**
> A laddered coin is never split by a tx that rivals `F`: `SP` is a STATE tier spending `X_m.out[0]`, a
> DESCENDANT of the trigger. The "give the trigger a CSV" alternative was not needed. The relay/confirmation
> point below survives verbatim as [B0]: a sub-coin whose funding is un-broadcast cannot root a trigger, so
> it stays un-laddered.
The conveyed trigger has **no timelock** (`TRIGGER_SEQUENCE = 0xFFFF_FFFD`, `lib/src/tesr.rs:145`), so every
prior owner of an adopted laddered coin holds a free, immediate spend of `F`. A branch-conveyed sub-coin's root hop
is the split tx, which **also** spends `F` ⟹ unwinnable fee race whose loser is the sub-coin's receiver, who
cannot detect the exposure at accept time. ⟹ laddered split-transfer must refuse to ladder a sub-coin whose parent
ladder was ever conveyed, or give the trigger a CSV. Also: tiers are v3/TRUC and the split tx is v2, so a v3
trigger cannot relay while the split tx is unconfirmed — **F must confirm, not merely be broadcast**.

## Split-child-bundle design — FLAWED (all 3 review lenses); 7 FATALs, several LIVE beyond splits
The in-ladder split's child bundle spans two aggregates (ancestors under `A_parent`, child tiers under
`A_child`). Designing + reviewing it exposed that the split's safety depends on server/enclave guarantees
that **do not currently hold**. Verdict: **FLAWED — not implementable until these land.**

> **Verdict UPDATED: the guarantees landed and the design SHIPPED.** 6 of the 7 are fixed (statuses per
> item below); #2 stands as a permanent architectural statement, recorded in `TRUST-MODEL.md`, not as a
> blocker. The shipped bundle also grew a third shape the review did not anticipate — an `ancestors` chain
> of N segments, each with its own terminality + census — which is what makes a received child re-splittable
> (sdk17) rather than a leaf.

1. **S1 (FIXED 9d63f15) — `sign/second` was ungated.** Every fork gate (`single_use`, `sig_budget`,
   `epoch_deadline`) lived only in `sign/first`; `sign/second` re-checked none. A durable null-challenge
   session opened before terminalization could be completed after ⟹ a 2nd co-sign of a terminal node
   (INV-19 fork). **Live on the un-laddered `single_use` lane, not just the laddered one.** Fixed by replicating the gate block in
   `sign/second`, fail-closed. Needs a **server rebuild + redeploy**. — **REDEPLOYED and now load-bearing:**
   `sign_first`/`sign_second` also carry the pending-transfer lock (19e6668), which is what lets a conveyed
   child be handed over without being frozen.
2. **[STANDS — architectural, by design] The enclave has NO notion of terminality.** `grep sig_budget|single_use|terminal` over `lockbox/src`
   + `enclave/App` = ZERO hits. The lockbox's only per-node state is `generated_public_key.sig_count`, a
   monotone counter with no policy — it will co-sign any tier forever when asked (`server.cpp:126-141`).
   Every terminal guarantee lives in the COORDINATOR's Postgres ⟹ "the blind SE cannot fork an off-chain
   tree" is only as strong as the coordinator. Architectural; recorded in the trust model. Not client-fixable.
   **Unchanged and accepted:** the enclave supplies the COUNT (its `sig_count` is the census's authoritative
   input), the coordinator's database supplies the POLICY. `TRUST-MODEL.md` defines "SE" as API server +
   database + enclave(s) and records the terminality budget as a database-side monotonic guard (B10, on
   `database/deposit.rs`). Do not read any doc on this page as claiming enclave-enforced terminality.
3. **[FIXED — keystone] The census counts partial-sig ISSUANCES, not tiers.** `update_sig_count` fires inside
   `get_partial_signature` (`lockbox/src/server.cpp:135`) BEFORE the sig is returned, while the finality
   marker `set_partial_sig_issued` lands later (`sign.rs:300`). Any retried signing round (lost reply, 502,
   DB error) permanently OVER-counts ⟹ the full-disclosure census never balances ⟹ the coin BRICKS. A
   **benign** failure mode, not only adversarial — it defeats my full-disclosure counting on flaky networks.
   **FIXED by the keystone (11edbae/5b5b698/ea09c01):** the lockbox caches the partial signature by session
   and increments `sig_count` atomically, and the client retries the SAME `sign/second` instead of
   restarting the round. sdk56 replays a round 3× and the census counts it ONCE.
4. **[FIXED] Terminalize-then-verify ordering bricks the parent.** A split terminalizes the parent (step 3) but
   only self-verifies its census (step 5). `set_sig_budget` is monotonic; if step 5 finds the census
   doesn't balance (per #3, for a benign reason), the parent is ALREADY irreversibly terminal — cannot
   renew, re-split, or recover.
   **This exact failure fired in production shape during the migration — [D1].** The in-ladder split's
   admission guard used the old signed-once backup floor (442 sat), but a child funds its OWN two tiers +
   dust, so a payment below `min_child_value` (1306 sat at 2 sat/vB) was ADMITTED, terminalized the parent,
   and only THEN failed — stranding the parent exactly as this finding predicted. Fixed by admitting against
   `min_child_value` (`mercurylib::tesr::min_child_value`, checked in `transfer.rs` before any
   terminalization), with the benign-drift half removed by #3.
5. **[FIXED] B-11: child `backups == []` contradicts the receiver bootstrap.** `get_tx0_outpoint` does
   `backup_transactions.first().ok_or(NoBackupTransactionFound)` — with no backups the receiver errors
   BEFORE `verify_bundle` is ever reached, and loses the tx0/auth-sig bootstrap.
   **Fixed by routing the child around that bootstrap entirely:** a `child_tesr_bundle` message takes its own
   branch in `transfer_receiver.rs` — the funding outpoint is `SP.out[j]` (from the conveyed segment, not
   from a backup vector), validation is `verify_conveyed_child`, and the auth/tx0 bootstrap is replaced by
   the conveyed handover material (`t1`/transfer signature) the receiver completes at `/transfer/receiver`.
   The signed-once backup-chain checks are skipped by construction, not by an exception.
6. **[FIXED] §2.2(4) sid↔key binding is rogue-key-forgeable.** `create_aggregated_address` is plain EC addition
   (`deposit/mod.rs:157`) with no keyagg coeffs / PoP, and `/info/statechain/<sid>` never returns the
   registered owner pubkey. So `owner_pubkey := P_target − E(sid)` is a free forgery ⟹ the ancestor census
   runs against an attacker-tuned decoy counter.
   **Fixed twice over, so no sid claim is ever trusted:** (a) the server now records an AUTHORITATIVE,
   unique aggregate per statechain_id (owner_share + enclave_share; migration 0009) which `/info/statechain`
   returns — proven live by **sdk57**; and (b) `verify_child_bundle` check [1] does not even rely on that,
   taking `A_parent` from the **on-chain `F.spk`** fetched from Electrum (BIP-341-tweaked before comparison)
   and binding the segment's declared aggregate to it. A sender-picked decoy `F` or a tuned counter fails
   the bind; sdk58 rejects the aggregate-tampering cases.
7. **[FIXED] Cross-segment census laundering.** `superseded_*`/`backups` live INSIDE a segment struct, so which
   counter an artifact debits is decided by which field the sender fills — while the verification key is
   prevout-derived. The two are never required to agree; per-segment dedup doesn't fire.
   **Fixed by making every segment carry its own census, verified against its own SE-fetched facts:** the
   parent segment via `verify_bundle_ex(..., PARENT_V2_BASELINE)`, each `ancestors[i]` against its own
   `AncestorFacts { num_sigs, aggregate_pubkey, terminal }`, and the leaf against
   `child_num_sigs == child_flat_backups + 2 + child_superseded_ok`. An artifact moved between segments no
   longer balances any of them.

## Verdict / order of work
1. **DONE** — default reverted to un-laddered deposits (exposure closed); B0, S1(verify_bundle), S2, S-1, S-2 fixed +
   attack-proven (sdk54/sdk55); B1 gated (HF-1/3/4/5); `sign/second` gates fixed (9d63f15, needs redeploy).
2. **BLOCKING for laddering-by-default (server/enclave workstream, NOT a client patch):** — **ALL LANDED.**
   - a census that counts TIERS, not partial-sig issuances (fix the `update_sig_count` vs
     `set_partial_sig_issued` ordering, or make the client census tolerant of issuance-count drift);
     — **done: keystone session cache, sdk56.**
   - terminality the enclave can ENFORCE (or an explicit trust-model statement that it is coordinator-only);
     — **resolved the second way: the statement is in `TRUST-MODEL.md`; the coordinator enforces, the
     enclave counts.**
   - the split-child-bundle redesign per findings 4–7 (bind sid↔key authoritatively; child bootstrap with
     backups; segment-scoped census). — **done, all four: on-chain-`F`-derived aggregate binding; the child
     bootstraps from its conveyed segment + handover instead of a backup vector; per-segment censuses.**
3. ~~**THE HONEST STATE:** the in-ladder split cannot be made sound purely client-side. B1 (relocated to the
   parent-state rival) is only closed by a census that rests on #2. Until #2 lands, laddering stays opt-in
   and splits of laddered coins stay refused (HF-1). Do NOT flip the default.~~
   **THE HONEST STATE (2026-07-29):** that judgement was right and was honoured — #2 was done as backend
   work, and only then did the split ship. There is now one protocol; laddered coins split in-ladder
   (sdk58/sdk59) and received children are first-class (sdk60/sdk17). `split_coin`'s HF-1 refusal survives
   on the direct API, where it is the correct answer, not a gate.
4. ~~Follow-on: adaptor-sig LN (an adaptor-sig design, since deleted) for the LN lane.~~ **Superseded: the LN lane rides a HODL
   invoice latch, not adaptor signatures (`LIGHTNING.md`).**
   Lightning works in both directions on the ladder — sdk63 (pay), sdk64/sdk67 (receive), sdk65 (non-exact
   in-ladder pay), sdk66/sdk68 (failure/reclaim). [D3] the one-call PAY API minted via `ensure_exact_coin`
   and therefore refused every laddered coin — fixed. The LN-latched piece is the one case that stays
   terminalized (it sits unclaimed past the pending-transfer lock's window).

---

## O-1 RESOLUTION (2026-07-21) — TES-R CAN be made sound; the count census is the right mechanism

The O-1 counter-machine review (the missing TESR-3 foundation) settled the central question:

> **A blind SE CAN support a receiver-verifiable "no hidden state" property — by COUNTING signing rounds,
> not by seeing messages. Counting is orthogonal to blindness.**

- A **label** census (`{level,m,k}`, as PROTOCOL.md:208 specified) is genuinely impossible — a blind SE
  cannot verify a declared coordinate matches the signed message (Theorem 1). That machine is dead; do NOT
  build it.
- The shipped `verify_bundle` uses a **COUNT** census (`num_sigs == flat_backups + tiers + superseded`),
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
  **STILL OUTSTANDING — the one item on this page not covered by a live test.** The dev lockbox and the SGX
  `enclave/App` are two implementations of the same signing surface; every SDK test exercises the former.
  A Linux+SGX build and the reuse-over-two-messages attack test are still owed.
- **sign/second fork gates** (9d63f15), **get_statechain_info NULL-challenge panic** (42a39fe),
  **sign/first fail-open lock → fail-closed** (549d9d2) — all server-side; activate on redeploy.
  — **REDEPLOYED (server + lockbox rebuilt from source); the full suite runs against them.**
- **S7 nodejs/web fail-closed on laddered coins** (ca76dfc).

### ~~BLOCKED~~ RESOLVED / RESIDUAL (not a client patch; needed infra or protocol design)
- **Keystone — retry-safety response cache** (the last thing gating laddering-by-default): lockbox caches the
  partial sig keyed on the challenge (return cached on retry, no re-sign / no increment; nonce-reuse guard
  preserved); coordinator caches likewise; client persists the session and retries `sign/second` — never
  restarts `sign/first`. This makes the count census retry-safe (a lost response no longer bricks a coin).
  BLOCKED because the lockbox is C++ that cannot be built on the dev host (heavy vcpkg/google-cloud-cpp);
  needs a CI/Linux build to implement+verify. The client-retry half has only marginal value without the
  cache and is deferred to land together.
  — **✅ DONE (see the KEYSTONE section below): the build obstacle was wrong.** The dev lockbox is a plain
  Docker C++ container, so it rebuilds locally; only the SGX `enclave/App` needs real hardware.
- **S5 — [RESIDUAL, recoverable — never fund loss] presign abandonment burns/bricks ladder rungs**: presign co-signs S' on a CLONE (num_sigs++)
  without updating the sender's persisted bundle, so an abandoned/failed transfer leaves num_sigs above
  what any future bundle discloses ⟹ every future receiver rejects (count mismatch) ⟹ brick; a malicious
  receiver can trigger it in one round-trip. The sound fix is a receiver-liveness gate (don't co-sign for a
  no-show) or a reclaim operation (re-sign the owner state one rung lower, disclosing the abandoned S' as
  superseded) — a protocol feature coupled to the keystone's idempotent presign, NOT to be improvised.
  > **STATUS: partially closed, and the residual is bounded — the coin is always EXITABLE; only off-chain
  > RE-transfer bricks.** The *malicious* half is closed: `transfer_sender::execute` now does every sender
  > pre-sign (backups + `S'`) BEFORE `get_new_x1` opens the transfer, and the coordinator's
  > pending-transfer lock then refuses further co-signs on that sid, so a receiver cannot induce extra
  > rungs by stalling. The *benign* half (an abandoned/failed transfer leaves `num_sigs` one above what the
  > bundle discloses) remains: the receiver-liveness gate was NOT built. Recovery is a `refresh()`
  > re-anchor, which resets the ladder and restores re-transferability. Live evidence for the LN instance
  > of this path — the orphan `S'` left by a failed latch — is **sdk68** (exact-amount reclaim) and
  > **sdk66** (non-exact rollback); the handling is documented at `SspClient::reclaim_lightning_payment`.
  > No test covers the non-LN abandonment case; a scoped SE `sig_count` reconcile is still unbuilt.
- **S1 — [STILL OPEN, deployment item] lockbox port 18080 published + unauthenticated** (confirmed live: `0.0.0.0:18080`): anyone
  reaching it calls sign/first+sign/second directly, bypassing every coordinator gate and voiding the
  census. Fix = authenticate the lockbox↔coordinator channel (shared secret/mTLS; lockbox rebuild) +
  unpublish the host port on co-located topologies. BLOCKED on lockbox rebuild + a deploy/topology call.
  **Still true of the shipped compose files** (`docker-compose-main.yml`, `docker-compose-lockbox.yml` both
  publish `18080:18080`). The lockbox-rebuild half of the blocker is gone (it rebuilds locally, see the
  keystone), so what remains is the auth channel + a topology decision. Every census claim on this page
  assumes the coordinator is the only caller of the lockbox — that assumption is a DEPLOYMENT obligation,
  not a code guarantee.

~~**Do NOT ladder deposits by default until the keystone lands and is verified.**~~ **The keystone landed and
was verified (sdk56); the default no longer exists — every fresh confirmed root coin is laddered.** The
count census is sound and retry-safe, and the in-ladder split (B1) rides on top of a trustworthy parent
census, exactly as this section required.

---

## KEYSTONE DONE (2026-07-22) + the TES-R-completion gate chain

The keystone LANDED and is VERIFIED (see the memory + commits 11edbae/5b5b698/ea09c01): the lockbox caches
the partial sig keyed on the session and increments sig_count atomically; the client retries the same
sign/second. sdk56 proves the signing round is idempotent under retry (num_sigs counted once across 3
replays). Server + lockbox redeployed from latest source; the full TES-R suite green.

**To reach "laddered by default + the pre-TES-R lane deleted" the remaining gates are, in order:**
**— ALL FOUR ARE NOW PAST. Outcome recorded per gate; gate 3 was solved a different way than planned.**

1. **S5 (presign-abandonment brick) — NEEDS A SERVER-SIDE PROTOCOL CHANGE (design ruling, workflow
   wab9jyo5b).** A purely client-side fix is IMPOSSIBLE — three independent FATALs, S-1 dispositive:
   the laddered transfer path co-signs a RECEIVER-PAYING SIGNED-ONCE BACKUP on every attempt
   (transfer_sender.rs:278 -> create_backup_tx_to_receiver), whose locktime is IDENTICAL across abandons;
   splicing journaled backups into the conveyed list hits INV-5 (ladder_decrements_by_interval,
   receiver.rs:274) — which cannot loosen without reopening the S2 hidden-state defense — and NOT splicing
   leaves flat_backups short => verify_bundle num_sigs mismatch. Both horns BRICK. FIX =
   **receiver-liveness gate**: add a receiver-signed commitment to the transfer protocol; the SE co-signs
   BOTH the signed-once backup AND S' ONLY against that commitment, so a no-show strands nothing (this is
   the work already flagged at transfer_sender.rs:257). Latent note (limitation, independent of S5): the
   receiver-paying signed-once backup a laddered coin co-signs descends from tx0's output, which T also
   spends — a latent on-chain rival of T on EVERY successful transfer of a laddered coin; deserves its
   own verification pass.
   > **OUTCOME — gate PASSED without the receiver-liveness gate; a bounded residual remains.** What shipped
   > instead: the pre-sign/`get_new_x1` re-order + the coordinator pending-transfer lock, which removes the
   > *inducible* (malicious-receiver) version. The benign abandonment still leaves `num_sigs` one high and
   > bricks off-chain RE-transfer until a `refresh()` re-anchor; the coin stays fully exitable throughout, so
   > this is never fund loss. See the S5 status block above for the evidence (sdk66/sdk68 cover the LN
   > instance; the non-LN abandonment case has no test).
   > **The latent note is still LATENT and still unverified.** The receiver-paying signed-once backup is
   > still co-signed on every transfer (`transfer_sender::execute`), and it still descends from the output
   > `T` also spends. Nothing on this page or in the suite verifies that rival directly. What bounds it in
   > practice: the backup is absolutely-locktimed (it cannot be broadcast until `L`, while `T` has no
   > timelock), INV-5 forces each hop's backup strictly LOWER, and the stale-ancestor clawback it would
   > enable is what the watchtower + `auto_exit_due` defeat (sdk45 keyless bundle, sdk51 hostile trigger,
   > sdk40 PART 2 stale state dying at consensus). A dedicated verification pass is still owed.
2. **In-ladder split (B1)** — enables non-exact payments from laddered coins (split_coin refuses laddered coins today,
   transfer.rs:426). Redesign in progress (workflow w7s7jxly2) now that the parent census is trustworthy.
   > **DONE.** `transfer()` splits a laddered coin in-ladder (sdk58 control + 11 adversarial REJECTs; sdk59
   > end-to-end payment), and the received child is first-class (sdk60 two off-chain hops; sdk17 partial
   > second hop). `split_coin`'s refusal survives only on the direct API. [D1] fixed the admission floor.
3. **Adaptor-sig LN = "finish lightning swaps"** — LN rides the pre-TES-R batch_id latch (lightning.rs);
   deleting that lane breaks it. Needs adaptor signatures (server/protocol).
   > **SOLVED DIFFERENTLY — adaptor signatures were NOT built.** The lane pivoted to a HODL-invoice latch at
   > the same trust bar (`LIGHTNING.md` — the adaptor/latch-fix designs were never built and are deleted): sdk63 pay, sdk64/
   > sdk67 receive, sdk65 non-exact in-ladder pay, sdk66/sdk68 failure+reclaim. [D3] the one-call PAY API
   > minted via `ensure_exact_coin` and so refused every laddered coin — fixed. The LN-latched piece is the
   > one case that deliberately stays terminalized.
4. Then: flip deposit_protocol_default -> 2; delete the pre-TES-R lane (verifier lane,
   validate_signature_scheme, the signed-once backup count term, batch_id latch, migrate tests); rewrite
   all docs/utexo/*.
   > **DONE — with one correction to the plan.** `deposit_protocol_version` / `UTEXO_PROTOCOL_DEFAULT` are
   > deleted and the tests are migrated. But deleting that lane did NOT mean deleting the un-laddered coin
   > SHAPE: `validate_signature_scheme` and the backup-chain handover are RETAINED and load-bearing, because
   > an RGB carrier must never be laddered and an un-broadcast-funded sub-coin cannot root a trigger [B0].
   > The `flat_backups` term likewise survives as the census's baseline count — it is the number of
   > signed-once backup transactions conveyed with the coin, and a laddered root coin's deposit still
   > co-signs exactly one of them at claim, so the term is not always zero. What was deleted is the *choice*.

~~**Every remaining gate is server/SE/protocol work, not a client patch**~~ — that read was correct, and the
backend program was executed gate by gate with adversarial review + live-stack verification, exactly as
scoped. TES-R's safety rests on the blind SE (counting + gating). The build/deploy cycle is proven (incremental
Docker rebuild + docker cp + restart). ~~Do NOT flip the default or delete the pre-TES-R lane until gates 1-3
land + are verified.~~ **All gates are past; there is one protocol.**
