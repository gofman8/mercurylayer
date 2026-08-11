# DECISIONS — the Mercury Utexo specification

Status: open record, started 2026-08-10. One entry per decision from
[`SPEC-ROADMAP.md`](SPEC-ROADMAP.md) §2. **Four of twenty-one are taken.**

Each entry states what was decided, what it *binds* (the document sections and code sites that may
no longer drift from it), and what it **pulls into scope** — because three of the first four
decisions enlarge v1 rather than shrink it, and a plan that does not say so will be wrong about its
own dates.

---

## The freeze rule — in force from this document's first commit

**No change to the frozen surface lands without the spec-section diff in the same commit.**

The frozen surface is: tier shapes and their nSequence/nLockTime rules, the protocol constants, the
verifier's refusal set, the wire format, and the receiver's admission rules.

This rule exists because the previous corpus went comprehensively stale in about two weeks — CATS-B
change 2, leaf combine, leaf renewal, `POST /transfer/cancel` and the re-anchor `Void` change all
landed inside the window its own surveys audited (`SPEC-ROADMAP.md` D0). A three-month specification
written against that rate of change does not converge. The exception path is not "ask first"; it is
"land both halves together".

**One consequence to accept deliberately:** under D2 the frozen surface is not yet stable, so the
freeze binds the *already-shipped* surface today and extends to the CATS-B additions as each lands.

---

## D0 — v1 is the FULL protocol, published as ONE document

**Decided:** a single normative specification covering the ladder *and* the surrounding protocol —
cryptographic constructions, the SE contract, deposit/onboarding, authorization, the chain model,
liveness and the watchtower obligation, cooperative exit, durability and recovery, and the interop
encodings. Not published in parts.

**Binds:** the spec's own table of contents (`SPEC-ROADMAP.md` §5). No section may be dropped to
make a date.

**Consequence, stated plainly:** nothing is externally reviewable until all of it is ready. The
roadmap's counter-argument was that publishing the ladder first buys outside review early, and that
is now foregone. The mitigation is WP10 — an adversarial pass over the *document* — which becomes
load-bearing rather than a final polish, because it is the only external-shaped review this plan
now contains.

**Also pulled in:** four subsystems still have **zero** survey passes — cryptographic constructions,
the SE (lockbox) contract, deposit/onboarding and authorization, and the confirmation/reorg half of
the chain model (`SPEC-ROADMAP.md` §4b). Under a ladder-only v1 those were out of scope. Under this
decision they are all in, and they must be surveyed before their sections can be drafted.

---

## D7 — Publish the depth-cap formula and give every profile explicit parameters

**Decided:** option (a). Publish the depth-cap formula plus its evaluation per network profile; give
testnet and signet explicit `TesrParams` with no silent fallback; reconcile the two `lockheight_init`
values to one normative profile; delete the phantom hard depth cap of 8. Treat it as a code change,
not a documentation table. `confirmation_target` joins the published table.

**Why it is first:** the epoch determines the cap, the cap determines the cost table, the cost table
determines `V_min`. D6 and D3 are sequenced behind it.

**Binds:** `lib/src/tesr.rs:219-224` (`TesrParams::for_network`), `server/Settings.toml:1-2`,
`server/src/server_config.rs:79-82`, `lib/src/transfer/receiver.rs:806-812` (`max_exit_txs` /
`max_split_depth`), `PROTOCOL.md:487-488` (the phantom cap), and the constants table in every
normative doc.

**The defect this closes:** `TesrParams::for_network` maps only `"bitcoin"`/`"mainnet"` to the
mainnet schedule. Everything else — testnet and signet included — silently receives the toy regtest
schedule of `d0 = 24` blocks (≈4 hours). The deployed profile is `network = "testnet"`,
`lockheight_init = 1000`, so **the profile actually running is the one the code calls regtest**, and
on it the derivation admits an exit chain of **139 transactions**. Round 2 found a second, independent
failure of the same combination: those 139 admit ~135 consecutive zero-CSV spine tiers, whose TRUC
relay stall (~68 blocks) **exceeds the entire regtest state schedule**. On the deployed profile the
relay-stall term dominates the timelock schedule.

---

## D1 — CTES-R is the normative RGB lane; flip the default

