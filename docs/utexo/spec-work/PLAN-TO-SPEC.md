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
| Code items on the critical path | **14** → **11 landed** (all six Tier A + B.1, B.2, B.3, B.4, B.7). B.8 is **proven not buildable as specified** (D41). **B.5 and B.6 are gated on decisions 5 and 10, which are not taken.** |
| Live E2E suite | **complete: 81/85**, and on a HEAD binary SDK80 + RGB8 also pass → **83/85 equivalent**. Two remain: SDK22 (coin selection, item below) and SDK29 (blocked on decision 8) |
| Unit + guard suite | green — **711 tests, 0 failures** |

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
| 0.3 | **Re-resolve every code citation by SYMBOL** | **Partly done** — the Stage 4 pass introduced zero new line-number citations and replaced the ones it touched with symbols; the re-audit found more still standing in SPEC-ROADMAP (D19/§4a, WP3a, WP3b, §7) which the follow-up commit fixed. A full corpus sweep is still owed. |
| 0.4 | **Evaluate the two closed forms** | **BLOCKED — needs a live fleet.** Both queries are over deployed coins; regtest has none that mean anything. This is an external dependency, not unfinished work: the forms are constructible (D40.3) and remain UNEVALUATED until there is a fleet to evaluate them against. |

**Believed done but NOT verified** — each must be re-graded or re-tested before it appears in prose:

- D34's "closes CO-1" — it does not, as deployed.
- D28's "what a receiver can check" row — attested a budget that, until today, no acceptance decision read.
- PROTOCOL.md §7 Phase 0's "DONE: generalized counters {level, m, k, total_sigs}, POST /renew/init" — `total_sigs` has **zero** occurrences in `server/`, `lockbox/` or `lib/`; no `/renew` route exists.
- INV-27's evidence pin `sdk30(a)` — it idles a k=0 deposit and structurally cannot witness the case where INV-27 is false.
- `live_p2a_package_rescue.rs`'s doc comment — claims to drive a seam its body never enters.

---

## Stage 1 — Decisions still to take

Four of the ten consolidated decisions are taken (D40). **Six remain.** Only one of them blocks
*writing*.

| # | Decision | Blocks writing? | Note |
|---|---|---|---|
| **8** | **The coloured lane's shape** — seal derivation, renewal scope, and whether coloured K>1 is built or refused | **YES** | Gated on Stage 0.1. Also the home of the `refuse_colored_multi_payee` question the suite surfaced: build idempotent coloured conveyance (journal `recipient_address`, make `convey_child_bundle` resumable) so K>1 works, or ship K=1-per-carrier and rewrite sdk29 to assert the refusal. **sdk29 fails today for exactly this reason** — it is not a bug, it is an undecided design question with a test attached. |
| 4 | R-2 / T-1 — the unwatched calendar deadline on whole laddered coins | no | Blocks the section being *true*. The carrier lane has **zero** automatic deadline coverage. |
| 5 | R-1 / R3 / D31 — the defence is fee-frozen at 2.0 sat/vB and two of three lanes cannot bump | no | Blocks §5.13 being true; that file currently contradicts itself four times. |
| 7 | D10 — the census's distinctness premise | no | One ordering change (validate before counting). |
| 9 | R13 — conveyed child CSVs: admitted band or derived value | no | Changes what §Children's CSV law *says*. |
| 10 | TRUC slot contention — who may take a tier's single child slot | no | Latency, not coins: post-D14 fees cannot buy CSV maturity. |

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
| B.5 | Wire `BumpCapability` through the child/spine lanes and the de-trigger | **BLOCKED on decision 5.** Its recommendation includes `committed_fee_rate` 2.0 → 3.0, which re-prices every floor and every piece in circulation — an owner call with real user impact, not a mechanical port. |
| B.6 | Conflict-aware rescue pricing; stop greedily broadcasting the successor | **BLOCKED on decision 10.** |
| B.7 | ~~Claim-path ordering: validate before counting~~ | ✅ **landed** |
| B.8 | ~~Head anchor on `validate_backup_chain_v2`~~ | **NOT BUILDABLE as specified — see D41.** `h_deposit` is the tip when the backup was built, which precedes `tx0`'s confirmation, so a receiver can derive only an UPPER bound and truncation moves the value DOWN. Needs `h_deposit` in the `utexo/sig_count/v2` attestation. `k` stays unpublished. |

