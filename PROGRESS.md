# PROGRESS — P0 defects → CATS-B

Branch `feat/spark`. **Nothing on this line has been merged to `dev` (the repo default branch).**
Read this file first if you feel disoriented; it is the thread.

## Objective

Close the P0 defects in the in-ladder split lane, then build **CATS-B** — the zero-CSV change spine
replacing the change child's `[extension, state]` pair with one un-timelocked spine tier plus a state
cap. Target end state: no admission gate takes a sender-supplied input; no irreversible SE operation
runs before durable state exists; quote and executor share one floor from one source; a partial
payment costs one transaction and ~one block of added exit latency instead of two and 2,124 blocks.

Design of record: `docs/utexo/PARTIAL-PAYMENT-ECONOMICS.md` §4 (§4.0 Freeze Lemma, §4.1 construction,
§4.2 why `nSequence 0` is safe, §4.7 prerequisites gating K>1, §4.8 the liveness trade).

## Work queue

1. Bind the headroom gate to signed `nSequence`; enumerate every requirement input + provenance.
2. One floor, one source, across quote and executor; kill the `tesr::load(..).unwrap_or(None)` swallow.
3. Make `journal_write` atomic; write-ahead record for `cosign_colored_in_ladder_split`.
4. Reconcile sdk02/31/74/75/78/79 with `colored_ladder = false`.
5. CATS-B stage 1: spine tier — `nSequence 0`, change gets one cap, no extension.
6. CATS-B stage 2: widen to K+1 payloads (`build_split_state` / `in_ladder_pay_many` already N-ary).
7. Update SPEC / PROTOCOL / INVALIDATION-SPEC / GRANULARITY-SPEC to what is built.

## Status

**HEAD is `2c351c6`** (CTES-R machinery, default OFF). `colored_ladder` defaults `false` at both
constructors (config.rs:278, :307) — INTENTIONAL and must stay; the lane is economically unsound
(one partial payment per carrier, ever).