**Decided:** option (c), against the roadmap's recommendation of a scope-out. RGB is **in** v1 and
normative, the coloured ladder (CTES-R) is the normative lane, and `SdkConfig::colored_ladder` flips
from `false` to `true`.

**Binds:** `clients/libs/rust-sdk/src/config.rs:283` and `:312` (the default), and every normative
statement about RGB forwarding, combine, batch cardinality and unilateral exit.

**What this buys:** the thing the scope-out could not. A coloured carrier gets a real ladder, ~36
child hops instead of a structural one-hop leaf, and the **first genuine unilateral, SE-free exit of
an RGB allocation** (sdk75). `README.md:7`'s headline — "every coin stays unilaterally exitable to
L1" — becomes true rather than being narrowed to exclude carriers. The legacy lane's structural-leaf
defect (`tokens.rs:3324-3325`, unsatisfiable at *any* value of `TOKEN_PIECE_SATS`) stops being the
shipped behaviour.

**What it pulls into scope — none of this is currently scheduled, and a spec cannot be written
around any of it:**

| Prerequisite | State today |
|---|---|
| **Coloured on-chain re-anchor** | **Does not exist.** `refresh` refuses carriers (`clients/libs/rust-sdk/src/refresh.rs:150`) while `build_colored_child_retransfer` errors at the CSV floor telling the user to "re-anchor it" (`clients/libs/rust/src/tesr.rs:6370-6375`) — naming a primitive with no implementation. A coloured coin therefore has **no renewal primitive**: its life is bounded and terminates in a forced exit. This is the largest single item and it is a *design* gap, not a coding one. |
| **One payee, one payment, forever** | `CTESR_CARRIER_SEND_DEPTH = 1` (`tokens.rs:131`) plus `refuse_colored_multi_payee` (`tokens.rs:230`, `:3600`) plus the depth-1 refusal in `colored_child_txids` (`tesr.rs:467-471`). Three independent live guards. A carrier pays once, to one payee. |
| **JS and web clients cannot receive a laddered coin at all** | `clients/libs/nodejs/transfer_receive.js:184`, `clients/libs/web/transfer_receive.js:224` fail closed on any laddered coin. Flipping the default makes **every** RGB coin unreceivable on both clients until `verify_bundle` is ported to wasm/JS and Kotlin. |
| ~~**rgb-lib is an uncommitted filesystem path**~~ | **CLOSED 2026-08-11 (`6b2e662`).** Was: a relative path, with four symbols mercury called existing in no commit, so HEAD did not build from a clean clone. Now pinned by revision — `rgb-lib = { git = "https://github.com/gofman8/rgb-lib", rev = "38e344e0…" }`, `rev` not `branch` because a branch moves. Verified: `Cargo.lock` records the git source, no `path = "../../../.."` remains in any manifest, and both `-p mercury-rgb` and `-p rust` build against it. |
| **Lightning is not wired to the coloured child lane** | `tokens.rs:3292-3295` refuses it by name. Under the scope-out this vanished at zero cost; under this decision it must be built or the LN appendix must say RGB-over-LN does not work. |
| **Coloured admission economics** | The coloured floor is frozen to a hardcoded 2.0 sat/vB, and above that rate the leaf is materially under water. D5 and D3 must resolve before §10/§11 can state a `V_min`. |
| **A measured value loss** | `sdk32_token_over_time.rs:48-66` records a coloured-lane loss: a carrier idle past its own deposit horizon fails the ancestor census on an expired flat backup. The test was reordered to avoid triggering it and still prints SUCCESS. This must be fixed, not documented. |

**Explicitly retained from the roadmap's D1 analysis, because it survives the change of answer:**
a coin bearing an off-chain commitment MUST NOT be given a *plain* TES-R ladder. Today that is
enforced sender-side only (`clients/libs/rust-sdk/src/wallet.rs:866`); the receiver-side refusal in
`validate_encrypted_message` was never built, and `transfer_receiver` reads and persists
`rgb_consignment` off conveyed backups with no cross-check (`clients/libs/rust/src/transfer_receiver.rs:885`,
`:967`). WP6(i) stands.

