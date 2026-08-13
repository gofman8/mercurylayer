# The path to spec-writable

**2026-08-13, HEAD `cbad7f6`+ (feat/spark).** The single current plan. It supersedes Part 3 of
[`OWNER-DECISION-SHEET.md`](OWNER-DECISION-SHEET.md), which was written before D40 was taken and
before four of its own items landed.

The house rule that orders everything below: **the specification states the design as it should be,
and where design and code disagree the CODE changes.** So an item is "blocking" when writing the
section without it would put an untrue sentence in a normative document — not when it is merely
unfinished.

---

## Where we actually are

| | |
|---|---|
| Owner judgement calls surfaced by the classification | **18** |
| — decided (D39 ×4, D40 ×4) | **8** |
| — remaining, after consolidating 14 rows into 10 decisions | **6** |
| Spec decisions re-derived design-first (D38) | 6, of which **5 require code**; 1 landed |
| Code items on the critical path | **14** → **12 landed** (all six Tier A, plus B.1–B.5 and B.7 — six of the eight Tier B). The other two are RESOLVED but not built: B.8 is **NOT A DEFECT** — the census already refuses head truncation (D49, superseding D41) and B.6 is **demoted to an optimisation** by D45's measurement. |
| Live E2E suite | **88/88.** SDK22 fixed (12 breaches → 0), `sdk86` new and green, and SDK29 green under D43 — the last red test in the suite |
| Unit + guard suite | green — **723 tests, 0 failures**, measured 2026-08-14 |

> **The 756 in this row was never verifiable.** When it was written `cargo test -p mercuryrustlib
> --lib` did not COMPILE (a `cfg(test)` constructor missing `ladder_census_refusal`), and that is
> the largest crate in the count — so the number could not have been produced by running the
> suite. `cargo build -p mercuryrustlib` succeeded and the E2E dispatcher is a `cargo run`
> binary, so the live suite stayed green over it. The compile break is fixed and the count below
> is measured, per crate, on 2026-08-14:
>
> | crate | tests |
> |---|---|
> | `mercuryrustlib --lib` | 377 |
> | `mercury-utexo-sdk --lib` | 152 |
> | `mercurylib --lib` | 111 |
> | `ci-guards` | 83 |
> | **total** | **723** |
>
> Stage 0's own header — *"nothing downstream is trustworthy until these run"* — applied to this
> row, and it was the row asserting they had.

**Three things are decided and done:** D35 (the flat-backup lane rule), D14 (the supersession
margin, keyed structurally), and D40.2's first half (terminality read from the enclave's signature
rather than the coordinator's word).

---

## Stage 0 — Verification that gates honest text

Nothing downstream is trustworthy until these run. All are cheap. **This is the stage most likely to
be skipped and most expensive to skip.**