**Queue items 1–4 are DONE and verified by running them here** (not on a subagent's report):

* `cargo +stable check --workspace --tests` clean (the client libs warn zero; the only remaining
  warnings in the tree are pre-existing token-server ones). Unit suites, per package:
  `mercurylib` 27, `mercuryrustlib` 71, `mercury-utexo-sdk` 91 + 1, `ci-guards` 5 + 11 + 5 — green.
* E2E, serially, from an isolated CWD against a binary built by plain `cargo +stable build`:
  **sdk82 PASS, sdk81 PASS, sdk02 PASS, sdk31 PASS.** sdk74/75/78/79 running.
  sdk81 was checked for substance, not just exit code: real SIGABRT (signal 6), journal replay, bob
  adopted the replayed child.
* **B5 needed no work** — sdk02 and sdk31 already pass with the default OFF.
* Item 1's real deliverable, the provenance enumeration, is
  [`docs/utexo/ADMISSION-INPUTS.md`](docs/utexo/ADMISSION-INPUTS.md): every term of the headroom
  requirement tagged backend / config / operator / signature / declared, plus the rule for the next
  gate. The `nSequence` binding was the small part.
* `ci-guards` narrowing: the guard's own `*assert*` exemption was line-scoped, so rustfmt wrapping
  `assert!(\n  x.is_err(),` made a test assertion look like a classifier. Fixed in the SCRIPT with a
  3-line lookback for an assert opener — **not** by adding allowlist entries. Its own fixtures
  (`guard_catches_every_covered_spelling`, `guard_catches_a_planted_call_site`) still pass.

**A regression I introduced and sdk72 caught.** The first version of the laddered-entry code ran
`tesr::load` FIRST and `continue`d on a hit — which let a coin whose deadline was UNCOMPUTABLE take
the ladder branch and skip the `exit_deadline_blind` refusal. That is the same silent-omission class
the refusal exists for, reintroduced by the fix for a different silent omission. The blind check now
runs before the ladder case, with a comment naming sdk72 C6 so the ordering is not "cleaned up"
later. Worth remembering: **the fix for one instance of this class is a normal place to create the
next one**, and only running the suite finds it.

**Also landed (§4.7 prerequisite, a live defect):** the keyless watchtower could only express a
height trigger, so a laddered coin — whose race is an EVENT (someone spends `F`) and whose
`exit_deadline_block` is legitimately `None` — was **dropped from the exported bundle entirely**.
Delegating to a third-party tower protected only un-laddered coins, silently. Now:
`WatchTrigger { watch_txid, watch_vout, csv_blocks, push_txs }`, an entry per laddered coin,
per-entry blindness (`WatchState::Acted.blind` + `any_blindness()`) so an unevaluable entry can never
report `Idle`, and 4 new tests including a non-vacuity control and old-bundle compatibility.

## 2026-08-03 — the value-conservation class (took priority over CATS-B)

A workflow launched to plan CATS-B change 2 instead found a **live theft-class defect at HEAD**,
proven by running it against the shipped verifier. Six commits:

| commit | what |
|---|---|
| `4e165e6` | child lane, both hops bound to `prev − one rung` — the proven skim |
| `a063e3f` | the extension's declared `out_value` bound to its signed output |
| `9c00140` | §4.5 correction: the census does **not** close sender-declared segment shape |
| `deed25c` | root ladder bound too — correcting my own scoping in `4e165e6` |
| `d692c07` | WHERE-it-pays + Σ-outputs, on the ancestor and leaf hops |
| `2ad2b2d` | the fee-rate **yardstick** — without it all of the above are bypassable |

**The generative rule.** `verify_tier_cosigned` binds a tier's INPUT amount and says nothing about
how that amount is split across outputs — and the SE is blind, so it signs any distribution by
design. "Genuinely co-signed" never implies "pays the right key" or "forwards the right amount".
`docs/utexo/VALUE-CONSERVATION-SWEEP.md` §1 has the four-property checklist; pinning three is worth
nothing, which finding 5 proved.

### Still open

* Sweep findings **4, 6, 7** — SSP pre-pay gates and the claim-path booking. All downstream of the
  bindings above, so re-test them against these rather than patching independently.
* **`ci-guards` has a hole this session walked into.** I wrote `hex::decode(..).unwrap_or_default()`
  inside a new security check; it compiled, always failed, and refused every honest ancestor bundle
  (sdk11/sdk17 caught it). The guard's patterns cover `.await.unwrap_or(` and deliberately EXCLUDE
  bare `.unwrap_or_default()` as "overwhelmingly Option, not Result". Here it was a Result, in a
  verifier. Widening the pattern is a change to the guard's own precision — decide it on its merits,
  do not allowlist around it.
* **CATS-B change 2** — untouched. See below; the research is done and the plan is sound.

## Queue item 5 — CATS-B, in progress

**The SPINE TIER is landed and verified** (commit after `09ec773`). `SPINE_CSV = 0` is signed by all
three split builders and admitted by the verifier as a distinct KIND with bounds `[0,0]`.

Why it is a kind and not a widened range: if `SPINE_CSV` were a legal `state` CSV, a state tier could
be un-timelocked (the [B1] shape) and a spine tier could quietly carry a real timelock — making every
payee's exit thousands of blocks slower with nothing to refuse it. `d_floor > SPINE_CSV` on every
shipped profile is asserted by `the_spine_csv_is_not_a_legal_state_csv_on_any_profile`.

The kind is chosen from `final_is_split`, which is a **code-path constant the receiver picks**
(`false` on every whole-coin path, `true` only from `verify_child_bundle`) — not a bundle field. That
matters: `[0,0]` is the loosest-looking bound in the file and would be a real hole if a sender could
elect into it.

What it bought: the split no longer consumes a state rung, so **§1.3's "a coloured carrier gets ONE
partial payment, ever" is dead** — the refusals that enforced it are gone. Per-level exit latency
2 124 → 720 blocks; mainnet depth 1 4 284 → 2 880, depth 100 4.08 → 1.41 years. The model in
`tesr_exit_wait_blocks` and the depth cap in `enforce_split_depth_cap` both read `SPINE_CSV` now:
leaving them at `state_csv(1)` would not have been "conservative", it would have been a silent
economic cap — refusing payments the chain carries fine — enforced by a stale constant.

Verified by running: unit suites green (mercuryrustlib 72 now), and a **full-suite baseline at
`5eddc0c`** — 22 distinct E2Es, all PASS against a freshly built binary:

```
sdk 1  2  11 29 30 31 32 34 36 45 58 59 69 72 74 75 76 77 78 79 81 82   +  RGB_E2E=1
```

sdk82 failed first, and the failure was worth having: it hard-coded `required_wait = 71` from
`state_csv(1)`, so after the spine landed it mined to a tip chosen for the old arithmetic, left 56
blocks against a real requirement of 53, and reported "THE DEFECT IS OPEN" when the gate had
correctly ADMITTED the coin. Re-derived from `SPINE_CSV`, not relaxed.

### Still to build in Phase 1

* **Change 2 (§4.1): the change leg gets one cap and no extension.** `in_ladder_split` builds every
  child identically, so this needs a per-child ROLE — an API change to
  `children: &mut [(Coin, String, u64)]`. Until it lands a payment still adds `[extension, state]`
  for the change as well as the piece, so the Freeze-Lemma bound of §4.0 is approached, not attained.
* **V1**: `ChildSegment::extension` becomes `Option`, which makes segment SHAPE sender-declared for
  the first time. §4.5 is explicit that this — not the CSV-0 race — is where the adversarial E2E
  budget goes. The census still closes it (a dropped tier leaves `expected` one short of `num_sigs`).
* **V2** (derive the ancestor `2` from the disclosed tier count), **V4** (spine-tip bundle key),
  **V5** (`min_spine_tip_value` on the change leg only), then change 3 (K > 1).

### Landed in P0 round 1 (uncommitted, verified by running)

* **P0-2 depth cap** — `enforce_split_depth_cap`, derived from live `TesrParams` + the SE's live
  `lockheight_init`, fail-closed if unreadable. Mainnet 3, regtest 30.
* **P0-5 exit model** — was `155d + 112` vB with `wait_blocks: 0`; now measured `293d + 375` vB and
  `2124d + 2160` blocks, derived from `TesrParams`. `auto_exit_margin_blocks` re-derived per network.
* **P0-3 journal** — `splitjrnl-<op_id>` write-ahead record; `sdk81` kills the process with a real
  SIGABRT at `UTEXO_CRASH_POINT` and replays both child ladders. Idempotent, never re-sends.
* **P0-1 gate** — `check_exit_headroom` + `sdk82`; refuses a real late-epoch conveyance with a named
  shortfall. **BUT bypassable — see queue item 1.**

### Open defects being fixed in round 2

* **B1 CRITICAL** — headroom requirement reads `TesrTier.csv`, a sender-supplied serde field nothing
  binds to the signed tx. Third instance of this shape (C-1 census, flat-conveyance classifier, now
  this). Fix: read `nSequence` from the parsed tx as `verify_bundle_ex` already does.
* **B2 CRITICAL** — floor raised in `quote_transfer` but not the executor; `fundable: true` can still
  be followed by a refusal.
* **B3 HIGH** — `transfer.rs:377` `tesr::load(..).unwrap_or(None)`; `ci-guards` RED.
* **B4 HIGH** — `journal_write` not atomic; `cosign_colored_in_ladder_split` terminalizes with no journal.
* **B5 HIGH** — six E2Es red because commit 2c351c6 flipped the default and did not reconcile them.

## Verification

```
cargo +stable check --workspace
cargo +stable test -p mercurylib -p mercuryrustlib -p mercury-utexo-sdk -p ci-guards
```

E2Es: build with plain `cargo +stable build` (**`--tests` does NOT refresh `target/debug/rust`** —
you will test a stale binary). Run each from its own scratch dir with `regtest.Settings.toml` copied
in (every flow wipes `wallet.db*` in the CWD):

```
ML_NETWORK=regtest RLN_REGTEST=~/Claude/rgb-lightning-node/regtest.sh \
COMPOSE_FILE=~/Claude/rgb-lightning-node/compose.yaml \
RLN_BITCOIND_CONTAINER=rgb-lightning-node-bitcoind-1
```

Full set: `SDK_E2E=` 1, 2, 29, 30, 31, 32, 34, 58, 59, 69, 74, 75, 76, 77, 78, 79, 81, 82; `RGB_E2E=1`.

## Rules that are not style preferences

* Never weaken or delete an assertion to make a test pass — re-derive it, or say why it no longer applies.
* Never add a `ci-guards` allowlist entry to go green. That guard exists because the
  silent-degradation class recurred four times; its first run named 11 real sites.
* Never report a test green you did not run. **Twelve subagents have claimed an unrun green; all
  twelve were caught.** Re-run anything executed against an earlier binary.
* `colored_ladder` stays `false`.
* Do not touch `enclave/`, `lockbox/`, `.github/`.

## Context worth not rediscovering

* **The census trap:** a laddered carrier still has flat backups (`tx1` co-signed at deposit,
  permanent). Passing `flat_backups = 0` bricks every coloured coin at claim.
* **≥3 rivals** in any rival-tier test, selecting a non-minimum-internal-txid rival — a 2-rival test
  passes ~50% of the time by luck.
* **E7:** one `update_witnesses` with the plain resolver silently kills a Tentative un-broadcast
  ladder, and `get_asset_balance` is blind to it. Assert with a read-only `color_psbt` stock probe.
* `sdk58`/`sdk59`/`sdk69` are blind to `PARENT_V2_BASELINE` bugs — they DEPOSIT the parent, making
  baseline 1 accidentally correct. `sdk76` splits a RECEIVED parent, which is the real test.