**Interaction with D13:** the roadmap argued D13 was urgent *because* the scope-out would leave the
plain lane normative and unprotected. That specific argument no longer applies — but **D13 does not
go away.** Plain sats leaves exist regardless of RGB, the sats lane is still the bulk of the
protocol, and the near-deadline force-exit remains gated to coloured rows. The defect is unchanged;
only its rhetorical framing was RGB-dependent.

---

## D2 — Land all remaining CATS-B units, then freeze

**Decided:** option (b), against the roadmap's recommendation of freezing now. Nothing about the
split shape is declared frozen until the deferred CATS-B units land.

**Binds:** `PROTOCOL.md:254` and §5.7 (which still describe the *pre-CATS* shape — a split state at
nSequence Δ_{k+1} "undercutting by ≥36" — and are therefore wrong today), `clients/libs/rust/src/tesr.rs:5346`,
`:2476-2482`, `:4285-4291`.

**In scope, and this is the decision's real content:** V4 (key prefix), **V5 (the 820-sat spine-tip
floor)**, the **coloured spine**, K>1 batch cardinality, and the `config.rs` cost-model re-derivation.

**The upside this buys:** V5 is a receiver-side *admission* rule, not an additive feature. Freezing
§5 without it and adding it later would retract an admissibility rule and move the `V_min` table D3
publishes. This decision removes that risk entirely, and removes the two "provisional" sections the
alternative would have shipped.

**The cost:** drafting of §5, §6, §10 and §11 waits on work that has not been scoped or estimated.
Those are core sections, not peripheral ones.

**Coupling with D1 — the item on both critical paths:** the **coloured spine** is one of the
deferred CATS-B units *and* a prerequisite of the normative coloured lane. It is now the single
highest-priority engineering item in the plan, and it should be scoped first.

**A correction that survives into §6 either way:** promoting the `tesr.rs:5323-5346` doc-comment
verbatim would publish a sound timelock argument beside a **false relay claim**. `PROTOCOL.md:245-247`
("each tier confirms before the next is valid, so no long vulnerable chains") is false for exactly
the spine: `SPINE_CSV = 0` means a spine tier IS valid before its parent confirms — that is why it
exists. §6 must state instead that a spine tier trades confirm-before-valid for latency, bounded by
the exit-chain length cap plus a TRUC in-flight window of 2, and that the 1P1C argument applies to
the CSV-separated rungs (`T→X`, `X→S`, `SP→ext`) and **never** to `SP_i → SP_{i+1}`.

---

## D3–D20 — the remaining sixteen, taken 2026-08-10

All follow the roadmap's recommendation. Full context for each is in `SPEC-ROADMAP.md` §2; recorded
here is the answer, and the consequence where it is not obvious.

