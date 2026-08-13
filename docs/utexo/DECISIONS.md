# DECISIONS — the Mercury Utexo specification

Status: open record, started 2026-08-10. It began as one entry per decision from
[`SPEC-ROADMAP.md`](SPEC-ROADMAP.md) §2, of which four were taken on day one (D0, D7, D1, D2 — the
order is deliberate, not numeric). **All twenty-one roadmap decisions D0–D20 are now taken, and the
record continues past them to D40**: everything from D21 on is a decision the roadmap did not
contain, forced by the answers given to the ones that did. **Twenty of the twenty-one have an entry
here; D4 does not, and that is deliberate** — it was answered in round 2 in
[`SPEC-ROADMAP.md`](SPEC-ROADMAP.md) §D4 ("*the proof HOLDS … D4 collapses to a deletion plus a
restatement*") and merged there into D8 ("*D4 and D8 are therefore one decision, not two*"), so its
residual is carried by the D8 row below rather than by an entry of its own. D4 appears in this file
only where that merge shows through, as the rejected `{level, m, k}` counter machine.

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
| ~~**Coloured on-chain re-anchor**~~ | **CLOSED 2026-08-12 (`b79b525`).** Was: `refresh` refused carriers outright while `build_colored_child_retransfer` errored at the CSV floor telling the user to "re-anchor it" — naming a primitive with no implementation, so a coloured coin had **no renewal primitive** and its life was bounded, ending in a forced exit. D29 chose the design (CR-D, a coloured de-trigger) and it is now built: `build_colored_detrigger` + `cosign_colored_detrigger` (`clients/libs/rust/src/tesr.rs`) under `TierRole::Detrigger`, driven by `UtexoWallet::colored_reanchor` (`clients/libs/rust-sdk/src/refresh.rs`). The carrier refusal no longer refuses — it **dispatches**: a coloured carrier is pointed at `colored_reanchor`, a plain one keeps the old refusal, and each arm names why. Both directions are load-bearing: sending a coloured carrier through the RGB-unaware `withdraw` burns the asset, and sending a plain one to CR-D finds no coloured material to build from. |
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
| **D8** | **Enumerated trust assumption**, published alongside R1–R9: `num_sigs`, the `statechain_id ↔ aggregate` binding, mailbox availability and ordering, tip service if proxied. SE-signed counts named as the closure path. | **VERIFIED 2026-08-10 — answer is NO, and it is theft-class.** `sig_count` travels as a bare JSON integer with no signature, MAC, attestation, freshness token or id-echo at any hop. The enclave — the only trusted crypto core — never reads or signs the count; it is pure host/DB state (`lockbox/src/enclave.cpp:139-204`, `db_manager.cpp:465-474`). The coordinator does `response["sig_count"].as_u64().unwrap()` and re-emits it (`server/src/endpoints/transfer_receiver.rs:67`); the client trusts it as the census RHS with no independent cross-check (`clients/libs/rust/src/tesr.rs:9093`). A coordinator that **under-reports** by k hides k co-signed rival states — the exact-equality census balances and the receiver (or an LN SSP paying an irreversible leg) accepts a coin the sender can still reclaim. This is exactly the defect the census exists to stop, resting entirely on coordinator honesty. **The "excluded scope, so state it rather than close it" verdict recorded here was overtaken the next day and is retained only as the diagnosis that motivated the fix** — D22 lifted the SE scope rule, and the count is now CLOSED on the Rust lane, not merely enumerated: `get_statechain_info` (`clients/libs/rust/src/utils.rs`) sends a per-request random 32-byte nonce and **refuses** any response lacking a `utexo/sig_count/v2` signature over `(statechain_id, num_sigs, sig_budget, nonce)`, verified by `verify_sig_count_attestation` against the **chain-anchored** `enclave_public_key` — not against the served attestation key, which would accept a coordinator signing with a key of its own. `has_sig_budget: None` is refused rather than defaulted. See D22 (D8-CLOSE), D28 (the budget ratchet) and D40.2 (consuming it at the acceptance decision). **Both JS clients still compare `statechainInfo.num_sigs` unattested** (`nodejs/transfer_receive.js`, `web/transfer_receive.js`) — that half is open and is stated as a named limitation by D39.3. Full write-up: [`notes/D8-SIGCOUNT-AUTH.md`](notes/D8-SIGCOUNT-AUTH.md), which carries the same superseded verdict. Runner-up theft-class row: the coordinator-computed `terminal` flag — closed by D40.2. Also schedules the `aggregate_xonly` backfill for pre-0009 rows, which nothing owned. |
| **D9** | **Freeze the current serde shape** as the normative wire format. Specify field by field; make absent-vs-null explicit and REQUIRED-to-reject wherever absence is currently free; write the ancestry disclosure-completeness rule. | The disclosure rule is not optional detail: a receiver cannot check that an ancestor split state's outputs do not exceed its funding without knowing what it is entitled to be shown. Pairs with WP3b. |
| **D10** | **Commission the analysis** in week 1 alongside the P2A spike. | Still not answered — deliberately. If the retained checks (INV-5 rejecting duplicates and inversions) already cover what blinding-commitment verification would, accepting it is honest and cheap. If not, it is an excluded-scope collision and must surface in week 1, not week 6. |
| **D11** | **Fix the encodings before freezing.** Add bech32/checksum and a version tag to the invoice; decide explicitly whether coin type 0 on testnet/signet is deliberate or a defect. | Freezing an unchecksummed, unversioned payment request in a v1 spec cannot be walked back, and field-typo losses in the wild are the predictable result. Also in WP6b: `decode_transfer_address` **panics** on a short-but-valid Bech32m payload. |
| **D12** | **Retire the absolute deadline once `T` confirms** — and mandate the handoff to `defend_ladders` **in the same clause**, or the walk strands mid-chain. Publish the δ budget. | Mandating a deferral *policy* is implementation; mandating the *bound* is not. Without the published budget an implementer reads `δ = 36` as slack and batches broadcasts into it. |
| **D13** | **Port the near-deadline defence to plain leaves** and specify it as REQUIRED of a conformant wallet. | **Re-priced by evidence, see below.** No longer the "port of existing, tested code" the roadmap assumed. |
| **D14** | **Relational margin law**: `sup.csv ≥ live_csv + δ`, receiver-enforced. Schedule-grid membership named as v2, with the `SPINE_CSV` exception recorded as the reason it is not free. | Additive-safe: a later strengthening to the grid form retracts nothing. **Superseded twice, and both are downstream in this document.** The single-δ form written here is replaced by D38's per-kind law (`δ` for a state, `δE` for an extension), because on regtest `δE = 3` and `δ = 6`, so one δ breaks every honest renewal. And it is no longer scheduled work: it **landed at `77807e1`** — `RivalKind::margin` in `clients/libs/rust/src/tesr.rs` refuses `sup.csv < live.csv + margin`, with the kind taken structurally from the LIVE rival by position parity at all five `LiveRival::read` sites, plus a preset census asserting `d_floor ≥ δ` and `e_floor ≥ δE` and a CI guard (`ci-guards/tests/deny_sender_declared_margin.rs`). |
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
* **The P2A anchor is deliberately NOT counted.** `LiveRival::read` takes the fee as
  `value_in − value_out` off the parsed transaction, and the anchor is one of those outputs, so its
  240 sats are never credited to the tier's rate. The reason given when this landed — "this tree has
  no `submitpackage` caller yet" — **expired with D31/#123**, which built one (`core_rpc.rs`). The
  exclusion is still right, for a better reason: a package rescue needs a `BumpCapability` the holder
  supplies, holding an owner-funded UTXO and a signer, and **a verifier cannot assume the party it is
  judging has one** — its absence is precisely the keyless case. Crediting the anchor would credit a
  rescue the bundle's holder may be unable to perform. The check stays conservative in exactly one
  direction: it may refuse a bundle a funded holder could broadcast, which costs a retry, not a coin.
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

## D28 — Terminality is SE-enforced and SE-attested; the budget is a RATCHET (closes D8(i))

**Decided and IMPLEMENTED 2026-08-11.**

A split/combine node is *terminal* when its spend budget is exhausted, and a receiver's census
argument for a child rests on that: the parent must never be co-signable again. The budget lived only
in the coordinator's Postgres, enforced only by the coordinator — so the sole witness to terminality
was the party the receiver is being protected from. D8 had made the *count* SE-attested; the SE had no
notion of a budget, so it could not attest that the count was **final**.

**What shipped.**

| | before | now |
|---|---|---|
| where the budget lives | coordinator Postgres | coordinator **and** enclave |
| who refuses the co-sign | coordinator | coordinator **and** enclave (`410`, before the secnonce is consumed) |
| what a receiver can check | the coordinator's word | a BIP-340 signature by the coin's chain-anchored key |

> **This row was true about the ATTESTATION and false about the SYSTEM until A.1 landed** (`52daca6`,
> 2026-08-13). The budget was signed, verified, and then read by no acceptance decision — a repo-wide
> grep for `has_sig_budget` in `clients/` returned one site, the verification itself. "What a receiver
> can check" is only a security property once a receiver checks it. `attested_terminal` now derives
> terminality from the attested payload and demotes the coordinator's answer to a refusing
> cross-check; `terminal_from_attested` / `cross_check_terminality` are exercised by four tests
> (`tesr::malicious_coordinator_terminality_tests`) including disagreement in BOTH directions.

**The ratchet is the security content.** `set_sig_budget` may create a budget or **lower** it, never
raise it — enforced in one SQL statement (`WHERE sig_budget IS NULL OR sig_budget > $1`) so the
database decides it rather than a read-then-write that two concurrent callers could assemble a raise
out of. Without it an attestation saying "1 of 1, terminal" would be true when issued and worthless a
block later, and terminality would not be a property at all. A raise is refused with `409` and a
message naming both values; setting the same budget again is idempotent.

**The attestation is `utexo/sig_count/v2`,** carrying count and budget under ONE signature:

```
sha256("utexo/sig_count/v2" || statechain_id || u32_be(count)
       || u8(has_budget) || u32_be(budget) || nonce32)
```

Two separate attestations were the alternative and are worse: they could be mixed across time, a
fresh count paired with a stale budget — exactly the confusion an attestation exists to remove.
`has_budget` is an explicit byte because **"no budget" (co-signable indefinitely) and "budget 0"
(terminal) are opposite facts** that must not share an encoding. v1 is gone rather than retained
(D23): leaving it would leave a way to obtain an attestation that says nothing about terminality.

**The coordinator now fails if the mirror does not land.** Its own write succeeding while the enclave
disagrees produces a coin the receiver's verification will refuse — reporting success there would hand
the caller a coin nobody can claim.

**Found while deploying, and worth keeping:** the schema lived in a `CREATE TABLE IF NOT EXISTS`
inside the first-deposit path, so a new column never reached an existing database and the deployed
lockbox answered `column "sig_budget" does not exist`. Schema setup now runs **once at boot**
(`db_manager::ensure_schema`) and is fatal on failure. A migration behind a lazy code path is a
migration that has not run.

**Verified live**, against the rebuilt lockbox: budget absent → `has_sig_budget:false`; set 5 → ok;
lower to 2 → ok; **raise to 9 → 409 refused**; re-set 2 → idempotent; attestation then reports
`terminal:true` and verifies in the Rust verifier at exactly `(count=2, budget=Some(2))` and at no
neighbouring pair; a co-sign attempt on that coin → **410**, while a no-budget coin passes the gate.

---

## D29 — The coloured re-anchor is **CR-D, a coloured de-trigger** (closes the CR-A/B/C spike)

**Decided 2026-08-11** on the evidence in `COLOURED-SPINE-REANCHOR-SCOPE.md` §2.

The question as originally posed — "which of CR-A/CR-B/CR-C, or an SE-assisted variant?" — had a
false premise. D1 assumed the coloured re-anchor means "colour the refresh transaction", and that is
**not buildable**: `refresh` routes through `withdraw`, an RGB-unaware builder that refuses carriers
one level up. The three-design menu was also not exhaustive.

**CR-D — a coloured DE-TRIGGER — needs no RGB rival over `F` at all.** Two transactions, zero CSV
wait, no SE change, and it reuses a role tag that is already allocated (`TierRole::Detrigger = 0x06`).
It dominates CR-B on every axis and removes most of CR-C's motivation.

**Consequence for the programme: G2 no longer selects the product.** It was scoped as the experiment
that would choose between designs; with CR-D available it decides only whether CR-A's
one-transaction form is *also* available. Build CR-D; run G2 as an optimisation question, not a gate.

**What changed under this decision since the spike was written:** its "single largest risk" was
BLOCKER 1, a live theft path in the plain spine. That is closed — the Σ-payload law on `SP` and the
relay-aware supersession race (D26, both halves), each with an attack test. **BLOCKER 2 (W1, the
fee-bump workstream) is now the largest**, and its open question is priced in
`notes/CPFP-WHO-FUNDS-IT.md`.

*(Both have since closed. W1's open question — who funds the child — was answered by D31, and the
path was then built: `core_rpc.rs`, `build_p2a_fee_child`, and `watch_pass_with_bump` /
`exit_pass_with_bump`. CR-D itself landed at `b79b525`. What this decision still leaves open is the
optimisation question it names above — whether CR-A's one-transaction form is also available.)*

---

## D30 — `colored_ladder` stays `false` until CR-D and the client ports land (D1's flip, re-gated)

**Verified 2026-08-11**, against code rather than against the plan.

D1 decided the coloured ladder is the normative RGB lane and that
`SdkConfig::colored_ladder` flips `false → true`. **The flip is not takeable yet**, and each of D1's
own three prerequisites was re-checked today and is still absent:

| prerequisite | state today |
|---|---|
| a coloured on-chain re-anchor | ~~not built~~ — **BUILT 2026-08-12 (`b79b525`)**, after this table was written. `build_colored_detrigger` + `cosign_colored_detrigger` implement D29's CR-D, and `colored_reanchor` drives them; the carrier check now dispatches to it instead of refusing. A coloured coin has a renewal primitive. |
| JS and web clients receiving a laddered coin | **still fail closed** (`nodejs/transfer_receive.js`, `web/transfer_receive.js`): neither can run `verify_bundle`, and both refuse rather than fall through to the flat census. That is still the shipped behaviour and it is still the correct refusal — but it is **no longer a prerequisite**: D33 established there is no external consumer of `clients/libs/web`, and the product SDK (`clients/libs/nodejs-utexo`) is a JSON-lines client over the Rust daemon that was never affected. This row is **satisfied**, per the amendment recorded at the end of D33. |
| Lightning on the coloured child lane | **refused by name** (`refuse_colored_multi_payee`, and the depth-1 cap). |

**As written on 2026-08-11 that read: flipping today would ship coins that cannot be renewed, cannot
be received by two of the four clients, and cannot use Lightning.** Two of those three have since
gone: CR-D shipped (`b79b525`) and the client row was closed at zero by D33. **What remains of this
gate is the coloured spine wiring and Lightning on the coloured child lane** — the `ColoredLatch::None`
refusal in the coloured-child arm of `colored_transfer` (`clients/libs/rust-sdk/src/tokens.rs`):
"a Lightning-latched colored transfer is not yet wired to the CTES-R child lane" — which D21 decided
to build. The decision was never reversed — it was sequenced, and the sequence has largely run.

**What DID land in the meantime**, so the gate is smaller than it was: the plain-spine theft that
BLOCKER 1 named is closed and exercised (D26 both halves), terminality is SE-attested (D28), the flat
ladder is no longer coordinator-defined (D27), and the `claim()` laddering hole that would have
destroyed an RGB allocation on a materialised carrier is closed (RGB Stage 0).

---

## D31 — Fee-bumping is **owner-funded** (A); the **funded tower** (B) is a documented deployment option

**Decided by the owner 2026-08-11**, on `notes/CPFP-WHO-FUNDS-IT.md`. Options (C) third-party fee
service and (D) raising `committed_fee_rate` are **not** taken.

### What was actually decided

**(A) is the normative v1 statement.** The protocol does not promise that anyone bumps a stuck tier
for you. The party who funds a CPFP package is the **coin owner**, out of their own wallet, because
they are the only party that has both a UTXO and a reason to spend it.

**(B) is offered, not assumed.** A tower operator MAY run a funded variant with a small hot fee
wallet. The spec describes it so the choice is informed, and states its exposure precisely.

### Why this is the honest answer rather than the comfortable one

It does not close the gap — **it renames it**. The guarantee becomes "the owner is online during a
fee spike", and being offline during a fee spike is precisely the case a watchtower exists for. That
is a real limit and it is now written down as one, rather than left to be discovered by an
implementer who reasonably assumed the tower had it covered.

The alternative that *looks* like it closes the gap, (D), does not either: any fixed
`committed_fee_rate` is exceeded by some future mempool, so it shrinks the exposure window instead of
removing it — and it is paid by **every tier of every coin, permanently, signed in, whether or not
that coin is ever broadcast**, while also raising `V_min` against two still-open decisions. Trading a
priced cost for an unpriced one to make a gap close neatly is how a spec acquires claims it cannot
keep.

### The measured facts this rests on (WP1)

**Read the units before quoting these.** The WP1 run was made on an isolated node with the floor
forced to **3 sat/vB** (`-minrelaytxfee=0.00003`), above the protocol's committed 2.0, on a
**synthetic 141-vB** transaction — not on a TES-R tier, which is **125 vB** (`TIER_VBYTES` in
`lib/src/tesr.rs`, derived byte for byte in its doc comment and pinned against a production-finalised
transaction by `the_uncoloured_fee_matches_a_measured_signed_tier`) and pays **250** at the committed
rate.

| | measured, and against what |
|---|---|
| a tier alone, under the relay floor | `min relay fee not met, 200 < 423` — **refused**. `423 = 141 vB × 3 sat/vB`; the 200 is 1.42 sat/vB. **A real tier under the same forced floor reads `250 < 375`.** The refusal is the protocol property; these two numbers are the lab's. |
| the same tier as a 1P1C package | `"package_msg": "success"` — this is the load-bearing result, and it is rate-independent |
| child fee paid in the run | **180 330 sats**, against a **754-sat requirement** (318 vB package × 3 sat/vB = 954, less the parent's 200). A ~239× overpay **chosen by the funding wallet, not a derived cost**: WP1 §2's own log puts the child at **177 vB**, i.e. **~1 019 sat/vB** on the child alone, lifting the package to the recorded 5.68 sat/vB against a 3 sat/vB floor. *(A `maxfeerate` clause stood here and was wrong: WP1's `maxfeerate` trip belongs to a **different transaction** — the separately-built literal-`51024e73` broadcast in "What this run did NOT establish" item 1. §2's package records no such trip, and at ~1 019 sat/vB it is far under Core's 10 000 sat/vB default, so it could not have tripped. The "chosen, not derived" reading rests on the 754-vs-180 330 arithmetic alone, which is enough.)* |

**The "~900×" ratio that used to sit here was wrong, and it is quoted in other documents.** Its
source (`notes/CPFP-WHO-FUNDS-IT.md`) computes 180,330 / **200** — the ratio to the *parent's fee* —
and this record re-based it onto the 240-sat **anchor**, which gives 751×, while also borrowing the
240 from a *different* run (§2's parent carried a placeholder output; the literal `51024e73` anchor
was exercised separately). **And 180,330 is not a cost this repo's fee law imposes — it is a target a
caller asks for.** `build_p2a_fee_child` (`lib/src/wallet/p2a_fee_child.rs`) derives the whole figure
from the rate handed to it — `required_total = ceil(target_fee_rate × package_vsize)`, less the
parent's already-committed fee — and **nothing bounds that rate** beyond `is_finite() && > 0.0`, so
the number *is* reachable: over the 278-vB package formed by a 125-vB tier and this builder's fixed
**153-vB** child (`estimate_child_vsize`, measured 153 estimated / 153 actual on regtest), a
180,330-sat child fee is a target of **~650 sat/vB**, ~325× the committed 2.0. An earlier form of
this paragraph called the figure "not derivable at any legal shape" on the strength of
`TRUC_MAX_CHILD_VSIZE = 1_000`; that constant caps **vsize, not rate**, and all it establishes is a
floor — no legal v3 child can be larger than 1 000 vB, so that fee is at least 180.33 sat/vB on the
child however the child is shaped. The supportable claim is the weaker and sufficient one: a fee of
this size is a **choice of target**, not a number the fee law hands you.

**What the corrected numbers do not change is the decision.** A rescue costs a real outlay by
whoever performs it, and it returns a 240-sat anchor — `CHILD_CHANGE_DUST = 330`
(`lib/src/wallet/p2a_fee_child.rs`) means the child's change must clear 330, so recovering the anchor
never pays for itself. The P2A output is anyone-can-spend, but "anyone *can*" is not "someone
*will*": there is no fee revenue in it for a stranger, so the only parties with an incentive are the
owner and a tower the owner pays.

### Normative consequences (this is the part that changes documents, per D19)

1. **`PROTOCOL.md` must state what a keyless tower CANNOT do**, so that an implementer reading only
   the spec cannot conclude otherwise. A keyless tower can watch, and can broadcast pre-signed tiers
   at their committed fee. It **cannot** fee-bump, because a CPFP child needs an input it does not
   have and a signature it cannot make.
2. **§5.13's amendment is re-scoped.** It currently reads as though towers *do* hold a fee wallet;
   under (A) that is the OPTIONAL variant, and the default tower is keyless and cannot bump.
3. **The funded variant's exposure is bounded and must be stated as such:** a funded tower still
   holds **no coin keys**, so compromising it loses the operator's fee float and cannot touch a
   user's coin. That is a materially smaller claim than "keyless" and materially larger than nothing.
   It also carries an operational duty — the float must be refilled, and **a tower that runs dry
   fails at exactly the moment it is needed**, which is the same failure as (A) with extra steps.

### What this unblocks and what it does not

`#123` (a `submitpackage`-capable broadcast path) got an answer to *who calls it* — the owner's
wallet — and **it has since been built.** The infrastructure objection recorded here (**electrum has
no `submitpackage`**, so any bumping path needs a Bitcoin Core RPC endpoint the SDK does not have,
WP1 §4) is **no longer true of the tree**: `clients/libs/rust/src/core_rpc.rs` is a minimal Core RPC
route existing for exactly that one call, `mercurylib::wallet::p2a_fee_child::build_p2a_fee_child`
builds the 2-input child (input 0 the anchor with an empty witness, input 1 the owner's funding
UTXO), and `watch_pass_with_bump` / `exit_pass_with_bump` wire it into both broadcast loops.

**The shape it landed in is D31 itself, not a general capability.** `BumpCapability` is an explicit
optional argument rather than ambient config, so `watch_pass(electrum, bundle)` keeps its signature
and its keyless meaning, and **its absence is the keyless case** — a tower cannot acquire the ability
to bump by accident. `SdkConfig::fee_bump` is `Option<FeeBumpConfig>` and is `None` in every
constructor unless the operator sets it (`clients/libs/rust-sdk/src/config.rs`). That config holds
the Core RPC route (url, user, password), a `target_fee_rate` and a `reserve_bumps_per_coin`
alongside `funding_secret_key_hex` — and what that key is, the field's own doc comment states:
**"A fee key, never a coin key, and that separation is the whole of D31's bounded exposure"**,
compromise costing the float and not a user's coin. That is the bounded-exposure clause of
consequence 3 above, held as a type rather than as prose.
The electrum limitation stands as a *fact*; what is gone is the claim that nothing in this repo can
call `submitpackage`.

---

## D32 — What `colored_ladder = true` silently REMOVES, inventoried before §10 is drafted (P5)

**Recorded 2026-08-11.** D30 sequenced the flip behind CR-D and two client ports. This is the other
half of that decision: the flip is not purely additive, and the capabilities it takes away have never
been written down anywhere a reader of the spec would find them.

Two capabilities disappear the moment the coloured lane becomes normative, because both live in the
legacy coloured-SPLIT lane that CTES-R retires:

| Capability | Where it lives today | What replaces it |
|---|---|---|
| **Coloured multi-carrier combine** — paying one invoice from N carriers at once | `create_colored_combine_tx`, reached through the multi-carrier combine | **Nothing.** The coloured in-ladder split is single-parent; N carriers must be spent as N payments |
| **Coloured leaf consolidation** — merging dust-sized coloured leaves back into one carrier | same lane | **Nothing.** Coloured leaves accumulate, and `PARTIAL-PAYMENT-ECONOMICS.md` already caps a carrier at one coloured partial payment |

**Why this belongs in the record rather than in a release note.** Both feed `V_min` — the smallest
economically viable coin — which D3 is still settling. Losing consolidation means coloured dust is
permanent, so the floor that makes a piece worth creating rises; and losing multi-carrier combine
means a payment larger than any single carrier needs N separate payments, each with its own exit
cost. A `V_min` derived without these terms would be optimistic in a direction users feel.

**They are also the reason the MIGRATION HATCH exists** (`SdkConfig::colored_ladder` docs): retiring
the legacy lane outright would strand every carrier CTES-R cannot serve, so
`tokens::migration_hatch_verdict` keeps the RGB-aware legacy lane open for exactly that class. The
hatch bounds the damage; it does not restore either capability.

**No code change.** This is an inventory item: it must appear in `DECISIONS.md` before §10 of the
spec is drafted, so §10 states the losses rather than a reader discovering them.

---

## D21 — RGB over Lightning: **BUILD IT** (owner, 2026-08-11)

The question the scope doc carried unanswered since it was written, and the reason **P3** sat in the
middle of the estimate as a coin-flip. Decided: **build it.** RGB assets traverse Lightning in v1.

**What that actually costs**, from `COLOURED-SPINE-REANCHOR-SCOPE.md` P3 — the plumbing is the easy
half and is already there (`convey_child_bundle(.., batch_id)`; the plain lane already passes it).
The real work is two things:

1. **An SSP pre-pay RGB gate over a coloured child's off-chain witness chain.** Today
   `validate_pending_token` resolves only `branch_txs` + `BackupTx.rgb_consignment`, so an SSP asked
   to pay against a coloured child cannot verify the allocation it is paying for. Without this the
   operator pays first and discovers second — the shape P0-2 and C-9 already fixed on the other
   lanes.
2. **An F7 journal for the coloured lane**, so a latched coloured send resumes after a crash rather
   than stranding a payment mid-latch.

**Consequence for the spec:** §P3's "or exclude it by name" branch is gone. The document must now
describe RGB-over-Lightning as a supported capability, which means its trust statement (what an SSP
verifies before paying) has to be written rather than deferred.

---

## D33 — `clients/libs/web` has **no external consumer**: leave it fail-closed (owner, 2026-08-11)

**P4's unbounded tail is closed at zero.** The scope doc found no in-repo consumer, and I re-verified
that today: the only references are its own test harness (`clients/tests/web/package.json`). The owner
confirms there is no out-of-repo consumer either.

**So no client port is required for the `colored_ladder` flip.** The two legacy clients
(`clients/libs/nodejs`, `clients/libs/web`) stay **fail-closed** on a laddered coin, which is the
correct behaviour and already shipped: neither can run `verify_bundle`, and refusing is right —
falling through to the flat census would let a padded ladder pass.

This matters more than a schedule line. The *product* SDK (`clients/libs/nodejs-utexo`) is a
JSON-lines client over the Rust daemon and was never affected, so **the flip's client story is
already complete**: every client that can receive a coin either verifies the ladder properly (Rust,
and the daemon-backed JS SDK) or refuses it by name.

**D30 is amended accordingly:** of its three prerequisites, the client port is now **satisfied**, and
the remaining gate on `colored_ladder = true` is the coloured spine + CR-D wiring alone. *(Update:
the CR-D half of that landed at `b79b525` on 2026-08-12 — see the D30 table above. The gate is the
coloured spine wiring, plus the coloured LN lane D21 committed to building.)*

---

## Newly open — created by D1

**This section is ANSWERED, and kept because it records what D1 opened.** D21 was decided by the
owner on 2026-08-11 — **build it** — in the entry above. Read the question below as the statement of
what D1 created, not as a live item.

**D21 — RGB over Lightning: build it, or exclude it by name?** Under the roadmap's recommended RGB
scope-out this disappeared at zero cost: coloured LN is already refused by name on the surviving lane
(`clients/libs/rust-sdk/src/tokens.rs:3292-3295`), non-exact RGB PAY is refused (`ssp.rs:1093`), and
RGB receive has no remote-SSP path (`ssp.rs:1174-1183`). D1 chose CTES-R as normative instead, so
RGB-over-LN must now either be built or stated as an explicit exclusion in the Lightning appendix.
Not decidable until WP11 closes D17's terminalization carve-out.

## Re-costing

The roadmap's 12–16 weeks described a full protocol v1 with RGB and Lightning as non-normative
appendices and the CATS-B shape frozen as-is. D0, D1 and D2 together describe a larger deliverable.
The estimate is **withdrawn pending a re-cost**, which needs the coloured-spine and coloured-re-anchor
items scoped first — those are design work of unknown depth, and a number produced before they are
scoped would be a guess presented as a plan.

*(Of the two, the coloured re-anchor is no longer unscoped: D29 chose CR-D and it shipped at
`b79b525`. The coloured spine is still the item the re-cost waits on.)*

---

## D34 — The receiver PROVES the enclave holds the share it is about to depend on

> **The heading used to end "(closes CO-1)". It does not, and [D40.2](#d402--co-1--o-1--co-3-are-one-defect-consume-what-is-attested-close-the-js-lane-name-the-real-closure)
> is why.** D34 closes the ROGUE-KEY half — a coordinator that lies about `aggregate_pubkey` no
> longer passes its own check. CO-1's residue is structural and proof of possession cannot touch it:
> the enclave key material and the counter it attests are held by the party the receiver is being
> protected from, so a proof made with that key proves possession to exactly the extent that key is
> independent — which is the question. D40.2 records the real closure and the one-row merge.

**Decided:** fix, by proof of possession at claim time.

The soundness hunt found that `aggregate_pubkey`, `x1_pub` and `server_public_key` are plain
coordinator Postgres columns served over `/info/statechain` and `get_new_key_info`, and that nothing
anywhere proves the enclave knows the discrete log of the share the receiver is about to depend on.
`verify_bundle_bound` tests **the coordinator's own column** against the chain key — so a coordinator
that lies about the column passes its own check. Combined with a colluding sender (who chooses
`user_public_key`, enabling the rogue-key decomposition `U := D − E_sid`), this mints an
attacker-controlled output that passes the entire acceptance path.

This is NOT the sender-alone threat model, which is genuinely blocked. It is the coordinator+sender
one, and the corpus already concedes that *no adversarial test in the repo models a malicious
coordinator — every one models a malicious sender*.

**The rule.** At claim, after the keyupdate commits, the receiver sends a fresh 32-byte nonce `ρ`;
the enclave signs `(domain-tag ‖ sid ‖ ρ)` with its **new** share `e′` and returns `(P, σ)`. The
receiver REQUIRES that `σ` verifies under `P` **and** that `o2·G + P` equals the taproot internal key
of `F` read from the chain. `o2` is fresh per transfer and unknown to the sender, so a forger must
sign under `A_D − O2` without knowing a discrete log.

One check collapses the whole substitution class at once — wrong `aggregate_xonly`, wrong `x1_pub`,
wrong `server_public_key`. It needs a new enclave endpoint; that cost is accepted.

**Consequence for the spec.** §7 and §17 may not claim receiver admission soundness against the
operator trust domain until this ships. Until then the honest statement is that admission rests on
the coordinator being truthful about the aggregate.

## D35 — On a COLOURED bundle every flat backup MUST be plain (closes RGB-1; corrects P1)

**Decided:** the permitted backup shape is a function of the DECLARED LANE, not the union of two
lanes' shapes.

RGB-1 (HIGH): a flat backup may legally carry an RGB transition and **nothing binds its assignment**.
`validate_backup_chain_v2` checks signature, sequence, locktime shape, reconstruction and INV-5 only;
`verify_if_locktime_is_reasonable_tx_version_and_output_size` admits `op_return_outputs <= 1`;
`reconstruct_transaction` copies `tx_n.output` verbatim so it constrains nothing; and
`verify_colored_shape` iterates `bundle.exit_tiers()` only. So the sender's hop-backup is an
undetectable allocation-theft primitive, and it also undermines the economic-security argument that a
prior owner's spend of `F` is unprofitable griefing.

**The rule.** Where `is_colored()`, every conveyed flat backup MUST be plain: refuse by name any
backup carrying an OP_RETURN, in the predicate that already runs over every flat backup, and make
that refusal part of the R′ acceptance set alongside `verify_colored_shape`. One predicate, fails
closed.

**The permissive shape STAYS for the un-laddered carrier lane**, where a coloured backup is the
legitimate exit material.

**This corrects P1, which is a DESIGN defect and not a code defect.** P1 as implemented refuses *a
plain ladder carrying an RGB consignment* — the inverse of the sound rule — and therefore breaks the
uncolourable carrier: `sdk78` shows a real 250-unit spend that does not land at the far end. The live
failure and the design hunt found the same boundary from opposite sides. Under the authority order
(design normative, code follows) the rule is corrected here first; the code follows.

**[Landed 2026-08-13.] The code now follows.** `verify_flat_backup_lane`
(`clients/libs/rust/src/tesr.rs`) states the lane rule from the structure it reads — `is_colored()`
off the bundle, `is_op_return()` off every backup transaction — and both R′ acceptance paths run it
(the claim path and `prepay_flat_census`, the one that authorises an irreversible Lightning leg). The
union-keyed predicate is DELETED, not bypassed. Normative text: PROTOCOL.md §5.10 rule 6, plus the
correction to §5.8's griefing-is-losing premise, which was stated for a lane where voiding the tree
yields the attacker nothing.

Two things worth keeping. **The old refusal message asserted "PLAIN TES-R ladder" while never reading
`is_colored()`** — it told the user the lane it had not checked, which is the same
description-versus-construction shape as the S3/S4 holes. And the P1 correction was verified the only
way it could be: `sdk78` failed at *"a spend that does not land at the far end is not a spend"* with
bob's piece confirmed COLOURED three log lines earlier, and passes now with carol booking all 250
units. 5 unit tests + `ci-guards/tests/deny_colored_backup_on_a_colored_ladder.rs` (negative-tested:
2 of 4 assertions go red on a mutated construction).

## D36 — Publish the REAL maintenance cadence; retract INV-27 as written (T-1/T-2)

**Decided:** correct the claim.

The real on-chain maintenance cadence is set by the **flat ladder**, not by the CSV hop budget, and
**INV-27 is false for every RECEIVED laddered coin** — the absolute flat-backup ladder is retained
under TES-R.

**MEASURED, 2026-08-13 (`sdk86`).** Both clocks, on one coin, across two whole-coin hops:
`L0 = 1389` at deposit (tip 389 + `initlock` 1000), `L1 = 1379` after one hop, `L2 = 1369` after
two — exactly `interval` per hop, with no block mined for the hop itself. Meanwhile 300 idle blocks
left the received coin's exit chain byte-identical with `F` unspent, and took 300 blocks of its
calendar. So the regtest shape affords **100 whole-coin hops** before the calendar is gone (mainnet
likewise: 10 000 / 100). SPEC.md's five INV-27 statements now carry this scope, and the invariant's
evidence pin moved from `sdk30` (a) — which idles a **k=0 deposit** and structurally cannot witness
the hop cost — to `sdk86`. The advertised footprint economics omits the binding constraint, and T-4 adds that the
split window is consumed 100 blocks per whole-coin hop as well as one per block, is unpublished, and
is invisible to every client.

The headline figure gets worse. That is the correct trade: a specification whose central economic
claim a reviewer can disprove in an afternoon has no authority, and every other claim in it inherits
the doubt.

**Also required (T-4).** Compute the sender's depth cap from the REMAINING window rather than the
constant — replace `let epoch_blocks = info.initlock;` with `epoch_deadline_from_flat_backups(parent)
− tip`, so the builder and the payee's `check_exit_headroom` measure the same quantity, and run it
BEFORE `set_spend_budget` terminalizes the parent. This is the same unification already performed for
`head_start`.

## D37 — The named-limitations section is built by an explicit classification pass

**Decided:** a second pass, forcing every residual into exactly one of three buckets.

The soundness hunt returned **zero** accepted residuals while its own frame lists **33** known ones;
every survivor was classified as fixable. That is not credible, and it is the one gap that would make
a soundness claim promotional rather than honest.

Each of the 33 known residuals plus the 11 surviving flaws is classified as exactly one of:
**mitigated** (say how), **bounded-and-accepted** (state the bound), or **genuinely open** (state the
exposure). The result is the specification's named-limitations section.

**A design is sound when its failure modes are enumerated and survivable, not when it has none.**

## D38 — The six remaining D3–D20 rows, RE-DERIVED design-first (D6, D9, D14, D16, D18, D20)

**These six were left open because their recorded recommendations are DESCRIPTIVE** — each ends in
some form of "specify what the code does". Under the authority order the specification states the
design **as it should be**, and where design and code disagree the CODE changes. So the descriptive
recommendations are not usable as answers, and each is re-derived below from the adversary model and
the security goals. Where the re-derivation lands on the same place the description did, that is
recorded as a result rather than assumed; where it does not, the code follows.

Backward compatibility is not a constraint (D23), which removes the "wire break" objection that had
been doing most of the work in three of these six.

---

### D6 — ONE exit-cost model, shape-aware, read by every consumer

**Question.** How does a party decide that a coin's unilateral exit fits inside the epoch it
inherits, and what number does the design publish?

**What the goals force.** The deciding quantity is the exit walk's total relative wait,
`Σ(csv_i + 1)` over the tiers **actually in the chain being conveyed**. A model that is not the
actual shape errs in one of two directions and both are defects: under-counting admits a coin whose
exit cannot finish before `min(L_k)` (safety), over-counting refuses honest coins and starts
defensive exits earlier than they are needed (liveness, and on-chain footprint the design's "idle
coins pay 0 vB" claim is measured against). **"Conservative" is not a defence when the goal is
liveness** — it is the same error with a friendlier sign.

**Decided.** There is exactly ONE exit-cost function. It is shape-aware, it reads the signed
`nSequence` rather than assuming a schedule, and every consumer reads that one — the receiver's
admission gate, the watchtower's auto-exit margin, and the published economics alike. **Two models
of one quantity is the defect, independent of which direction the second one errs.**

*Code consequence.* The admission gate already satisfies this (`exit_wait_blocks` = `Σ(csv+1)`,
`max_exit_txs` splices each level's real `SplitLevelShape`). The published model does not:
`tesr_exit_txs`/`_vbytes`/`_wait_blocks` assume the two-tier shape at every level (`3 + 2d`) where a
CATS-B spine level is one tier (`d + 4`), and they feed `auto_exit_margin_blocks_for` and the
invalidation economics. Make them shape-aware or delete them and route their consumers through the
admission function. Measured figures the spec publishes from it: mainnet depth cap **10** (not the
phantom 8 and not 19); depth-10 = **9,383 blocks ≈ 65.2 days, 23 transactions**.

*What NOT to do.* Do not delete the pre-CATS baseline rows in `PARTIAL-PAYMENT-ECONOMICS.md` — they
are the "before" half of a labelled before/after. Label them; deleting them makes the multi-year exit
read as current.

---

### D9 — On the wire, absence is a refusal; there is no default

**Question.** What is the acceptance discipline for a conveyed message?

**What the goals force.** The message body is **unauthenticated**. `verify_transfer_signature`'s
preimage is `(tx0_txid, tx0_vout, new_user_pubkey)` and nothing else — no site in `lib/`, `clients/`
or `server/` ever signs the body. So every field arrives from the adversary, and each accepted value
must be either (i) **derived** from an authenticated source (the chain, the SE, the receiver's own
record), or (ii) **refused**. `#[serde(default)]` on a wire struct is a third category that the
adversary model has no room for: it converts the sender's *omission* into a positive claim the
receiver then asserts on their behalf. An unrecognised field is the same failure in the other
direction — a payload the receiver does not understand, accepted anyway.

**Decided.** Wire structs are `deny_unknown_fields`. No `#[serde(default)]` on any wire struct: an
absent field is a typed refusal naming the field, unless the design states a derivation for it, in
which case the derivation is normative and the field is not read at all. **Local persistence records
are not wire** — `SpineTipBundle` and `SplitJournalChild` keep their defaults and the spec says so
explicitly, or an independent implementer builds to twelve fields that never cross.

The three frozen payloads are `TransferMsg`, `TesrBundle`, `ChildTesrBundle`.

**And the ancestry disclosure rule is TWO rules, not one.** The branch lane refuses on a COUNT
(Σ structural inputs, each named and proven terminal at the SE); the child lane has no count at all
(root-anchored to on-chain `F`, `ancestor_facts` length equality, contiguous walk, per-segment SE
facts). Merging them in the spec produces a rule neither lane implements.

*Code consequence.* Five `serde(default)` fields on `TransferMsg` and thirteen across the three wire
structs become required-or-derived; `deny_unknown_fields` appears zero times in the tree today (once,
as a comment) and must appear on all three.

---

### D14 — The supersession margin is the propagation budget, and the kind is STRUCTURAL

**Question.** How much CSV separation must a disclosed superseded tier carry over the live one?

**What the goals force.** The race is decided by relay and confirmation, not by arithmetic. The
design already carries a name for the propagation budget — `δ` for states, `δE` for extensions — so
any separation smaller than that budget is a margin the design itself says can be crossed. The
shipped relation is strict inequality (`sup.csv > live.csv`), i.e. **one block**, which is not a
budget. And a margin selected by a field the sender fills in is not a margin at all: the adversary
chooses which branch they are judged on.

**Decided.** A superseded **state** must satisfy `sup.csv ≥ live.csv + δ`; a superseded **extension**
`sup.csv ≥ live.csv + δE`. **The kind is taken from the LIVE rival, structurally** (position parity),
never from which conveyed list the superseded tier arrived in. Presets must satisfy `d_floor ≥ δ` and
`e_floor ≥ δE`, checked by the preset census, or the law refuses honest bundles on some future
schedule.

**Why the per-kind form and not a single δ.** An honest extension renewal produces exactly `δE` of
separation by construction (`renew` supersedes into `superseded_extensions` while the replacement
spends the same parent outpoint). On regtest `δE = 3` and `δ = 6`, so a single-δ law breaks **every**
honest regtest renewal. Mainnet is immune only because `δ = δE = 36` — the same coincidence that hid
the structural-kind hole.

*Code consequence — **DONE, at `77807e1`**.* Written as future work ("`LiveRival` carries no kind;
the fix adds one at its two construction points"), it has landed, and larger than scoped: `RivalKind`
is a field on `LiveRival`, supplied by the caller at **five** `LiveRival::read` sites — never parsed
out of a conveyed field — from position parity in the tier vector, so an odd index is an extension.
`RivalKind::margin` returns `δ` or `δE` from the preset, the relational check refuses
`sup.csv < live.csv + margin` with its own named error ("*a one-block lead is not a margin; it is the
rounding error a reorg or a slow relay erases*"), the preset census asserts `d_floor ≥ δ` and
`e_floor ≥ δE` (`clients/libs/rust/src/tesr.rs`), and `ci-guards/tests/deny_sender_declared_margin.rs`
pins both the structural keying and the census. `DECISIONS.md`'s D14 row (the single-δ form) is
superseded by this. Schedule-grid membership
stays v2 — `SPINE_CSV = 0` is outside any grid by construction.

---

### D16 — `protocol_version` is a message-SHAPE selector; ordering it is a category error

**Question.** What does the version tag mean, and what must a receiver do with a value it does not
know?

**What the goals force.** A tag exists so a receiver can refuse a payload it does not understand.
Comparing it with `≥` asserts the opposite: that an unknown FUTURE value is safely processed by
TODAY's rules. Nothing establishes that, and the three values in play are not generations of one
shape — `0` is the un-laddered carrier lane, `2` is a root-ladder conveyance, `4` is a child
conveyance carrying key handover. They are three different message shapes that happen to be numbered.
**The code already shows the seam**: `MIN_PREPAY_CHILD_PROTOCOL_VERSION = 3` is a floor over a set
that contains no 3.

**Decided.** The admissible set is exact: `{0, 2, 4}`. Anything else is refused by name. The numeric
ordering carries no meaning and the spec says so in terms, so that no implementer reads a floor as a
compatibility promise. The v3 dispatch arm is deleted — it branches on a shape nothing emits, and it
early-returns before the `/transfer/receiver` key handover.

*Code consequence.* Replace the `<`/`≥` floors with exact-set membership at the nine receive-path
sites; delete `transfer_receiver.rs`'s v3 arm; the child gate becomes "shape 4", not "≥ 3". The
uniffi FFI that silently strips `protocol_version`, `tesr_ladder` and `child_tesr_bundle` then fails
CLOSED (a stripped tag is not in the set) instead of downgrading silently — which is the whole point
of exact-set dispatch and is why it is not merely a stylistic tightening.

*Scoping note for §7.* "No ceiling exists" is true of the **Rust receive path only**. Both JS clients
refuse everything `≥ 2`, so they are ceiling-conformant by accident while refusing the entire laddered
space. The justification for the ceiling is that there are no independent implementations to
coordinate with and the two in-repo non-Rust receivers already fail closed — **not** D23, which
explicitly carves `protocol_version` floors and ceilings OUT of its scope.

---

### D18 — ONE error vocabulary, and it is the wire `code`

**Question.** Is the error code part of the conformance surface?

**What the goals force.** It already is, in fact: three specs' conformance tables target `ERR-n`, and
four client profiles across five sites branch on the wire string. A surface with two parallel
vocabularies — `ERR-n` in prose, PascalCase on the wire — is not a surface, because a conformant
implementation cannot tell which one it is being tested against. Prose is not an interface.

**Decided.** There is ONE normative vocabulary and it is the **wire `code` field**. `ERR-n` becomes an
index INTO it, not a parallel taxonomy, and the traceability tables point at codes. The wire strings
are pinned explicitly (`#[serde(rename = "…")]`) so that a future Rust identifier rename cannot move
the wire value — the identifier and the interface stop being the same thing. Casing stays PascalCase:
compatibility is not a constraint, so this is a free choice, and PascalCase is what four profiles
already parse. Changing it buys nothing and costs five sites.

**A missing variant is a conformance failure, not a binding detail.** The generated Kotlin bindings
enumerate only two of the three variants — `TransferCancelledError` is absent — so a Kotlin client
meeting the shipped 410 cannot deserialize the body at all, and the one variant whose job is to stop
a cancelled payment looking like an idle mailbox is invisible on that profile. Regenerating the
bindings is part of this decision, not downstream of it.

*Correction to the evidence.* `ERR-11` is NOT missing — it is defined at `SPEC.md:765` under §13 and
normatively referenced three times. §11's list skips it because the entry lives with its requirement.

---

### D20 — Capability discovery is by required-field PRESENCE on the artefact being relied on

**Question.** How does a client establish that the coordinator can do the thing it is about to depend
on?

**What the goals force.** A feature list, an advertised version, or any other self-description is a
**claim by the party being defended against**. The coordinator is in the adversary model; its
statements about itself are not evidence. The only sound evidence is the artefact the client is about
to rely on, checked at the point of use.

**Decided.** Capability discovery is by REQUIRED-FIELD PRESENCE on that artefact, checked where it is
used, refusing by name. A client MUST NOT infer a coordinator capability from any advertised
identifier or feature list. Extension is by additive OPTIONAL request parameters and additive
OPTIONAL response fields. **An optional field's absence MUST be a typed cause established from
positive evidence**: a consumer MUST NOT read "could not ask" or "no record" as "the coordinator says
absent", and MUST NOT default absence to a permissive value.

Two worked examples belong in the spec verbatim, because each is a mistake that actually shipped:
`has_sig_budget` `None`-vs-`Some(false)`, and the three-way split where only a *positively reported*
absence licenses the flat lane.

`/info/config`'s `version` is informational and MUST NOT drive any compatibility decision;
`batchtimeout` is advisory; `initlock`/`interval` are a CROSS-CHECK the client refuses to proceed past
on mismatch — they are compiled in (D27), never sourced from the wire. Leaving them sourced would
reopen the exact hole D27 closed, since `interval` is the yardstick INV-5 measures every flat-backup
hop against.

*What must NOT be written.* "Pre-0009 coins are permanently un-ladderable and REFUSED" — unqualified,
that strands 8,155 live rows the code conveys fine on the flat lane under
`PermanentLicence::LegacyNoAggregate`. Un-ladderable, yes; unconveyable, no.

---

**Five of the six require code changes.** They are design decisions, and under the authority order the
code follows: D6 (one shape-aware exit-cost model), D9 (`deny_unknown_fields`, no wire defaults), D14
(per-kind structural margin + preset census), D16 (exact-set dispatch, delete the v3 arm), D18 (pinned
wire codes + regenerated Kotlin bindings). D20 is the one the code already implements — recorded as a
verified result, not assumed.

## D39 — Owner calls, batch 1 of the residual classification (2026-08-13)

Four of the eighteen owner judgement calls the classification pass surfaced. Each is recorded with
the bound that was attached when it was put, so a later reader can see what was accepted rather than
only what was chosen.

### D39.1 — Sub-economic finality: publish `r*(V,d)`, keep the fixed floor

**Decided:** no code. `min_child_value` stays a constant, and the named limitation is the
**`r*(V,d)` table plus the sentence that the aggregate is uncounted**.

*The bound, as accepted.* `V_min(d,r)` is a closed form the tree already carries, so the failure is
not unbounded — it is **unevaluated**. The function exists, the inputs exist, nobody calls it.
`min_child_value = (committed_fee(2.0) + P2A)·2 + dust = 1,310` is `V_min` at the hardcoded 2.0
sat/vB and correct at **no other rate**: `r* = 2.01` for a floor-value piece, and a depth-1
10,000-sat piece goes under water somewhere between 5 and 10 sat/vB. ~~What is genuinely unbounded is
**aggregate** exposure.~~ **Corrected by D40.3 below, and the correction matters because "unbounded"
is the word a reviewer checks.** The aggregate is **constructible and UNEVALUATED, not unbounded**:
each piece's loss is capped by its own `V < V_min`, so the loss set
`Σ V_i · 1[V_i < V_min(d_i, r)]` is monotone in `r` and **saturates** at the piece lane's total
value. What survives from the sentence above is the real defect — nothing counts how many
sub-economic pieces exist, and the marginal cost of voiding each further one is zero. Also read `r`
as the **mempool floor sustained across the whole walk**, not a fee estimate.

*What the payee wears.* Anyone holding a piece below `V_min(d, r_live)` loses it entire, to the party
who split it, with no malice required. On the coloured lane it is worse: the allocation is not
recovered by the splitter, it is **destroyed**.

### D39.2 — The coloured lane's sats-vs-asset mismatch: name it; issuers size carriers

**Decided:** policy, no code. The specification states the mismatch and the destruction-not-capture
asymmetry explicitly, and carrier sizing becomes an **issuer obligation**.

*The bound, as accepted.* There is none, and the sentence appears in no document today.
`TOKEN_PIECE_SATS = 3,066` is the coloured root floor at 2× head-room and is **independent of the
asset carried**; `colored_child_floor` and `colored_spine_tip_floor` are pure fee arithmetic. So an
allocation's security is denominated in its carrier's SATS while the loss is denominated in ASSET
value, and the two are unrelated.

*Why this one is not covered by G8's attacker-gain bound.* On the plain lane a voided piece's sats
are recovered by the sender, so "the attacker gains at most X" describes the victim's exposure. On the
coloured lane the allocation is closed by an RGB-unaware witness and **destroyed** — nobody acquires
what the payee loses — so no attacker-gain bound describes it at all. Trigger: any prior owner of an
ancestor broadcasting their retained flat rung over `F`, ~224 sat at 2 sat/vB, voiding the tree in
ONE transaction with zero marginal cost per additional piece.

### D39.3 — Plaintext transport and the JS client gap: name both, ship as-is

**Decided:** no code. Both halves become named limitations.

*The bound, as accepted.* Absent for the transport half — the delay figures (`δ = 36`,
`epoch_expiry_height`) bound the DELAY, not the loss. Shipping defaults are plaintext on every hop
(`http://` SE, `tcp://` electrum, `rpc://` RGB proxy, plaintext SSP) and `tor_proxy` covers the SE's
HTTP only.

*The part the limitation must state, because it is not what a reader would assume.* **D27 and
D8-CLOSE are Rust-SDK-only.** Both shipped JS clients take `initlock` and `interval` verbatim from
`/info/config` with no cross-check and pass that `interval` straight into `validateSignatureScheme`,
and compare `statechainInfo.num_sigs` unattested. So on **two of three shipped clients the wire still
defines INV-5** — precisely the tamper D27 exists to remove — and the census RHS is unauthenticated.
There is also a receive-side denial primitive: an `estimate_fee` the attacker controls makes
`verify_transaction_signature` refuse every honest backup chain by `FeeTooLow`/`FeeTooHigh`.

This means the published trust model must say, in terms, that two of three shipped clients do not
enforce an invariant the specification declares normative. That is the cost of this call and it is
accepted knowingly.

### D39.4 — Mailbox availability and censorship: SURVEY IT NOW

**Decided:** survey it before §7 is written. It is the one adversary surface with **no round of
analysis behind it**, and it sits in the section whose credibility IS the adversary model.

*What the classification pass already corrected, and the survey must not re-break.* The attribution.
Coordinator-alone reduces to **denial**: a cancel still requires a single-use, endpoint-bound Schnorr
signature under the SENDER's auth key, which the coordinator cannot forge, and the re-conveyance to a
second payee is the sender's act with the sender as beneficiary. **The loss arm requires coordinator
+ sender collusion — the same adversary as CO-1.**

## D40 — Owner calls, batch 2: the four that block WRITING (2026-08-13)

Taken against the adversarially-verified decision sheet
([`spec-work/OWNER-DECISION-SHEET.md`](spec-work/OWNER-DECISION-SHEET.md)), in which **all fourteen**
first-pass answers came back `NEEDS_CORRECTION`. Where the refutation moved the answer, that is
recorded here rather than in the sheet alone, because the correction is the part worth keeping.

### D40.1 — B1: publish it corrected, and ship the one remedy the adversary cannot decline

**Decided:** state B1 as **unbounded and custodial-equivalent against a retaining operator**, delete
the four false mitigations, and surface **"sever from `F`"** — broadcast the already-co-signed `T`
(125 vB, `lock_time 0`, no SE, no counterparty, no wait) — as a first-class user action.

*What the refutation killed.* The re-anchor defence recorded in the first pass. `refresh`/`reanchor`
is **cooperative** — its own doc says so — and it routes through `withdraw`, needing one fresh SE
co-signature. **The B1 adversary IS the operator.** The recommendation was "ask the attacker to sign
the transaction that destroys its own attack", and refusal is already blessed as non-theft. A
value threshold θ fails for two more reasons: it caps a *coin* while a victim holds *n* of them, and
the field it gates on (`amount`) is **chosen by the sender**, so a hostile sender conveys a large
payment as many sub-θ pieces.

*What survives, and it is new.* A **duration** bound. B1 exposure ends the moment the holder
broadcasts `T`: every historical `(o_i, e_i)` pair is dead thereafter. It bounds time and adversary
class rather than value, and it is the only action here that does not need the adversary's help.

*The four mitigations being deleted*, each refutable in about one grep: "the SE is blind to which
coins are valuable" (the operator's own migration-0009 column says otherwise); "≥144 blocks of
notice" (`F` is key-path P2TR with `merkle_root = None`, and the attack never broadcasts `T`);
"pre-hack untouched coins are unconditionally safe" (circular — its proof is T-SE-1 restated); and
"collusion leaves publicly-queryable evidence".

**NOT decided here:** publishing the prior-owner count `k`. It is gated on first closing a forgery
hole — `validate_backup_chain_v2` enforces only *pairwise* decrement, with nothing anchoring the head
of the chain to `h_deposit + initlock`, so a sender truncates the high end, buys one extra co-sign per
dropped rung, discloses it in `superseded_states`, and the census still balances. A receiver can be
shown a one-element chain and conclude "structurally B1-immune". **A safety signal that can be forged
downward is worse than no signal**, so `k` waits on the head anchor.

### D40.2 — CO-1 / O-1 / CO-3 are ONE defect: consume what is attested, close the JS lane, name the real closure

**Decided:** the ~1-day fix, now — plus the honest statement of what it does not close.

1. Derive terminality on both acceptance paths from the **attested** `has_sig_budget &&
   num_sigs >= sig_budget`; delete the unattested `get_spend_budget` reads; keep the coordinator's
   answer as a **raw-quantity** cross-check that refuses on disagreement (both sides hold the same
   absolute value, so the quantities really are comparable and the stronger check is available).
2. Make the JS/web laddered gate **structural**, not keyed on three sender-declared fields.
3. **Name** a second, independently administered SE write domain as the only construction that
   actually closes this, and gate it on a second **legal entity** — it is meaningless otherwise.

*The structural finding.* O-1, CO-1 and CO-3 are not three faces of one anchor; they are **one
defect** — the enclave key material and the counter it attests are both held by the party the receiver
is being protected from. Grading them independently triple-counts it. **Publish one row, cite it three
times.**

*The live hole this closes.* `verify_conveyed_child` fetches the attested payload and then reads
terminality from an **unattested** coordinator endpoint computed entirely from the coordinator's own
Postgres. A repo-wide grep for `has_sig_budget` in `clients/` returns exactly one site: the attested
budget is verified and then **never read by any acceptance decision**. One unattested `terminal: true`
kills a whole parent tree, on both the pre-pay and claim paths.

*Rejected, with the argument recorded so it is not re-proposed.* An external anchor over
`(sid, n, h_n)`. This row's attack is **under**-reporting, and the receiver's rule would be a *floor*
against an adversary who writes the floor: anchor `n=5` while the true count is 7, serve 5, check
5 ≥ 5, pass. Tightening to equality does not save it — `h_n` is a hash over per-coin session keys no
receiver can recompute. It also costs a per-coin activity oracle in a design whose SE deliberately
sees nothing.

*Also corrected:* the victim-count figure. `max_derived_tokens_per_statechain = 64` is a **per-parent
lifetime** cap over spent and unspent rows, not a per-level fan-out — so the binding count is ~64, not
the published 76,335, which was three orders of magnitude out **in the alarming direction**.

### D40.3 — C-1: a minimum-slack admission margin

**Decided:** refuse a conveyed child whose `check_exit_headroom` slack is below a margin proportional
to its own walk, and publish the normative sentence that goes with it.

*Why this and not a stress rate.* **Every input is already receiver-derived.** That is precisely what
disqualifies the stress-priced alternative, which would add a `stress_fee_rate` nobody can derive to
the trust base — and at stress = 20, depth 10 the minimum admissible piece becomes ~125,330 sat,
killing the entire simulated payment range to insure a marginal exposure the pass priced at **67 sat**.

*The defect being closed.* `check_exit_headroom` admits at **slack == 0**, and a test pins that case.
The slack is the piece's entire tolerance for confirmation variance, fee stall and reorg across a
2,885-block walk — and **the sender chooses it by choosing when to convey**. The margin removes that
free lever. Cost: late-in-epoch conveyances are refused, forcing a re-anchor — a liveness cost, in the
direction `ADMISSION-INPUTS.md` already names as the safe one.

*Two corrections to the record.* The aggregate is **constructible and UNEVALUATED**, not unbounded:
the loss set `Σ V_i · 1[V_i < V_min(d_i, r)]` is monotone in `r` and **saturates** at the piece lane's
total value. And `r` is the **mempool floor**, not a fee estimate — "under water between 5 and 10
sat/vB" means a floor *sustained* there across the whole walk, which is a materially different claim.

### D40.4 — Sequencing: Tier A code first, then draft

**Decided:** land the items that must exist before their section can be written honestly, then draft.
Slowest to first prose; fastest to prose that survives review.

## D41 — B.8 (the flat-chain head anchor) is NOT receiver-derivable; `k` stays unpublished

**Investigated 2026-08-13 while executing D40.1's gate. The specified fix cannot be built as
written, and that is a sharper reason to withhold `k` than "not done yet".**

D40.1 made publishing the prior-owner count `k` conditional on first closing a forgery hole:
`validate_backup_chain_v2` enforces only *pairwise* decrement, so a sender can truncate the HIGH end
of the chain, buy one extra co-signature per dropped rung, disclose it in `superseded_states`, and
leave the census balancing exactly — while a receiver reads a one-element chain as "structurally
B1-immune". The proposed anchor was: **max conveyed `nLockTime` must equal `h_deposit + initlock`.**

**`h_deposit` is not derivable by a receiver.** The first backup's locktime is
`block_height + initlock` where `block_height` is the tip **at the moment the deposit backup was
built** (`lib/src/transaction.rs`, the `qt_backup_tx == 0` arm) — which is *before* `tx0` is funded
and confirmed. So for a receiver holding `tx0` at confirmation height `H`:

    h_deposit ≤ H   ⟹   max_locktime ≤ H + initlock

That is an **upper** bound. Truncating the head makes `max_locktime` *smaller*, so an upper bound
cannot detect it. No chain fact forces `h_deposit ≥ anything`: the depositor may wait arbitrarily
long between opening the address and funding it. The existing `LocktimeTooLow`/`LocktimeTooHigh`
checks bound the CURRENT owner's rung against the tip, not the head of the chain.

**So the anchor needs `h_deposit` from somewhere, and the only holder is the coordinator** — the
party the count is meant to inform on. Taking it on the coordinator's word makes `k` forgeable in
exactly the direction that matters (downward, toward "safe").

**The real path is the attestation rail that already exists.** `h_deposit` is per-coin, immutable
after deposit, and known inside the enclave's own record; adding it to the `utexo/sig_count/v2`
payload would make the anchor checkable with no new trust. That is the same rail D40.2 consumes for
terminality and it is where this belongs — not a second mechanism.

**Until then `k` is NOT published**, per D40.1. The B1 disclosure stands on the T remedy
(`sever_from_f`) and on the honest statement that the exposure is unbounded, neither of which needs a
count.

## D42 — What Tier B could not land without an owner call (2026-08-13)

Eleven of the fourteen critical-path code items have landed. Three have not, and the reasons are
different in kind — worth separating, because "not done" and "cannot be done as specified" and
"needs a decision" are three different states and only one of them is work.

### B.8 — **cannot be built as specified.** See [D41](#d41).

`h_deposit` is not receiver-derivable, so the head anchor needs the enclave attestation extended. `k`
stays unpublished; the B1 disclosure does not depend on it.

### B.5 — **needs decision 5**, and not for the plumbing

Threading `BumpCapability` through the child and spine lanes is mechanical. The rest of decision 5's
recommendation is not: it raises `committed_fee_rate` from **2.0 to 3.0**, which re-prices *every*
floor in the system and every piece already in circulation. `min_child_value` is
`(committed_fee(r) + P2A)·2 + dust`; at 2.0 that is 1,310 and at 3.0 it is 1,610, so every existing
piece between those values becomes un-splittable and the coloured floors move with it.

That is an owner call about user impact, not an engineering choice, and landing the plumbing without
it would ship half a decision — the half that looks like progress.

**One part of decision 5 must not be split off even if the rest waits:** the live-rate de-trigger has
to ship in the SAME COMMIT as the superseded-de-trigger census term, or a failed rescue bricks
conveyability. This repository has hit that silent-degradation shape three times.

### B.6 — **needs decision 10**

Conflict-aware rescue pricing and the greedy-successor fix both change what a wallet broadcasts and
when. The adversarial pass also corrected the framing: post-D14 **fees cannot buy CSV maturity**, so
this lane costs latency, not coins — and selling it as closing a theft channel is the overclaim a
reviewer catches. What it is worth deciding is the pinning sub-clause, alone.

---

**So the honest state of Stage 2 is 11 landed, 1 impossible-as-written, 2 awaiting decisions.** No
further code on the critical path can be written correctly without an owner answering decisions 5 and
10 — and decision 8 for the coloured chapter, whose blocking measurement (Stage 0.1) is now DONE and
came back clean: `sdk75` and `sdk77` both pass on HEAD, so P2 did not break coloured-ladder
spendability.

## D43 — The coloured lane ships K=1 per carrier; `sdk29` asserts the refusal (decision 8)

**Decided:** ship K=1-per-carrier. Do not build idempotent coloured conveyance.

This was the one decision that blocked *writing*, and Stage 0.1 is what made it answerable: `sdk75`
and `sdk77` pass on HEAD, so P2 did not break coloured-ladder spendability and there is no repair
cycle standing in front of the choice.

**`sdk29` is not a failing test — it is an undecided design question with a test attached.** It
exercises a coloured multi-payee batch, which needs `convey_child_bundle` to be resumable and the
split journal to carry `recipient_address`; neither exists. Under this decision it never will, so the
test is rewritten to assert the refusal by name. A test that pins a refusal is coverage; a test left
red against an unbuilt feature is a standing invitation to build it by accident.

**What the spec says.** One payee per carrier per payment. Issuers size carriers to the asset value
they intend to move — the same answer already taken for carrier granularity — and a payment to two
payees uses two carriers. The limitation is named rather than worked around.

**What this does NOT decide.** The coloured spine and coloured re-anchor stay scoped-but-unbuilt
(16–21 weeks, `COLOURED-SPINE-REANCHOR-SCOPE.md`). K=1 is a statement about batching, not about the
ladder.

## D44 — `committed_fee_rate` 2.0 → 3.0, and `BumpCapability` through every lane (decision 5)

**Decided:** raise the committed rate AND wire the bump. Both, and B.5 lands with the census term.

§5.13 contradicted itself four times because the code says one thing (a defence frozen at 2.0 sat/vB,
with two of three lanes unable to bump) and the design says another. Under the authority order the
CODE changes.

**The cost is real and is accepted.** `committed_fee_rate` 2.0 → 3.0 moves `min_child_value` from
1,310 to 1,610 sat, which re-prices every piece in circulation: a piece minted under the old floor
and now below the new one is not lost, but it can no longer fund its own tiers and must be rescued by
`combine`. That is the trade for a defence that can still relay when the floor moves.

**The ordering constraint is not advisory.** The live-rate de-trigger MUST ship in the same commit as
the superseded-de-trigger census term. A failed rescue that leaves the census short bricks
conveyability, and this repo has hit that silent-degradation shape three times.

### The cost, MEASURED rather than estimated (2026-08-13)

The raise was applied end-to-end as a probe, driven to the point where every derived floor was
re-computed independently and checked against what the code produced. Then reverted, because D44's
own ordering rule forbids landing it without the wiring and the census term. The patch is kept.

**Every floor, re-derived at 3.0** (a plain rung is `ceil(125·r) + 240`, a coloured rung
`ceil(168·r) + 240`):

| | at 2.0 | at 3.0 |
|---|---|---|
| plain rung | 490 | **615** |
| coloured rung | 576 | **744** |
| `min_child_value` | 1,310 | **1,560** |
| `min_spine_tip_value` | 820 | **945** |
| plain root-ladder floor | 1,800 | **2,175** |
| coloured ROOT floor | 2,058 | **2,562** |
| coloured CHILD floor | 1,482 | **1,818** |
| `min_split_output` | 554 | **666** |

**Two corrections to what the decision was taken on.**

1. `min_child_value` lands on **1,560**, not the 1,610 the decision sheet estimated. Slightly cheaper
   than advertised.
2. **`TOKEN_PIECE_SATS` moves 3,066 → 4,074, and `TOKEN_CARRIER_SATS` 17,384 → 22,536 (+30%).** This
   was NOT in the decision sheet and is the largest user-visible cost. It is not optional: the piece
   constant is defined as the coloured root floor at `PIECE_FEE_RATE_HEADROOM` (2×) the committed
   rate, so raising the rate raises the piece — and the head-room is a safety property (a piece must
   still be able to ladder if the rate drifts). Keeping the piece at 3,066 would mean cutting the
   head-room to ~1.34×, which trades a safety margin for a stable number. Not taken.

**The one benefit, also measured.** A 4,074-sat piece survives packaging up to **37 sat/vB** before
the sweep zeroes out, against 28 at the old size — 2.6× the legacy 1,500-sat piece's head-room rather
than 2×.

**Blast radius:** 3 constants, 2 derived constants, and 21 derivation pins across `tokens.rs`,
`granularity_model.rs` and `tesr.rs`. Every pin re-derived from the arithmetic, never copied from the
new output — a pin updated by reading what the code now says agrees with any bug. The arithmetic is
coherent across all 21, which is itself evidence the change is right rather than papered over.

**CONFIRMED by the owner, 2026-08-13, with the +30% in front of them:** accept it, keep the 2×
head-room. The head-room is a safety property and trading it for a stable number would re-open the
"unclaimable by construction" trap the piece constant was derived to escape — a piece that meets a
drifted rate but can no longer carry its own root ladder. The probe's values are the shipping values.

**Still owed before it lands:** the `BumpCapability` wiring (B.5), the superseded-de-trigger census
term in the SAME commit, and a full live E2E run — the E2Es fund carriers from `TOKEN_CARRIER_SATS`.

## D45 — TRUC slot contention is a PRICE, not a DoS — publish the measurement (decision 10)

**Decided:** publish, no code. B.6 is demoted to an optimisation.

Measured live rather than argued (`a_third_party_can_only_take_the_anchor_slot_by_paying_more`):

| | |
|---|---|
| owner's rescue holds the slot | 3.58 sat/vB |
| an UNDER-paying squat | **refused** — `new feerate 0.00000895 BTC/kvB <= old feerate 0.00003581` |
| an OVER-paying squat | succeeds at 65.36 sat/vB — the tier's package feerate goes **up**, at the stranger's expense |
| the owner reclaims | by out-bidding, same mechanism |

The anyone-can-spend anchor is therefore not a griefing vector on confirmation. The half that would
have been dangerous — downgrading a well-paying rescue to a cheap child, holding a tier under the
relay floor indefinitely — is forbidden by the replacement rules. What a squatter buys is the right
to pay the owner's fees; what it costs the owner is a bid. TRUC's 1000-vB child cap bounds the bid.

**Named in the limitations section with these numbers.** B.6 (conflict-aware rescue pricing) saves
fees in a rare case rather than closing a hole.

## D46 — `deadline_safety_due` covers CARRIERS (decision 4)

**Decided:** extend the pass. Forced action stays `T`, never the flat backup.

A.2's doc pass surfaced that `deadline_safety_due` excludes carriers on BOTH routes, so an RGB
carrier's `min(L_k)` rested on `auto_exit_due` alone — and the carrier lane is where the loss is an
ASSET rather than sats.

`sdk86` measured today that the calendar this protects is real: the flat backup chain's absolute
locktime is finite, mining moves the tip toward it, and each whole-coin hop spends `interval` of it
(100 hops on either preset). INV-27's "idle coins never age" is true of the CSV side only.

Nothing surfaces that calendar to a user today, which is why the automatic pass — not an API field —
is the answer for the lane that can lose an asset.