### Ordering constraints that genuinely bind

- **Stage 0.1 → decision 8 → any coloured-lane prose.** Not negotiable; the decision is unanswerable without the measurement.
- **B.8 → publishing `k`** — and B.8 itself now depends on adding `h_deposit` to the enclave attestation (D41), because no chain fact bounds it from below. `k` stays unpublished; the B1 disclosure does not need it.
- **Decision 5's live-rate de-trigger must ship with the superseded-de-trigger census term IN THE SAME COMMIT**, or a failed rescue bricks conveyability. This repo has hit that silent-degradation shape three times.
- **A.2 is blocked by nothing** and is the sharpest open hole in its row.
- The CO-1 *pinning* chain (confidential `t2` → lockbox host split → pinned SE identity) is **explicitly not in scope** under D40.2, which chose the 1-day fix plus naming the real closure. It is roadmap, not plan.

---

## Stage 3 — Verification before any completeness claim

- The three negative tests that **do not exist today**: a malicious coordinator on the terminality path; a fee-stuck tier driven through a *pass* rather than the builder directly; a third-party anchor squat.
- The zero-CSV successor test **on regtest**, where `d_floor >= δ` clears with *zero slack* (6 ≥ 6) while mainnet clears at 144 ≥ 36. A mainnet-only test is the exact trap D14's own preset census was written to flag.
- The **carrier variant** of every deadline and allocation test. The coloured lane is where three separate bounds turned out to be sat-denominated descriptions of a victim who loses an asset.
- Replace `sdk30(a)` as INV-27's evidence.
- ✅ **DONE** — a CI guard that no client reads `num_sigs` except through the attested reader. The property was a convention; A.1 (terminality derived from the attested budget) is what made losing it reintroduce the D8 hole one field over.

---

## Stage 4 — Parallel, blocking nothing

✅ **Landed** (`c403e13`, `590b21e`, `8a62800`): ~20 corrections across 10 normative documents, an adversarial audit that flagged 7 for rework, 45 rework fixes, and a re-audit that caught the rework RE-OPENING a closed item by scoping a grep to one file. Citation re-resolution by symbol.
The two fleet queries. The D32 inventory. The three-clocks separation in
`PARTIAL-PAYMENT-ECONOMICS.md`.

**Worth doing early despite blocking nothing**, because the spec will otherwise inherit refuted
sentences: PROTOCOL.md's four self-contradictions in one file, §7 Phase 0's DONE-but-absent counter
machine, and INV-6 — published as a **numbered invariant with a coverage claim** for a refusal that
does not exist in the tree.

---

## What can be written TODAY

Not nothing, and this is the part worth acting on while the rest proceeds:

- **The adversary model and the security goals** — subject to the one-row merge (O-1/CO-1/CO-3 are one defect; publish one row, cite it three times).
- **Ladder mechanics, the census, R′** — modulo B.2's wire freeze, which changes the *field table* rather than the argument.
- **The named-limitations section** — this is what D37's classification and D39/D40's bounds were for, and it is the section a reviewer judges the document by.
- **§7's mailbox subsection** — [`MAILBOX-SURVEY.md`](MAILBOX-SURVEY.md) is done, including the finding that replay is refused *incidentally* (by SE key rotation) rather than by a rule, and the recommendation that it be refused by name.

---

## The honest summary

**Genuinely blocking:** Stage 0 in full; decision 8; Tier A.

**Merely unfinished:** every Tier B item. Each is a real defect with a known fix and a known cost,
and none prevents a correct section from being written today — provided the section says what is
true rather than what was planned.