| # | Decided | Note |
|---|---|---|
| **D3** | Renewal and rollover are **caller-driven primitives**. Normative end-of-life rule: at `d_floor` the coin is terminal — exit unilaterally, or re-anchor cooperatively. Compaction REQUIRED at the depth cap, threshold left to implementations. Publish `V_min(d, r)` as a table over fee rate; name sub-economic finality as a limitation. | This makes `PROTOCOL.md:295` and `:348` **wrong**, not merely undocumented — they claim renewal and rollover run automatically inside `transfer()`, and there are zero call sites. WP7 deletes those claims rather than building to them. |
| **D5** | Tier rate is a **per-network constant, receiver-enforced by equality**; the tolerance band is retained for flat backups with a normative width; publish both. | **Gated on the P2A spike (WP1).** If a tier cannot be reliably fee-bumped through its anchor above 2 sat/vB, equality is unsound as stated and this decision reopens. Do not draft §12's fee clauses before the spike reports. |
| **D6** | **Re-derive the exit-cost model from measured vsizes** at depths 0..3 through the production finaliser; change the receiver admission rule; re-pin every test; regenerate all doc figures from the constant. | Sequenced **after D7**. This is not documentation regeneration — `max_exit_txs` is an admission rule, so re-deriving changes *which conveyed bundles a receiver admits*. Budget the test churn. |
| **D8** | **Enumerated trust assumption**, published alongside R1–R9: `num_sigs`, the `statechain_id ↔ aggregate` binding, mailbox availability and ordering, tip service if proxied. SE-signed counts named as the closure path. | **VERIFIED 2026-08-10 — answer is NO, and it is theft-class.** `sig_count` travels as a bare JSON integer with no signature, MAC, attestation, freshness token or id-echo at any hop. The enclave — the only trusted crypto core — never reads or signs the count; it is pure host/DB state (`lockbox/src/enclave.cpp:139-204`, `db_manager.cpp:465-474`). The coordinator does `response["sig_count"].as_u64().unwrap()` and re-emits it (`server/src/endpoints/transfer_receiver.rs:67`); the client trusts it as the census RHS with no independent cross-check (`clients/libs/rust/src/tesr.rs:9093`). A coordinator that **under-reports** by k hides k co-signed rival states — the exact-equality census balances and the receiver (or an LN SSP paying an irreversible leg) accepts a coin the sender can still reclaim. This is exactly the defect the census exists to stop, resting entirely on coordinator honesty. Closure = SE-attested counts = enclave work = **excluded scope**, so the spec STATES the assumption; it does not close it. Full write-up: [`notes/D8-SIGCOUNT-AUTH.md`](notes/D8-SIGCOUNT-AUTH.md). Runner-up theft-class row: the coordinator-computed `terminal` flag. Also schedules the `aggregate_xonly` backfill for pre-0009 rows, which nothing owned. |
| **D9** | **Freeze the current serde shape** as the normative wire format. Specify field by field; make absent-vs-null explicit and REQUIRED-to-reject wherever absence is currently free; write the ancestry disclosure-completeness rule. | The disclosure rule is not optional detail: a receiver cannot check that an ancestor split state's outputs do not exceed its funding without knowing what it is entitled to be shown. Pairs with WP3b. |
| **D10** | **Commission the analysis** in week 1 alongside the P2A spike. | Still not answered — deliberately. If the retained checks (INV-5 rejecting duplicates and inversions) already cover what blinding-commitment verification would, accepting it is honest and cheap. If not, it is an excluded-scope collision and must surface in week 1, not week 6. |
| **D11** | **Fix the encodings before freezing.** Add bech32/checksum and a version tag to the invoice; decide explicitly whether coin type 0 on testnet/signet is deliberate or a defect. | Freezing an unchecksummed, unversioned payment request in a v1 spec cannot be walked back, and field-typo losses in the wild are the predictable result. Also in WP6b: `decode_transfer_address` **panics** on a short-but-valid Bech32m payload. |
| **D12** | **Retire the absolute deadline once `T` confirms** — and mandate the handoff to `defend_ladders` **in the same clause**, or the walk strands mid-chain. Publish the δ budget. | Mandating a deferral *policy* is implementation; mandating the *bound* is not. Without the published budget an implementer reads `δ = 36` as slack and batches broadcasts into it. |
| **D13** | **Port the near-deadline defence to plain leaves** and specify it as REQUIRED of a conformant wallet. | **Re-priced by evidence, see below.** No longer the "port of existing, tested code" the roadmap assumed. |
| **D14** | **Relational margin law**: `sup.csv ≥ live_csv + δ`, receiver-enforced. Schedule-grid membership named as v2, with the `SPINE_CSV` exception recorded as the reason it is not free. | Additive-safe: a later strengthening to the grid form retracts nothing. Implementation is WP3c. |
| **D15** | **Claim-time finality** with the current windows, plus the outstanding sender obligation. `key_updated`-tied lock lifetime named as v2. | Deciding it is what stops the spec either omitting payment finality entirely or accidentally committing to "payments are reversible for one hour". |
| **D16** | **MUST-reject unknown-higher `protocol_version`** (fails closed; needs a coordinated flag day) and **4 is the floor** — delete the legacy v3 branch. | (d) is the surviving theft half of audit C-14, and it changes what a child **is** — which is why §Children cannot be written without it. Also in scope: the uniffi FFI silently strips `protocol_version`, `tesr_ladder` and `child_tesr_bundle` in both directions, so "which client profiles are conformant" is part of this decision. |
| **D17** | **Wall-clock windows published as configuration** with the failure mode named (adequate for a non-normative appendix); enforce `latch_expiry < CLTV deadline` before LN is ever normative. Terminalization carve-out decided separately as WP11 (1–2 days). | See D21 below — D1's answer changed this one's surroundings. |
| **D18** | **Stable `code` field alongside `message`.** Renumber the ERR-n taxonomy against it; re-point the tests. | Size it *with* the test churn, not as a schema edit — E2E tests match on error substrings today, which makes the prose a de-facto interface. An independent implementation cannot conform to prose. |
| **D19** | **Full normative watchtower obligation**: a margin formula, a maximum polling interval expressed as a fraction of the smallest CSV in the bundle, a REQUIRED re-export trigger set plus a freshness field, and a third reporting state (tolerable-and-pending) distinct from failure. Write down what a keyless tower **cannot** do as normative properties. | Answers §4's warning that an implementation built from a ladder-only spec could be structurally correct and still lose funds. The named mechanism is the missing margin and cadence. |
| **D20** | **Both**: a capability/feature-discovery mechanism (a distinct `protocol_version` plus a feature list on `/info/config`) **and** a normative statement that coins minted under an older coordinator retain their original semantics for life, per-column. | Also: state that the published `version` is informational and MUST NOT be used for compatibility decisions until it is split. |