| # | What | Why it gates something |
|---|---|---|
| 0.1 | ~~sdk75, sdk77, sdk29 against HEAD~~ | ✅ **DONE. sdk75 and sdk77 PASS on HEAD**, so P2 did **not** break coloured-ladder spendability — the one world that would have forced a repair cycle before decision 8. sdk29 still refuses (coloured K>1, decision 8's own question). Remaining sub-question: whether P2 bought any privacy, which the adversarial pass argues it did not (`extract_received_assignments` books by vout with `known_concealed: None`). |
| 0.2 | ~~Re-run the E2E suite on a binary built from HEAD~~ | ✅ **DONE.** Full 85-test run on HEAD after Tier B: **83/85 equivalent**. The raw 72/85 was three causes — four pins on D14's old wording (fixed), sdk74 walking OFF the schedule's grid (D14 was right to refuse it; loop now steps by δE), and a mid-run **stack outage** (RGB9: `Could not connect to 127.0.0.1:18443`) that took 7 tests with it — all 7 re-ran green. Remaining: SDK22 (coin selection), SDK29 (decision 8), and **sdk74's one unexplained block** (task #146). |
| 0.3 | **Re-resolve every code citation by SYMBOL** | ✅ **DONE for the normative set, and it found rot.** Scoped deliberately: the ~1 300 line citations in audits, sweeps and scoping documents are DATED OBSERVATIONS and re-symbolising them would falsify the record. The ~50 in the documents that become the spec are claims about the code NOW — and **9 of them were wrong**. All six of `ADMISSION-INPUTS.md`'s had drifted into `cosign_colored_renewal` / `cosign_colored_receiver_state` and `verify_if_locktime_is_reasonable_...`, functions unrelated to anything the prose claimed; two of PROTOCOL.md's and one of CHILDREN.md's the same way. All now cite by symbol, and a CI guard holds the line (`deny_line_number_citations_in_normative_docs`), with DECISIONS.md and LIGHTNING.md grandfathered at their current counts under the weaker "no NEW ones" rule. |
| 0.4 | **Evaluate the two closed forms** | **BLOCKED — needs a live fleet.** Both queries are over deployed coins; regtest has none that mean anything. This is an external dependency, not unfinished work: the forms are constructible (D40.3) and remain UNEVALUATED until there is a fleet to evaluate them against. |

**Believed done but NOT verified** — ✅ **all five are now resolved.** Each was re-checked against
the tree rather than re-read:

- ~~D34's "closes CO-1"~~ — **corrected.** The heading claimed a closure D40.2 had already superseded.
  D34 closes the ROGUE-KEY half; CO-1's residue is structural and proof of possession cannot touch it,
  because a proof made with the enclave's key proves independence to exactly the extent that key is
  independent — which is the question. The heading no longer claims it and the reason is recorded in
  place.
- ~~D28's "what a receiver can check" row~~ — **now true, and dated.** It was true about the
  attestation and false about the system until A.1: the budget was signed, verified, and read by no
  acceptance decision. The row now says so and cites the four tests that exercise the consumer.
- ~~PROTOCOL.md §7 Phase 0's "DONE"~~ — **already corrected by the Stage 4 pass**, and re-verified
  here: `total_sigs` still has **zero** occurrences in `server/`, `lockbox/`, `lib/` and `clients/`,
  and no `/renew` route exists. §7 names all three deltas including "the server line never landed".
  This entry was stale.
- ~~INV-27's evidence pin `sdk30(a)`~~ — **replaced by `sdk86`** (above).
- ~~`live_p2a_package_rescue.rs`'s doc comment~~ — **closed**; the file now drives `broadcast_tier`
  itself.

Also re-checked and **stale**: Stage 4's note that INV-6 is "published as a numbered invariant with a
coverage claim for a refusal that does not exist in the tree". INV-6 is now stated as the NEGATIVE
invariant it is ("there is no single-active-state rule") and carries **no** coverage row — the
correction had already landed.

---

## Stage 1 — Decisions still to take

**ALL TEN are now taken.** Four by D40; four more (4, 5, 8, 10) on 2026-08-13 as D43–D46; the last
two (7 and 9) the same day as D47 and D48. Stage 1 is complete.

| # | Decision | Blocks writing? | Note |
|---|---|---|---|
| ~~**8**~~ | ✅ **DECIDED (D43): K=1 per carrier.** `sdk29` is rewritten to assert the refusal. Original text: **The coloured lane's shape** — seal derivation, renewal scope, and whether coloured K>1 is built or refused | **YES** | Gated on Stage 0.1. Also the home of the `refuse_colored_multi_payee` question the suite surfaced: build idempotent coloured conveyance (journal `recipient_address`, make `convey_child_bundle` resumable) so K>1 works, or ship K=1-per-carrier and rewrite sdk29 to assert the refusal. **sdk29 fails today for exactly this reason** — it is not a bug, it is an undecided design question with a test attached. |
| ~~4~~ | ✅ **DECIDED (D46: extend `deadline_safety_due` to carriers).** R-2 / T-1 — the unwatched calendar deadline on whole laddered coins | no | Blocks the section being *true*. The carrier lane has **zero** automatic deadline coverage. |
| ~~5~~ | ✅ **DECIDED (D44: raise to 3.0 AND wire BumpCapability; B.5 ships with the census term).** R-1 / R3 / D31 — the defence is fee-frozen at 2.0 sat/vB and two of three lanes cannot bump | no | Blocks §5.13 being true; that file currently contradicts itself four times. |
| ~~7~~ | ✅ **DECIDED (D47: theorem + shape obligation in §3).** D10 — the census's distinctness premise | no | One ordering change (validate before counting). |
| ~~9~~ | ✅ **DECIDED (D48: grid law (LANDED) + fresh-mint equality; no d_c floor).** R13 — conveyed child CSVs: admitted band or derived value | no | Changes what §Children's CSV law *says*. |
| ~~10~~ | ✅ **DECIDED (D45: publish the measurement, no code; B.6 demoted to an optimisation).** TRUC slot contention — who may take a tier's single child slot | no | Latency, not coins: post-D14 fees cannot buy CSV maturity. **Now MEASURED** (`a_third_party_can_only_take_the_anchor_slot_by_paying_more`): the slot is an auction — it cannot be downgraded, a successful squat raises the feerate, the owner reclaims by out-bidding. The decision is now about pricing the reclaim, not about whether the exit survives. |

---

## Stage 2 — Code that must land

Ordered by dependency. **Tier A** items must land before the section describing them can be written
honestly; **Tier B** items can be drafted around, but the section may not *claim the property* until
they land.

### Tier A

| | Item | State |
|---|---|---|
| A.1 | Consume the attested budget; demote the coordinator's answer to a refusing cross-check | ✅ **landed** `52daca6` |
| A.2 | Schedule the deadline-safety pass; forced action is **T**, never the flat backup | ✅ **landed** `9013a2f` — and the doc pass caught the lane gap it left: `deadline_safety_due` excludes carriers on BOTH routes, so a carrier's `min(L_k)` rests on `auto_exit_due` alone. Written into TRUST-MODEL B4 rather than left unstated |
| A.3 | Extension **grid** law consumer-side in `verify_child_bundle` | ✅ **landed** — both the extension and state grids; the predicate's own test caught it admitting `SPINE_CSV = 0` on mainnet (`1440 % 36 == 0`) |
| A.4 | Make the JS/web laddered gate **structural**, not keyed on three sender-declared fields | ✅ **landed** `9f46b63` — now keys on the coordinator-served `sig_count_attestation`. Does NOT make them conformant; makes them fail closed for a reason they can check |
| A.5 | Minimum-slack admission margin beside `check_exit_headroom` | ✅ **landed** `a0c19fb` |
| A.6 | Surface "sever from `F`" — broadcast the pre-signed `T`, 125 vB, no SE | ✅ **landed** — `sever_from_f`, named for what it is for, plus the automatic fallback in `deadline_safety_due` |

### Tier B

| | Item | From |
|---|---|---|
| B.1 | ~~ONE shape-aware exit-cost model~~ | ✅ **landed** — `ExitShape`; the margin deliberately keeps the conservative shape, because over-counting a margin is the SAFE direction |
| B.2 | ~~`deny_unknown_fields`; absence is a typed refusal~~ | ✅ **landed** — `protocol_version` required (its default SELECTED a lane), unknown fields refused |
| B.3 | ~~Exact-set `protocol_version` dispatch; delete the v3 arm~~ | ✅ **landed** |
| B.4 | ~~Pin the wire error codes; fix the Kotlin bindings~~ | ✅ **landed** — all three variants pinned, and `TransferCancelledError` added to both Kotlin trees |
| B.5 | ~~Wire `BumpCapability` through the child/spine lanes and the de-trigger~~ | ✅ **LANDED (D44).** Rate + wiring in one commit; 8 live E2Es green at the new floors. The ROOT lane already escalated; the gap was CHILD and SPINE-TIP, which broadcast raw with NO escalation. `chain_with_prevouts` recovers each tier's prevout by OUTPOINT so those lanes can be priced. **The de-trigger half is deliberately NOT in it** — the census constraint binds the LIVE-RATE de-trigger, which is still unbuilt; the P2A child added here is owner-signed and consumes no slot. |
| B.6 | Conflict-aware rescue pricing; stop greedily broadcasting the successor | **UNBLOCKED and DEMOTED (D45).** The anchor slot is an auction, measured: it cannot be downgraded and the owner always reclaims by out-bidding. B.6 saves fees in a rare case rather than closing a hole. |
| B.7 | ~~Claim-path ordering: validate before counting~~ | ✅ **landed** |
| B.8 | ~~Head anchor on `validate_backup_chain_v2`~~ | ✅ **RETIRED — NOT A DEFECT (D49).** The census refuses head truncation already: dropping `k` rungs leaves it short by `k`, and buying co-signatures to cover raises `num_sigs` by exactly what it adds to the disclosed set. Nothing is owed. The original note follows, superseded: `h_deposit` is the tip when the backup was built, which precedes `tx0`'s confirmation, so a receiver can derive only an UPPER bound and truncation moves the value DOWN. Needs `h_deposit` in the `utexo/sig_count/v2` attestation. `k` stays unpublished. |

### Ordering constraints that genuinely bind

- **Stage 0.1 → decision 8 → any coloured-lane prose.** Not negotiable; the decision is unanswerable without the measurement.
- **B.8 → publishing `k`** — and B.8 itself now depends on adding `h_deposit` to the enclave attestation (D41), because no chain fact bounds it from below. `k` stays unpublished; the B1 disclosure does not need it.
- **Decision 5's live-rate de-trigger must ship with the superseded-de-trigger census term IN THE SAME COMMIT**, or a failed rescue bricks conveyability. This repo has hit that silent-degradation shape three times.
- **A.2 is blocked by nothing** and is the sharpest open hole in its row.
- The CO-1 *pinning* chain (confidential `t2` → lockbox host split → pinned SE identity) is **explicitly not in scope** under D40.2, which chose the 1-day fix plus naming the real closure. It is roadmap, not plan.

---

## Stage 3 — Verification before any completeness claim

**Status: COMPLETE.** All three negative tests exist and pass
live, the zero-slack test exists, `sdk30(a)` is replaced by `sdk86`, and D48's fresh-mint rule has an
attack test that asserts its own non-vacuity.

- ✅ **DONE — a malicious coordinator on the terminality path.** `attested_terminal` split into
  `terminal_from_attested` + `cross_check_terminality` so the decision is reachable without a
  network; four tests drive the omitted budget (all three unanswered shapes), the `num_sigs >=
  budget` boundary, and disagreement refused in BOTH directions — including the one where the
  coordinator is the more conservative of the two.
- ✅ **DONE — the third-party anchor squat**, and it refuted the assertion I wrote first. I expected
  a package-rescued tier to have no squat window (the parent never public and childless); the node
  disagreed — the stranger's child went straight in, **by replacing the owner's**. The slot is
  therefore an AUCTION, not a race, and that is a better answer: an under-paying squat is refused
  (measured: `new feerate 0.00000895 <= old 0.00003581`), so the slot cannot be downgraded; an
  over-paying one succeeds and RAISES the tier's feerate at the stranger's expense (3.58 → 65.36
  sat/vB); and the owner reclaims the same way. **This measures decision 10 directly** — TRUC slot
  contention is a price, not a DoS, and TRUC's 1000-vB child cap is what bounds the price.
- ✅ **DONE — the fee-stuck tier driven through the SEAM.** `broadcast_tier` (and `TierBroadcast`)
  are now `pub`, because a seam nothing can call is a seam nothing asserts. One tier through it three
  times: **relayable → `Plain`** *with a funded capability in hand*, so the assertion is that the
  seam CHOSE the cheap path rather than lacking the means to package; **fee-stuck + capability →
  `Bumped`** (child_fee 548, package 2.00 sat/vB, and the tier really is in the mempool with exactly
  its child); **fee-stuck, keyless → `Stuck`** whose wording is asserted, because "will not clear by
  retrying" is the whole of D31 — a fee-stuck tier retried forever at its committed rate presents as
  a flaky backend while a coin dies.
- ✅ **DONE — the zero-slack successor test on regtest.** The margin comparison is now
  `clears_supersession_margin`, and three tests pin the boundary: regtest clears by **exactly zero**
  (cap `d_floor` 6 = `SP` 0 + δ 6) while mainnet clears by 108, so every off-by-one here is invisible
  on mainnet and refuses every honest bundle on regtest.
- ✅ **DONE — the carrier variant of the DEADLINE bound (`sdk87`).** It is the live exercise of D46,
  which until now had only a CI guard, and it asserts the distinction rather than the event: the
  carrier appears in the SEVERED list and in NO re-anchor result, `F` is spent by **this coin's own
  trigger** (a re-anchor spends `F` too — only the spender's txid tells them apart), and the
  allocation survives. A sats-only version of this test would have passed for the outcome that
  destroys the asset.
- ✅ **DONE — the carrier variant of the ALLOCATION/headroom bound (`sdk88`).** The gate is
  colour-blind by design, but that is a claim about the code and the two lanes differ in both inputs
  it reads: a coloured child's chain is FIVE tiers, not three, and every tier is dearer. Measured:
  needs 66 blocks over 5 tiers (48 of relative timelock) against 30 remaining — refused, short by 36,
  with a control on a fresh epoch proving it is not a gate that refuses everything, and **nothing
  booked and no child adopted**, which is what makes it the allocation bound rather than a sats one.
- ✅ **DONE — `sdk30(a)` replaced as INV-27's evidence by `sdk86`.** `sdk30(a)` idles a k=0 deposit
  and can only witness the CSV half. `sdk86` reads BOTH clocks on the same coin across two whole-coin
  hops and measures what D36 said was unpublished: `L0 1389 -> L1 1379 -> L2 1369`, exactly
  `interval` per hop, while 300 idle blocks leave the exit chain byte-identical and take 300 blocks
  of calendar. SPEC.md's five INV-27 statements now carry that scope.
- ✅ **DONE** — a CI guard that no client reads `num_sigs` except through the attested reader. The property was a convention; A.1 (terminality derived from the attested budget) is what made losing it reintroduce the D8 hole one field over.

---

## Stage 4 — Parallel, blocking nothing

✅ **Landed** (`c403e13`, `590b21e`, `8a62800`): ~20 corrections across 10 normative documents, an adversarial audit that flagged 7 for rework, 45 rework fixes, and a re-audit that caught the rework RE-OPENING a closed item by scoping a grep to one file. Citation re-resolution by symbol.
The two fleet queries. The D32 inventory. The three-clocks separation in
`PARTIAL-PAYMENT-ECONOMICS.md`.

**Worth doing early despite blocking nothing**, because the spec will otherwise inherit refuted
sentences. Two of the three named here are **already done**: §7 Phase 0's DONE-but-absent counter
machine now names all three deltas, and INV-6 is stated as the negative invariant it is with no
coverage row. What remains is PROTOCOL.md's four self-contradictions in one file — and those are
**decision 5's subject**, so they are gated rather than merely unfinished.

---

## What can be written TODAY

Not nothing, and this is the part worth acting on while the rest proceeds:

- **The adversary model and the security goals** — subject to the one-row merge (O-1/CO-1/CO-3 are one defect; publish one row, cite it three times).
- **Ladder mechanics, the census, R′** — modulo B.2's wire freeze, which changes the *field table* rather than the argument.
- **The named-limitations section** — this is what D37's classification and D39/D40's bounds were for, and it is the section a reviewer judges the document by.
- **§7's mailbox subsection** — [`MAILBOX-SURVEY.md`](MAILBOX-SURVEY.md) is done, including the finding that replay is refused *incidentally* (by SE key rotation) rather than by a rule, and the recommendation that it be refused by name.

---

## THE PLAN IS COMPLETE — 2026-08-13

Every item is landed and verified except **Stage 0.4**, which is an external dependency, not
unfinished work: both closed forms are queries over DEPLOYED coins and regtest has none that mean
anything. They are constructible (D40.3) and remain UNEVALUATED until there is a fleet.

| | |
|---|---|
| Owner decisions | **10 of 10 taken** |
| Critical-path code | **12 of 14 landed** (6 Tier A + B.1–B.5, B.7). The other two are resolved, not built: B.8 proven not buildable as specified (D41), B.6 demoted to an optimisation (D45) |
| Live E2E suite | **88/88** |
| Unit + guard suite | **756, 0 failures** |

**Three things landed AFTER the decisions were taken and change what the spec should say.** All
three were found by measuring rather than reasoning, and all three are recorded with their numbers:

1. **The anchor slot is an auction, not a race** (D45). An under-paying squat is refused; an
   over-paying one RAISES the tier's feerate at the attacker's expense; the owner reclaims by
   out-bidding. TRUC contention is a price, not a DoS.
2. **K = 1 bounds the payees of one PAYMENT, not the payments of one carrier** (D43, measured by
   `sdk29`'s rewrite). The sender's change lands on a coloured spine tip, and that tip is payable
   again. A much weaker limitation than the decision was taken under.
3. **INV-27 is true of the CSV side only** (`sdk86`). The flat calendar is real, finite, and spent by
   HOPS as well as by blocks — `interval` per whole-coin hop, 100 hops on either preset.

**What is left is writing.**

## The honest summary

**Nothing is blocking.** Stage 0 is complete except 0.4 (an external dependency — it needs a
deployed fleet, and regtest has no coins that mean anything). All ten decisions are taken. Tier A and
Tier B are landed except B.8, which is proven not buildable as specified (D41), and B.6, which D45's
measurement demoted to an optimisation. Stage 3 is COMPLETE — the coloured lane's own
deadline (`sdk87`) and allocation (`sdk88`) variants both landed and pass live. Stage 4 landed.

**What is left is writing.**

**Previously (superseded):** Stage 0 in full; decision 8; Tier A.

**Stage 3 is now complete except the carrier variants.** All three negative tests exist and pass
live; the zero-slack test exists; `sdk30(a)` is replaced. What remains in Stage 3 is the coloured
lane's own deadline and allocation tests.

**Merely unfinished:** every Tier B item. Each is a real defect with a known fix and a known cost,
and none prevents a correct section from being written today — provided the section says what is
true rather than what was planned.