### D13 has been re-priced by evidence, and the roadmap's estimate no longer holds

The roadmap called option (a) "a port of existing, tested code". A safety probe run before implementing
found it is not. Removing the `is_colored()` gate at `clients/libs/rust-sdk/src/wallet.rs:1886-1894`
would have created a **theft path**, not a fee waste:

- `child_in_ladder_split` is a **plain-only** lane (`clients/libs/rust/src/tesr.rs:5935`;
  `refuse_uncolored_over_colored_child` at `:571-581` refuses coloured children outright), so the
  coloured loop shipping today has **never been exposed to it**.
- Unlike `child_retransfer` and `combine`, that lane does **not** park the coin's status before its
  irreversible co-signature. A terminalized leaf therefore reads CONFIRMED with a stale `ctesr-` row
  from the CSP co-signature until conveyance completes — and **permanently** after any error or
  crash in that span (`resume_split_conveyance` repairs only the conveyed pieces, never the
  terminalized parent).
- The ported loop would force-exit it and broadcast `state_child`, rivalling CSP and **destroying
  grandchild pieces already conveyed to payees**.

Required with the port, therefore: a split-journal guard (zero extra I/O — the loop already holds
every row, and the journal is written at `Planned` *before* the co-signature, so it is durable
evidence available before anything can be handed out).

Two further defects the same probe found, **live in shipped code today**, independent of the port:

1. **`head_start` is short by one block per tier.** `wallet.rs:1904` sums `Σ csv`; the claim-time
   admission gate this defence exists to honour uses `exit_wait_blocks` = `Σ (csv + 1)`
   (`lib/src/transfer/receiver.rs:720-722`) — each tier's timelock *plus* the block its parent needs
   to confirm. The current expression also `filter_map`s away any tier whose csv is `None` entirely.
   The two sites decide the same quantity two ways and the watchtower errs **fail-open**.
2. **The loop computes a deadline from DECLARED timelocks**, which `child_exit_chain`'s own doc
   forbids: *"Anything that computes a requirement, a deadline or a cap from these timelocks must use
   `child_exit_chain_bound`"* (`clients/libs/rust/src/tesr.rs:5489-5493`).

## D22 — SE scope rule REVOKED (2026-08-11): `lockbox/` and `enclave/` are in scope

**Decided:** the standing "never modify `enclave/` or `lockbox/`" constraint is lifted, for both.
The owner's words: *"do whatever to fix all issues, if it requires changes in enclave, why not?"*

**Why it matters immediately: D8(a) stops being unclosable.** Every prior write-up — this document
included — recorded the theft-class unauthenticated census count as something the spec must *state*
rather than *close*, on the sole ground that closure needed SE work. That ground is gone.

**What changes across this record:**

| Previously | Now |
|---|---|
| D8: spec states `num_sigs` as an enumerated trust assumption; closure is excluded scope | Closure is IN scope — see D8-CLOSE below. The enumeration is still published (it is good practice), but (a) becomes a *closed* row rather than a standing assumption |
| D10: "may be a third excluded-scope collision", commissioned to surface in week 1 | No longer a collision at all. It is ordinary work, costed on its merits |
| The census counter machine (`{level, m, k}` not served — PROTOCOL §5.5/§5.11) | SE-side per-level counters were the "build it" option D4 rejected partly on scope. Re-openable if wanted, though D4's proof means it is no longer *needed* |
| SSP-side expiry gates (C-7 residual) | Buildable |

**D8-CLOSE — the shape, since it is smaller than "change the enclave" sounds.** The lockbox already
holds a key and the client **already fetches and trusts `enclave_public_key`** for a different
binding (`validate_tx0_output_pubkey`, `clients/libs/rust/src/transfer_receiver.rs:1313`). So:
lockbox signs `(statechain_id, sig_count)` at `lockbox/src/db_manager.cpp:487` /
`lockbox/src/server.cpp:357`; the coordinator forwards the signature alongside the count; the client
verifies it against a pubkey it already possesses before using the count as the census RHS
(`clients/libs/rust/src/tesr.rs:9093`). A signature over two fields on one endpoint — not an
architecture change. Add a replay/freshness binding so a stale `(sid, count)` pair cannot be
re-served.

**Practical:** `lockbox/` is plain C++ (Crow, monocypher) with **no SGX** in its build files —
ordinary Docker container, locally buildable and testable. **Production runs lockbox, not the SGX
`enclave/App` lane**, so lockbox changes ship and enclave changes matter only if SGX is revived;
prefer lockbox where both would work.

**Unchanged:** the deployment items the owner set aside separately (port 18080 unauthenticated; the
committed SE seed and Vault token) are NOT reopened by this. Lifting a code-scope rule is not a
reversal of those.

---

## D23 — BACKWARD COMPATIBILITY IS NOT A CONSTRAINT (2026-08-11)

**Decided.** The owner, asked about legacy coins, the attestation rollout and the testnet schedule,
answered each the same way: *"I don't care about backward compatibility at this stage… optimise
everything for efficiency of testing and bringing us closer to the spec."*

This is a standing rule, not three separate answers. **Do not design phased rollouts, compatibility
shims, or migration paths for existing data.** Where a choice is between "correct" and "does not
break what is already deployed", take correct. The deployed stack is a testnet development
environment; its coins are expendable and can be wiped.

**What it changes, concretely:**

| Was going to be | Now |
|---|---|
| D8-CLOSE staged rollout: accept-then-require | **Require the attestation outright.** No flag, no phase 2, no "record when missing" |
| Legacy pre-0009 coins: re-binding endpoint vs accept | **Neither — ignore them.** No code (D24) |
| testnet keeps the toy schedule to avoid breaking deployed ladders | **testnet gets the mainnet schedule** (D25) |
| `for_network`'s silent regtest fallback preserved "so no caller changes" | Fallback can go; an unrecognised network should fail |

**What it does NOT change:** the *protocol's* own compatibility rules — `protocol_version` floors
and ceilings (D16), and what a conformant implementation must accept — are a spec question about
independent implementations, not about this deployment's history. D23 is about not carrying our own
past; it is not licence to make the specification version-blind.

---

## D24 — Legacy pre-0009 coins: ignore them, no code

**Decided** under D23. The `aggregate_xonly` backfill that `tesr.rs:8063-8066`, `SPEC-ROADMAP` WP4
and D8 all call "the only complete fix" **cannot run** — measured: 0 of 8,155 NULL rows are
backfillable, because migration 0009 added `user_public_key` and `aggregate_xonly` in one statement
so the rows that lack one lack both, and the coordinator stores no chain data to recover the
aggregate from (`notes/AGGREGATE-XONLY-BACKFILL.md`).

**The fact that decided it: new coins are never affected.** Every post-0009 deposit records both
values, which is why the populated counts are identical (3,793 / 3,793). This is a legacy-data
artefact of a test deployment, not a protocol defect.

**Actions:** none on the coins. Two documentation consequences:
* `SPEC-ROADMAP` WP4's gate "Zero NULL `aggregate_xonly` rows" is **unachievable as written** and is
  restated as "zero NULL among post-0009 rows" — otherwise WP4 can never close.
* The gate at `clients/libs/rust/src/tesr.rs:8049-8100` that keeps such a coin un-laddered stays. It
  is correct: it refuses to mint an unclaimable ladder. It should NOT be advertised as making the
  coin transferable.

---

## D25 — testnet runs the MAINNET schedule; regtest stays fast

**Decided** under D23, resolving the tension in D7 between test speed and spec fidelity by giving
each network the schedule that suits its purpose:

| network | schedule | why |
|---|---|---|
| `bitcoin` / `mainnet` | mainnet (`d0 = 1440`) | — |
| `testnet` / `testnet3` / `testnet4` / `signet` | **mainnet** | Real ~10-minute blocks, so the toy `d0 = 24` was ~4 hours of real time. Public test networks should exercise **the schedule that ships**. |
| `regtest` | test-scale (`d0 = 24`) | Blocks mine on demand; this is where E2E speed comes from, and it is preserved exactly |
| anything else | **refused** | A typo used to fall through to the toy schedule — `"mainet"` would have produced a 4-hour ladder on real money |

**This is a deliberate compatibility break**, permitted by D23: a receiver derives its accepted CSV
band from its own compiled-in preset, so ladders built by the old build against the deployed testnet
coordinator will be refused. Those coins are expendable.

**It also fixes what D7 called the sharper problem.** The deployed profile is `network = "testnet"`
(`server/Settings.toml`), which silently ran the regtest schedule and therefore admitted a
**139-transaction exit chain** — ~135 consecutive zero-CSV spine tiers, whose TRUC relay stall
(~68 blocks at two in flight) exceeds the entire toy state schedule
(`notes/WP1-TRUC-P2A-SPIKE.md`). On the mainnet schedule that combination does not arise.

---

## D26 — The SP fee law AND relay-awareness (both halves)

**Decided:** fix `#134` at both ends, not just the cheap one.

The attack needs two gaps together: `SP` carries **no fee law** (`clients/libs/rust/src/tesr.rs:9590-9601`
— the code's own comment: "the ONE tier in the whole structure with neither a Σ-payload law nor a
payload-count law"), and supersession is decided by a **maturity race that presumes relayability**
(`verify_superseded_segment`, `:8418-8433`, kills a rival only on `sup.csv > live_csv`). Co-sign an
`SP` at ~1 sat over 211 vB and the victims' lower-CSV tiers are unrelayable by construction, losing a
race the verifier believes they win.

**Why both.** Binding the fee alone closes *this* instance. It leaves the deeper defect intact: the
supersession argument would still assume something it never checks, so any future path to an
unrelayable tier reopens it. That is the same shape as D13 — a defence resting on an unstated
assumption — and it is worth closing at the level of the assumption.

**Risk to respect while implementing:** `SP` is the tier that hosts the level below, so its outputs
fund every child. A naive Σ constraint could refuse honest spines that legitimately spread value
across many payload outputs. The sibling tier one block up already carries both laws, so the shape is
precedented — mirror it rather than inventing one, and keep every honest-spine test green.

### Both halves are now IMPLEMENTED (2026-08-11)

**Half 1 — the Σ-payload law on the intermediate `SP`** landed first, closing the live instance.

**Half 2 — relay-awareness** landed as `#135`, and it is the one that mattered for the future. The
supersession race now reads a `LiveRival { csv, relayable, implied_fee, vsize }` instead of a bare
CSV, and refuses by name when the live tier cannot be broadcast:

> *superseded state 0 is disclosed as beaten by the live tier over the same outpoint, but that live
> tier pays N sats over M vB — under the 1 sat/vB relay floor, so it cannot be broadcast and cannot
> win any race.*

Three details worth keeping:

* **One constructor, three segment kinds.** Root ladder, ancestor segment and child segment all build
  their live map through `LiveRival::read`. Three hand-rolled fee computations would be three chances
  to disagree, and the one that got it wrong would report an unrelayable tier as relayable — exactly
  the hole being closed. The fee comes from the parsed transaction and the prevout map, never from a
  declared `out_value`, which is attacker-supplied on a conveyed bundle.
* **The P2A anchor is deliberately NOT counted.** Package relay via `submitpackage` would rescue an
  underpaying tier, but this tree has no `submitpackage` caller yet (its own open item), so crediting
  the anchor would credit a rescue nobody can perform. The check is conservative in exactly one
  direction: it may refuse a bundle a future build could broadcast, which costs a retry, not a coin.
* **It is redundant on honest ladders, and that is the point.** The value laws already hold every
  live tier at the committed rate (2 sat/vB, twice the floor), so the whole suite stayed green when
  the check landed — 589 tests. The redundancy is what converts "a future tier without a value law
  silently reopens the race" into "it fails here, by name".

---

## D27 — `initlock`/`interval` are compiled in; the coordinator's copy is a cross-check (closes D8(f))

**Decided and IMPLEMENTED 2026-08-11**, unblocked by D25's per-network-constant pattern.

`interval` (`lh_decrement`) is the yardstick INV-5 measures every flat-backup hop against — the rule
that each hop decrements by EXACTLY `interval`, which is what stops a sender padding the backup
vector with duplicates to inflate `flat_backups` and absorb a hidden co-signed state. It used to
arrive from the coordinator's `/info/config`. **That let the coordinator define the defence.**

**The tempting fix — derive it from the conveyed chain — is circular, and that is why D8(f) stayed
open.** A padded chain with uniform `I/2` decrements derives `I/2` and validates against itself,
accepting exactly the padding INV-5 exists to stop. The value has to come from somewhere neither the
sender nor the coordinator chooses.

| network | initlock / interval | hops |
|---|---|---|
| `bitcoin` / `mainnet` | 10 000 / 100 | 100 |
| `testnet` / `testnet3` / `testnet4` / `signet` | **10 000 / 100** (mainnet, per D25) | 100 |
| `regtest` | 1 000 / 10 | 100 |
| anything else | **refused** | — |

`TesrParams::flat_ladder_params` is the single source of truth, and **all three consumers now derive
from it** rather than transcribing it: the client refuses any coordinator that disagrees, the
coordinator **refuses to boot** if its own env disagrees, and the SDK's `auto_exit_margin_blocks`
derivation reads the interval through a `const fn` instead of the two literals it used to carry.

**The numbers are the code defaults and the documented 100-hop capacity, not a new choice** — the
mainnet figure is what `server/src/server_config.rs` already defaulted to and what
`clients/libs/rust-sdk/src/config.rs` already calls the fail-closed bound on audit [17]. Making the
value authoritative rather than advisory is the change; re-tuning the margin is not, and would be a
separate decision.

**Writing it exposed that three of the four config sources in the repo disagreed** — with the table
and with each other: `docker-compose-main.yml` at 50000/6, `docker-compose-test.yml` at 1100/1, and
`server/Settings.toml` declaring `testnet` at the regtest 1000/10. Only the running regtest stack
happened to match, which is why no E2E ever caught it. All four are now aligned and pinned by
`ci-guards/tests/deny_flat_ladder_config_drift.rs`, which parses the real files.

**Verified live, both directions**: the running coordinator (1000/10) is accepted as `regtest`
(`clients/libs/rust/tests/live_info_config.rs`), and a coordinator configured one block off refuses
to start with both numbers in the message.

**The cost, stated plainly:** a config typo is no longer a quiet protocol weakening — it is a
fleet-wide outage, because every client refuses that coordinator. That is the intended trade, and it
is why the boot check and the CI guard exist: to make the typo fail at build and boot rather than at
the first transfer.

---

## Newly open — created by D1

**D21 — RGB over Lightning: build it, or exclude it by name?** Under the roadmap's recommended RGB
scope-out this disappeared at zero cost: coloured LN is already refused by name on the surviving lane
(`clients/libs/rust-sdk/src/tokens.rs:3292-3295`), non-exact RGB PAY is refused (`ssp.rs:1093`), and
RGB receive has no remote-SSP path (`ssp.rs:1174-1183`). D1 chose CTES-R as normative instead, so
RGB-over-LN must now either be built or stated as an explicit exclusion in the Lightning appendix.
Not decidable until WP11 closes D17's terminalization carve-out.

## Re-costing

## Re-costing

The roadmap's 12–16 weeks described a full protocol v1 with RGB and Lightning as non-normative
appendices and the CATS-B shape frozen as-is. D0, D1 and D2 together describe a larger deliverable.
The estimate is **withdrawn pending a re-cost**, which needs the coloured-spine and coloured-re-anchor
items scoped first — those are design work of unknown depth, and a number produced before they are
scoped would be a guess presented as a plan.
