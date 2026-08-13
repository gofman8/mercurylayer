# SPEC-ROADMAP — what must be true before a normative Utexo specification can be written

> **TIMELINE RE-COST 2026-08-11 — `COLOURED-SPINE-REANCHOR-SCOPE.md` §4.1.**
>
> **11–21 engineer-weeks** of build (was 19–27), **12–22 weeks to a full v1 draft** (was 20–27).
> **In agent sessions (§4.2): ≈6–9 to build, ~2–4 more to draft** — of which 2–4 are the
> non-compressing kind (live-stack E2Es, regtest cycles, the external rgb-lib fork).
> **D21 = BUILD RGB-over-Lightning. D33 = `clients/libs/web` has no consumer, so P4 closes at zero
> and the flip's client story is already complete.**
> Re-checked line by line against the tree rather than adjusted on paper. What moved:
>
> * **S0** (the intermediate-`SP` fee law — a live theft path) and **R0** (census attestation) are
>   DONE and verified, the latter live against the deployed coordinator.
> * **W1**, previously the largest item at 3–5 wk *and* the one holding the spec date, is DONE:
>   the package route, the P2A anchor spender and the v3 fee child are built and a stuck tier is
>   rescued through repo code; the funding question is decided (**D31**) and the tower float rail
>   is built with its capacity bound measured. **§6 and §15 are writable now** — that was the
>   sentence this re-cost turns.
> * The **plain** spine landed after the scope was written (CATS-B), so the coloured spine now
>   rides on tested scaffolding; S6, the "least grounded number" in the scope, is de-risked.
>
> Still open and still governing the date: **S5** (largest item), **P4** (unbounded on an owner
> question), and **D21**, which is referenced as a gate but has never been recorded as a decision.
> **CR-D no longer belongs on that list — it landed in `b79b525`** (`colored_reanchor`,
> `clients/libs/rust-sdk/src/refresh.rs:88`, over `build_colored_detrigger` /
> `cosign_colored_detrigger`, `clients/libs/rust/src/tesr.rs:2126`, `:2923`). What is left of it is
> not a builder but a **scheduler**: `colored_reanchor` has zero callers outside its own module.


Status: working document, 2026-08-10. **Amended 2026-08-10 by a second, gap-fill survey round — see
§0.** Verified against `feat/spark` @ `280ed88` (the first round ran at `a9187e9` / `29bac98`; the
gap-fill round re-verified at `280ed88`). Every claim carries a `file:line`. Claims I could not
verify are marked **UNVERIFIED** in place.

> **ROUND 3 — re-verification at HEAD `7f0c8c2`, 2026-08-13.** Not a new survey: a pass that opened
> the code behind the round-2 findings again. **Six of them are now closed in the tree**, and a
> roadmap that still lists them as open mis-prices the schedule in the expensive direction. Each is
> corrected in place, with the round-2 text kept as the problem statement it was:
>
> * **D14** (receiver-side δ margin) — **LANDED.** `sup.csv >= live.csv + margin`, with the margin
>   read off the LIVE rival's kind (`RivalKind::margin`, `clients/libs/rust/src/tesr.rs:10511`,
>   enforced at `:10751-10767`). **WP3c is done**, and with it one of the two publication gates.
> * **D8's first trust item** (`num_sigs`) — **LANDED and fail-closed.** The client sends a random
>   per-request `attestation_nonce`, the coordinator forwards it verbatim, and an unattested or
>   unverifiable count is a **refusal** (`clients/libs/rust/src/utils.rs:97-197`;
>   `server/src/endpoints/transfer_receiver.rs:55-105`). D4's premise P3 is earned; P1 is not.
> * **D7** (network profile) — **LANDED.** `TesrParams::for_network_checked` names every network and
>   an unknown one PANICS rather than falling through to the toy schedule; testnet/signet run the
>   MAINNET schedule (`lib/src/tesr.rs:264-325`). The deployed epoch is now **10 000**, matching the
>   compiled-in table (`server/Settings.toml:2`; `flat_ladder_params_const`, `lib/src/tesr.rs:351`).
> * **D13** (the plain leaf's runtime deadline defence) — **LANDED.** The near-deadline pass covers
>   plain children and spine tips, not only coloured ones (`clients/libs/rust-sdk/src/wallet.rs:2317`
>   and its `[D13]` banner).
> * **W1 / D5's escape hatch** — **BUILT.** A P2A anchor spender, a v3 fee child and package
>   submission over Core RPC all exist and are wired into both broadcast loops
>   (`lib/src/wallet/p2a_fee_child.rs`; `BumpCapability`, `clients/libs/rust/src/tesr.rs:9631`;
>   `watch_pass_with_bump` / `exit_pass_with_bump`). D31 decided the owner funds it.
> * **WP6b** (interop) — **DONE except the HRPs.** The address decoder bounds-checks before slicing
>   (`lib/src/lib.rs:58-63`), the invoice and the recovery bundle both refuse an unknown version, and
>   the uniffi FFI now REFUSES a laddered message outbound instead of stripping it.
>
> Still open exactly as written: **WP3a** (the forged-yardstick tripwire still asserts ACCEPTANCE,
> `tesr.rs:18145-18194`), the `aggregate_xonly` backfill (no migration past 0011), D3's automatic
> renewal (`renew_auto` still has zero non-test call sites), D18, D20.

---

## 0. Provenance — which parts of this document had how much scrutiny

**The original run was partial.** It was planned as six surveys and three returned. The three that
returned were organised by *method* — documentation truth, test evidence, RGB — which is why §4 was
written as "seven unsurveyed subsystems". **Three lanes failed and returned nothing: core
soundness, the audit backlog, and Lightning.** Everything in §1–§7 as originally written was
therefore blind to all three.

A second, gap-fill round has now closed them, plus the operational surface:

| Round-2 lane | Status | What it changed here |
|---|---|---|
| Re-anchor remainder, TRUC / package relay, P2A | **returned in full** | D5 escalated, D6 widened, D2 corrected, new D12; §4 chain-model row rewritten |
| Census sufficiency, [B1], the leaf security argument | **returned in full** | **D4 largely answered** (proof holds, conditionally) and merged with D8; D3 corrected; new D13; §9 downgraded |
| Audit backlog Tier 1 (verifier binding) | **returned in full** | C-1, C-2, H-1, H-2(a) verified **CLOSED**; §7's binding half becomes writable; new D14 |
| Audit backlog Tier 2/3 (coordinator gates, child exit) | **returned in full** | D8 enumeration replaced (4 items → 12); new D15, D16; §4 gains a row |
| Lightning (HODL latch) | **returned in full** | App. B's §6/§8 blocked; new D17; D1's scope-out price reduced |
| Operational surface (wire/versioning, errors, durability, watchtower, encodings, coordinator migration) | **returned in full** | §23 unblocked; new D18, D19, D20; D9 and D11 corrected |

Scrutiny is **not** uniform, and the reader should treat these differently:

- **Two independent passes**: the TES-R ladder shapes, the census, the value laws, the verifier
  refusal set, the leaf lane, the split spine.
- **One pass, round 2 only**: relay/TRUC policy, the coordinator transfer/claim/cancel state
  machine, Lightning, the watchtower obligation, durability/recovery, the encodings, error surface,
  coordinator migration.
- **One pass, round 1 only**: RGB, documentation truth, test-evidence quality.
- **Still zero passes**: cryptographic constructions, the SE (lockbox) contract, deposit /
  onboarding, authorization, the confirmation/reorg half of the chain model, mailbox availability
  and censorship. These remain the load-bearing gap.

Round 2 was a code-reading round. **No lane executed a test**, so every E2E-backed claim below stays
AUTHOR-ATTESTED (see the §7 evidence note). Round-2 findings that rest on Bitcoin Core policy
constants (TRUC / BIP-431 ancestor limits, sibling eviction) were **UNVERIFIED** — those constants
appear nowhere in this tree. **Round 3: the ancestor limit and the 1P1C rescue were run against Core
30.2.0 and are now measured** (`docs/utexo/notes/WP1-TRUC-P2A-SPIKE.md`, itself AUTHOR-ATTESTED —
reproducible from the note, not from CI). **Sibling eviction was not run and stays UNVERIFIED.**

---

## 1. The answer

**No — a normative spec cannot be written today without guaranteeing a retraction, and the gap is
wider than "the ladder is nearly frozen" suggests.** The *ladder* is close: tier shapes, the census,
the value laws, the flat-backup ladder, the leaf CSV budget and the verifier's refusal set are
settled design backed by a genuinely spec-grade unit corpus (303 `mercuryrustlib` tests that build
real transactions, co-sign with real keys, carry non-vacuity controls and an
`assert_not_an_unrelated_refusal` anti-wrong-reason guard; the fee constants are *measured* against
the production finaliser at `lib/src/tesr.rs:867-920`, not asserted). But a document a third party
can implement from is not the ladder. It is also the cryptography, the SE contract, onboarding and
authorization, the chain model, the watchtower obligation, cooperative exit, recovery and the interop
encodings.

**Round-2 amendment.** Six of those subsystems have now been surveyed and the picture shifted in
both directions. Better than assumed: the **verifier is bound to the coin** on all three admission
lanes (C-1, C-2, H-1 and H-2(a) are CLOSED — see §4a), the **census sufficiency proof holds** against
a malicious sender (D4 collapses to a deletion plus a restatement), **durability and recovery** are
snapshot-consistent and spec-grade, and the **child lane now has a wired reactive watchtower pass**.
Worse than assumed: there is **no machine-readable error surface at all**, **no coordinator
protocol-migration story at all**, **no forward-compatibility rule for `protocol_version`**, the
**leaf's only runtime deadline defence is gated to the RGB lane D1 recommends removing**, the
**coordinator trust surface is twelve items, not four**, and the **P2A escape hatch is not merely
unbuilt but unreachable through the reference transport**.

**Round-3 amendment (HEAD `7f0c8c2`).** Three of those six "worse than assumed" findings have been
answered in the tree since, and one was answered so directly that the code comment names this
document's own framing. The leaf's deadline defence **now covers the plain lane** (D13), so the RGB
scope-out no longer costs a defence. The P2A escape hatch is **built and reachable** — an anchor
spender, a v3 fee child, and a package route that is a Bitcoin Core RPC precisely because electrum
has none (D5) — and the TRUC ancestor limit behind it is a measurement rather than a reading of Core
(§4a). The coordinator trust surface is still twelve items, but the first of them — the census's own
right-hand side, `num_sigs` — is **closed by a nonce-bound enclave attestation the client refuses to
proceed without** (D8). The three that stand unchanged are the ones with no code in them yet: the
error surface, the migration story, and the `protocol_version` ceiling.

Shortest honest path, in order:

1. **Decide the deliverable and freeze the surface** (D0). The existing corpus went comprehensively
   stale in about two weeks; writing a spec against an unfrozen tree reproduces that failure.
2. **Spend week 1 buying down what is left** — **round 3: this is now D10 (the laddered-coin
   blinded-MuSig residual) plus the survey of the four remaining unsurveyed subsystems, and nothing
   else.** The P2A/CPFP mechanism is built and the TRUC relay regime is measured; D4 was answered in
   round 2. D10 is the only week-1 item that could still turn out to be an excluded-scope collision.
3. **Take the decidable decisions** — D1, D2, D3, D5, D6, D7, D8, D9, D11 from round 1, plus **nine
   new ones from round 2** (D12–D20). Most still resolve toward "specify what the code does and
   delete the aspirational prose"; the new ones are mostly *policy the code has never had to state*.
4. **Close the two value-law holes the suite currently asserts are OPEN** — the receive-side δ margin
   that stood beside them is closed (D14/WP3c). WP3a in particular is still open at HEAD and is still
   the publication gate, because a spec that contradicts a green test in its own repo is worse than
   no spec.
5. **Draft in parallel from day one.** About half of an implementable spec depends on no open
   decision.

Timelines are in §7, and the current schedule is the 2026-08-11 re-cost at the head of this document
(**11–21 engineer-weeks of build, 12–22 to a full v1 draft**). The round-2 figures below it —
decisions closed 5–7 weeks, ladder-only v1 in 8–10, full protocol v1 in 12–16 — are kept as the
baseline they were measured against, and round 3 moves them only downward. **One** item I refuse to
put a number on, down from three: D10.

---

## 2. Decisions only the owner can take

Ordered so they can be worked through in one sitting. D10 is *not* decidable in a sitting — it is
commissioned in the sitting and answered by the week-1 work. **D4 was in that category and no longer
is: round 2 answered it.** D0–D11 are round 1, amended in place where round 2 contradicted or
sharpened them; **D12–D20 are new in round 2**.

### D0 — What is v1, and is the surface frozen while it is written?

| | |
|---|---|
| **Why it blocks** | The corpus that WP7 must repair went stale in ~2 weeks: CATS-B change 2 (spine tip), leaf combine (`clients/libs/rust/src/combine.rs:897`), leaf renewal (`clients/libs/rust/src/tesr.rs:19929`), `POST /transfer/cancel` (`server/src/endpoints/transfer_sender.rs:319`) and the re-anchor Void/Blind change all landed inside the window the surveys audited. A 3-month spec written against that rate of change does not converge. |
| **Options** | (a) **Ladder-only v1**: "Mercury Utexo TES-R Ladder Specification" — tiers, census, value laws, split/leaf, exit. Honest title, 8–10 weeks. (b) **Full protocol v1**: adds crypto, SE contract, deposit/auth, chain model, liveness, cooperative exit, recovery, encodings. 14–18 weeks. (c) Full protocol, published in two parts, ladder first. |
| **Recommendation** | **(c)**. Publish the ladder part first under an honest title, then the surrounding parts. Whichever is chosen, declare a **hard freeze** on the frozen surface — tier shapes, constants, verifier refusal set, wire format, admission rules — effective the day drafting starts, with a written exception rule: *no change to the frozen surface lands without the spec-section diff in the same commit*. The freeze is free and it is the single largest omission from any plan that does not have one. |

### D1 — Is RGB in scope for v1, and if so which of the two live lanes is normative?

| | |
|---|---|
| **Why it blocks** | Two complete, mutually exclusive RGB lanes sit behind one boolean, `SdkConfig::colored_ladder`, false at both constructors (`clients/libs/rust-sdk/src/config.rs:283`, `:312`). They differ on every property a spec must state. Unilateral exit: legacy carriers have **none** — `unilateral_exit` refuses by name at `clients/libs/rust-sdk/src/wallet.rs:2892`; CTES-R has one (sdk75). Forwarding: legacy is a structural 1-hop leaf — the second split needs `carrier_sats > TOKEN_PIECE_SATS + fee_reserve` with `fee_reserve ≥ 300` (`clients/libs/rust-sdk/src/tokens.rs:3324-3325`), unsatisfiable at *any* value of `TOKEN_PIECE_SATS` (now 3066, `tokens.rs:105`); CTES-R gives ~36 child hops. Combine: legacy atomic, CTES-R structurally cannot and substitutes N non-atomic legs with no journal (`tokens.rs:4147-4172`). Batch K>1: legacy allows, CTES-R refuses by name (`tokens.rs:230`, `:3600`). CTES-R **had** no on-chain re-anchor — `refresh` refused carriers while `build_colored_child_retransfer` told the user at the floor to "re-anchor it", a primitive that did not exist. **Round 3: it exists.** CR-D landed in `b79b525`: the plain refusal now DISPATCHES, pointing a coloured carrier at `colored_reanchor` (`clients/libs/rust-sdk/src/refresh.rs:88`, dispatch at `:264-295`), which broadcasts the already-co-signed trigger and then a coloured de-trigger — two transactions, zero CSV wait, no SE schedule change. The residual is narrower and different in kind: **nothing schedules it.** `colored_reanchor` has zero callers outside its own module, and both automatic passes exclude carriers by design (`auto_refresh_due` and `deadline_safety_due`, `refresh.rs:432`, `:513`), so a coloured carrier's renewal is manual. CTES-R is capped at one payment and one payee per carrier (`tokens.rs:131`, `clients/libs/rust/src/tesr.rs:467-471`), is not wired to Lightning (`tokens.rs:3292`), cannot be received by the JS/web clients (`clients/libs/nodejs/transfer_receive.js:184`), and depends on an **uncommitted filesystem path** (`clients/libs/rust-rgb/Cargo.toml:15`) whose working tree holds three methods mercury calls that exist in no commit. `README.md:7`'s headline "every coin stays unilaterally exitable to L1" is false for RGB carriers as shipped. |
| **Options** | (a) Scope RGB out of v1; one-page non-normative status appendix. (b) Specify the legacy lane normatively and state "RGB carriers have no unilateral exit". (c) Specify CTES-R, flip the default. (d) Specify both with a normative selector. |
| **Recommendation** | **(a)** — but priced honestly, because it is *not* one sentence. The sats lane has a precondition that is only definable in the lane being removed: **a coin bearing an off-chain commitment MUST NOT be given a plain TES-R ladder** ([B1] plus a terminal-freeze violation). Today that is enforced sender-side only (`clients/libs/rust-sdk/src/wallet.rs:866`); the recommended receiver-side refusal in `validate_encrypted_message` was never built, and `transfer_receiver` reads and persists `rgb_consignment` off conveyed backups with no cross-check (`clients/libs/rust/src/transfer_receiver.rs:885`, `:967`). So the scope-out costs: one §1 sentence, one normative ladder-establishment precondition written with an RGB-agnostic predicate, **and a receiver-side refusal that must be built** (WP6). Do not pick (b): it enshrines the structural-leaf defect `COLORED-FORWARDING.md` was written to kill. |
| **Round 2 — the price goes DOWN by one item and UP by another** | **Down:** the scope-out also removes the coloured Lightning half at **zero cost** — it is already refused by name on the surviving lane (`clients/libs/rust-sdk/src/tokens.rs:3292-3295`), non-exact RGB PAY is refused (`clients/libs/rust-sdk/src/ssp.rs:1093`), and RGB receive has no remote-SSP path (`ssp.rs:1174-1183`). `LIGHTNING.md §9` residual 6 disappears with it, and the sats LN lanes never touch `tokens.rs`. **Up (round 2, now PAID):** the only runtime near-deadline exit for a *leaf* was gated to COLOURED children by an explicitly deliberate narrowing (`Row::Child(cb) if cb.is_colored() => …, _ => continue`), so scoping RGB out would have left the v1-normative lane as the **unprotected** one. **Round 3: the port happened.** The loop now runs on both lanes and dispatches only the *event* on colour (`clients/libs/rust-sdk/src/wallet.rs:2317-2331`, `:2412-2421`), with the mid-split leaf case handled by a split-journal gate rather than by excluding plain rows (`:2249-2257`). The scope-out no longer costs a defence. This was **D13**. |

### D2 — Is the CATS-B split shape frozen for v1?

| | |
|---|---|
| **Why it blocks** | `PROTOCOL.md:254` and §5.7 still describe the pre-CATS shape (a split state at nSequence Δ_{k+1}, "undercuts by ≥36"). Code pins `let sp_csv = SPINE_CSV` = 0 after refusing if `s0_csv <= SPINE_CSV` (`clients/libs/rust/src/tesr.rs:5346`, `:2476-2482`, `:4285-4291`). The entire security argument for zero — replace-by-lower-timelock at its extreme, the [B1] asymmetry not arising because voider and victim are the same entity, and the explicit liveness trade that zero-CSV tiers accelerate an honest exit and a theft identically — exists **only** in a doc-comment at `tesr.rs:5323-5346`. No normative document contains it. |
| **Options** | (a) Freeze the current shape; V4 (key prefix), the coloured spine and K>1 become named v2 work; promote the `tesr.rs:5323-5346` argument into §5 verbatim. (b) Land the remaining CATS-B units first, then freeze. (c) Revert the spine. |
| **Recommendation** | **(a) with one exception: V5 is not deferrable.** V4 (a key prefix) and K>1 are additive and retract nothing. **V5 is the 820-sat spine-tip floor — a value *admission* rule.** Publishing §5 without it and adding it in v2 retracts a receiver-side admissibility rule and moves the V_min table D3 publishes. Either land V5 before §5 freezes, or mark §5 and §10 "provisional on the spine-tip floor" in §1. Do not let it ship as "additive". |
| **Round 2 — a required correction to (a)** | Promoting `tesr.rs:5323-5346` verbatim publishes a **false relay claim** alongside a sound timelock one. That doc-comment argues the CSV axis correctly (replace-by-lower-timelock at its extreme; voider and victim are the same entity). It says nothing about relay — and the sentence that would sit beside it in §5/§6, `PROTOCOL.md:245-247` ("each tier confirms before the next is valid, so no long vulnerable chains"), is **false for exactly the spine**: `SPINE_CSV = 0` means a spine tier IS valid before its parent confirms, which is why it exists (`clients/libs/rust/src/tesr.rs:5346`, `:4603`, `:2476-2482`). Consecutive spine tiers are precisely the "long vulnerable chain" that sentence says cannot arise, bounded only by the exit-length cap (`max_exit_txs`, `lib/src/transfer/receiver.rs:955`). **Round 3 — that cap is now 23 on the deployed profile too, not 139**: under D25 testnet runs the mainnet schedule and the deployed epoch is 10 000, so the deployed evaluation is the mainnet one (`lib/src/tesr.rs:264-305`; `server/Settings.toml:2`). §6 must instead state: a spine tier trades confirm-before-valid for latency; the compensating bounds are the exit-chain length cap plus a TRUC in-flight window of 2; and the 1P1C argument applies to the CSV-separated rungs (`T→X`, `X→S`, `SP→ext`), never to `SP_i → SP_{i+1}`. |
| **Round 2 — `transfer_many` is not gated by this** | `transfer_many` has shipped (`clients/libs/rust-sdk/src/transfer.rs:713-800`) with a `DERIVED_SLOTS_PER_STATECHAIN` cap (`wallet.rs:52-63`) and three E2Es (sdk11, sdk69, sdk83). Do **not** specify it as a separate operation: it is the same in-ladder split with K payload outputs. Parameterise the split shape over K and state the admissible range per lane — CTES-R refuses K>1 by name (`tokens.rs:230`), the sats lane is capped by the derived-slot budget. |

### D3 — What is a coin's normative end of life, and under what value/fee conditions is it reachable?

This merges three questions that cannot be answered separately: renewal ownership, the `d_floor`
terminal state, and the sub-economic viability floor.

| | |
|---|---|
| **Why it blocks** | (i) `PROTOCOL.md:295` ("the SDK runs renewal inside `transfer()`"), `:348` ("rollover runs unattended inside `transfer()`") and `SPEC.md:152-156` state automatic renewal as running code, and `PROTOCOL.md:38` declares §5 normative-and-shipped. It is false: `renew_auto`/`rollover_auto` (`clients/libs/rust/src/tesr.rs:4967`, `:4975`) have **zero** SDK call sites; sdk43 calls `mercuryrustlib::tesr::renew` directly with hardcoded CSVs. (ii) At the floor `next_rival_state_csv` just errors (`tesr.rs:2026`). (iii) The stated escape is "exit or re-anchor" (`tesr.rs:6375`) — but `reanchor` requires a CONFIRMED coin and then calls `withdraw` (`clients/libs/rust-sdk/src/refresh.rs:180-212`), i.e. it is the **cooperative, SE-co-signed** 112-vB path, and for a sub-coin the branch is materialised on chain first. So "re-anchor" is cheap and SE-dependent for a root, and costs the exit walk plus a fresh deposit for a leaf — it is not a second, cheaper option for the coins that need it. (iv) Below a break-even `V_min(d,r)` neither exit nor re-anchor is affordable: a split piece carries no flat backup (`CHILD_V2_BASELINE = 0`) while the sender retains a 112-vB flat backup over F that voids the tree, and `check_exit_headroom` checks the exit fits in *time*, never that it is worth doing. `SUBECONOMIC-FINALITY.md:3-8` is listed in `README.md:119-122` as a normative finding whose own header says "Nothing in this document is built." (v) `PROTOCOL.md:334-337`'s "default SDK policy: solo compaction at depth > 3" has no code — zero occurrences of `compact` in the SDK — and `PROTOCOL.md:619`'s "~180 vB/yr" footprint economics is *derived* from it. |
| **Correction the spec must carry** | Compaction is **not** "an optional depth-capping policy, not a liveness requirement", as `PROTOCOL.md:334` says. A derived depth cap is enforced at four build sites and on the receive side (`enforce_split_depth_cap_shaped`, `clients/libs/rust/src/tesr.rs:5210`, `:5953`; `max_split_depth`, `lib/src/transfer/receiver.rs:768-779`). At the cap further splits are refused and continued liveness requires an on-chain re-anchor — which is compaction under another name. Only the *threshold* is policy. |
| **Options** | (a) Specify renewal/rollover as caller-driven primitives; state a normative end-of-life rule ("at `d_floor` the coin is terminal: exit unilaterally, or re-anchor cooperatively"); state compaction as REQUIRED at the depth cap with the threshold as implementation policy; publish `V_min(d, r)` as a table over fee rate and name sub-economic finality as a limitation; re-derive §7.1's footprint from the cap and the epoch. (b) Build the automatic policy and a receiver-side viability gate, then specify. (c) Specify the unbuilt policy as REQUIRED-of-implementations. |
| **Recommendation** | **(a)**, written as ONE section. Do not pick (c) — a spec whose reference implementation is non-conformant on day one is a wish. Two things to get right: publish V_min as a function of `(d, r)`, not the single r=2 number (see D5 — every floor in the tree is frozen to 2.0); and re-derive the footprint economics rather than deleting them, but do it **after** D7, because the number depends on the epoch. |
| **Round 2 — the hop arithmetic is wrong in both directions, and the binding cap is TIME** | "36 whole-coin hops" is the **per-epoch** state budget `(d0 1440 − d_floor 144)/δ 36` (`lib/src/tesr.rs:210`, `:227-229`). **Leaf renewal resets the state to `state_csv(0)` while stepping one extension rung down** (`plan_child_renewal_auto`: `p.ext_csv(next)` + `p.state_csv(0)`, `clients/libs/rust/src/tesr.rs:19508-19513`), so the ceiling is ~`m_max(15) × 36`, not 36. But renewal **never moves the epoch deadline** and a leaf **cannot be re-anchored** — `refresh()` routes a `ctesr-` coin to `unilateral_exit` because `SP.out[j]` is un-broadcast and there is no confirmed outpoint to co-spend (`tesr.rs:5006-5008`). So the binding cap on a leaf is **TIME** — `min(L_k)` over the parent's flat backups — and the hop budget is only reachable inside that window. Also: **each `/transfer/cancel` reclaim consumes one state rung from the same δ budget** (`tesr.rs:7319-7365`), unmetered and undocumented. A spec publishing "36 hops" is wrong three ways: too low for the renewal ceiling, silent on the deadline, silent on the cancellation drain. |
| **Round 2 — sharpen the sub-economic limitation for a leaf** | `check_exit_headroom(csvs, tip, epoch_expiry_height)` takes **no value argument** (`lib/src/transfer/receiver.rs:741-760`): it decides whether an exit fits in *time*, never whether it is worth doing. A leaf additionally has no flat backup (`CHILD_V2_BASELINE = 0`, `tesr.rs:5321`) and no re-anchor path. So the terminal state is not "below V_min the exit is unaffordable" — it is "**below V_min the leaf is unaffordable to exit AND expires at `min(L_k)` to the party who split it**", who is the parent's last owner, i.e. exactly the adversary. Publish V_min(d, r) **together with** the leaf epoch deadline, or not at all. |

### D4 — Is exact-equality census on the SE's TOTAL signature count sufficient? *(PROTOCOL.md's own O-1)*

| | |
|---|---|
| **Why it blocks** | `PROTOCOL.md:306` claims "SE atomically advances the public counters {level, m, k, total_sigs}" and §5.11 R4′ (`:481-483`) makes the receiver check the current extension's nSequence against "the SE's publicly-served counters". **Nothing serves m or k**: `/info/statechain/<id>` returns only `num_sigs` forwarded from the enclave's `sig_count` (`server/src/endpoints/transfer_receiver.rs:45-67`), `/statechain/spend_budget` returns `{sig_budget, finalized, terminal}`, and there is no `/renew/init` route anywhere in `server/src/endpoints`. The live verifier derives every CSV from the conveyed bundle and cross-checks the TOTAL only (`flat_backups + tiers.len() + superseded_ok`, `clients/libs/rust/src/tesr.rs:9093`). `PROTOCOL.md:767-771` lists this as "blocking, STILL OPEN" while §5.5/§5.11 describe it in the present tense. Receiver verification is the heart of the spec. |
| **Options** | (a) Attempt a sufficiency proof: total-count equality + root-anchoring to the on-chain F (commit `7a03799`) + REQ-38's per-superseded parse/link/signature/strictly-higher-CSV checks admit no substitution of a disclosed decoy for a hidden co-sign. If it holds, specify total-count census and DELETE the counter machine from §5.5/§5.11. (b) Build the counter machine. (c) Specify the counter machine and call the implementation a subset. |
| ~~**Recommendation**~~ *(round 1, superseded)* | ~~Commission (a) in week 1 … if it fails, (b) requires SE-side per-level counters, i.e. excluded scope.~~ |
| **ANSWER (round 2) — the proof HOLDS. D4 collapses to a deletion plus a restatement.** | The counter machine was proposed to detect hidden co-signs *per level*. Total-count equality already does that, because a hidden co-sign at **any** level raises the same total. What the total alone cannot do is distinguish a decoy tier from a hidden state — and that is closed by three other mechanisms, not by per-level counters: **per-item validation** (`verify_superseded_segment` parses, txid-binds, ladder-links, co-sign-verifies, CSV-bounds-checks and proves non-confirmable by direct contention plus a transitive-death fixpoint — `clients/libs/rust/src/tesr.rs:8309-8460`), **slot uniqueness** (one `HashSet<Txid>` seeded from the LIVE tiers and spanning both superseded lists, `:8404-8425` — this is what closes [C-2]), and **root anchoring** (`verify_bundle_bound`, `:8227-8288`). **There is no excluded-scope collision from D4.** Remove it from §7's "three things I will not put a number on"; it is a week and a deletion, exactly as the optimistic branch predicted. |
| **The two premises the proof is conditional on** | P3 — `se_num_sigs` is the TRUE count — was **not** earned at `280ed88`: `/info/statechain` forwarded a bare JSON integer from the lockbox with no signature or attestation, and a coordinator under-reporting by ONE lets a sender hide one co-signed low-CSV state and pass the census exactly. **Round 3: P3 IS earned.** The client sends a random per-request nonce, the enclave signs `(sid, num_sigs, budget, nonce)`, and the client verifies it against the **chain-anchored** `enclave_public_key` — not the served `attestation_pubkey`, which would accept a coordinator signing with a key of its own. An unattested count is a refusal, with no phased rollout (`clients/libs/rust/src/utils.rs:97-197`; the pass-through, and the two `.unwrap()` panics it replaced, at `server/src/endpoints/transfer_receiver.rs:55-105`). P1 — the sid↔aggregate binding — is **still** supplied by the coordinator and unattested (`ladder_binding_precheck_cause`, `clients/libs/rust/src/tesr.rs:10344`). By contrast P4, `flat_backups`, is genuinely earned by the receiver in both lanes (`clients/libs/rust/src/transfer_receiver.rs:583-661`; child re-validation with INV-5 exact-decrement and prevout pinning at `tesr.rs:5673-5700`). **So: exact equality on a total DOES prove no other spending path exists — it is a sound proof with a named trust assumption, not a proof with a hole.** It must be published *with* the assumption. **D4 and D8 are therefore one decision, not two** — see D8. |
| **What §7 must say, and must NOT say** | (i) State the census as **three conjoined obligations**, never as the equation alone: a total, a per-item validation battery, and slot uniqueness over the **union** of live and disclosed tiers. The repo has been bitten twice by treating it as arithmetic — [S1] junk padding and [C-2] duplicate disclosure, where a *genuine* tier disclosed twice inflates `expected` by one for free while passing parse, linkage, signature and race checks unchanged. (ii) The property is "**no undisclosed spending path signed under A exists**" — **not** "the receiver cannot be defrauded". The census bounds spending *paths*, not *value*; two live value-theft classes pass it untouched and are pinned as OPEN tripwires (WP3a forged fee yardstick, WP3b ancestor minting). Hand the value claim to §8. |
| **Remaining D4 work** | (1) DELETE the `{level, m, k}` counter machine and `POST /renew/init` from `PROTOCOL.md §5.5`/`§5.11`. (2) RESTATE the census as a bijection wherever it appears (`PROTOCOL.md:494-495`, `CHILDREN.md:43`, `:66-67`) — adding "+ superseded" to a doc that still presents it as arithmetic reproduces the exact misreading that produced [S1] and [C-2]. (3) One adversarial test that conveys a child over an **already-triggered** parent: `verify_conveyed_child` fetches F only for its scriptPubKey and value, and rests the liveness claim on a comment — "unspent/confirmed is enforced by the terminality of the parent" (`tesr.rs:5641-5658`). Same evidentiary shape D4 existed to buy down. |

### D5 — Which fee yardstick governs which material — and does the escape hatch exist?

| | |
|---|---|
| **Why it blocks** | There are **three** yardsticks live, not two. (i) **Bundle parameter**: `fee_rate` is a field of `TesrBundle`, used ~40× as `bundle.fee_rate`. (ii) **Receiver's own constant**: the only binding to `TesrParams::for_network(...).committed_fee_rate` exists in ONE of three verifier entry points, `verify_conveyed_child` (`clients/libs/rust/src/tesr.rs:5769-5776`). (iii) **A ±tolerance band around the live market**, enforced on every flat backup the same census counts: `if (fee_rate + fee_rate_tolerance) < current_fee_rate_sats_per_byte { FeeTooLow }` and its `FeeTooHigh` mirror (`lib/src/transfer/receiver.rs:480-484`), default 5.0 (`clients/libs/rust-sdk/src/client_config.rs:66`), threaded into the live claim path. That band is an unnamed **liveness** property: if the market moves more than ±5 sat/vB from where the backups were signed, an honest receiver refuses an honest transfer. |
| **The dependency that makes this urgent** | Choosing "constant" commits mainnet tiers to exactly 2.000 sat/vB forever (`lib/src/tesr.rs:210`, `:215` — both presets), and above that rate the pre-signed tier no longer relays standalone (`lib/src/tesr.rs:85`), leaving the P2A anchor as the only remedy. **That remedy is not implemented.** `P2A_SCRIPT_BYTES` / `p2a_script()` / `P2A_VALUE` appear only where a tier *attaches* the output and in vbyte accounting; a repo-wide grep finds no builder that **spends** an anchor. The only CPFP code is `cpfp_tx::create_cpfp_tx` (`clients/libs/rust/src/broadcast_backup_tx.rs:64`, `wasm/src/lib.rs:258`), which bumps a legacy un-laddered backup by spending that tx's own P2TR output — unrelated to a tier anchor. So the escape hatch is design-plus-implementation with open content: who funds the bump, from which UTXO, under TRUC's one-unconfirmed-child and 1000-vB child limits, and what stops a third party pinning an anyone-can-spend anchor. |
| **Options** | (a) Tier rate = per-network constant, receiver-enforced by equality; band retained for flat backups with a normative width; publish both. (b) Bundle parameter within a normative admissible band, with a stated authority. (c) Constant on mainnet, parameter on test networks. |
| **Recommendation** | **(a) — but gated on the P2A spike (WP1).** The proof that honest bundles pass under equality already exists (`the_receivers_yardstick_is_a_per_network_constant`, `tesr.rs:15307`). Build one anchor-spending path and one regtest run with `minrelayfee` raised above 2 sat/vB **before** committing. If it cannot be made to work reliably, (a) is unsound as stated and the real options become (b) or "constant plus an accepted, stated liveness failure above 2 sat/vB" — both of which change §10 and §11 substantially. Whatever is chosen, §11 must state all three yardsticks and what happens when the market leaves the band (today: the coin stops moving off-chain, and no test covers it). |
| **Round 2 — the hatch is worse than "unimplemented": it is unreachable through the reference transport, and on a spine its slot is contended** | The round-1 claim is CONFIRMED (grep finds `P2A_SCRIPT_BYTES`/`p2a_script`/`P2A_VALUE` only in the definitions `lib/src/tesr.rs:36-41`, `:96-97`, the attach sites `:273`, `:559`, vbyte/value accounting `:112-164`, and tests — **no builder constructs a TxIn on a P2A outpoint**). Three escalations: **(1)** the only CPFP builder cannot be repurposed — `lib/src/wallet/cpfp_tx.rs:66` fixes `input_vout = 0` (a legacy backup's own P2TR output, not an anchor) and `:111` sets `version: 2`, and a v2 transaction may not spend an unconfirmed v3 output at all. **(2)** the transport cannot submit a package — every broadcast in the client libraries is `electrum_client::transaction_broadcast_raw` (18 call sites under `clients/libs/`), and the existing CPFP path broadcasts parent and child as two independent calls (`clients/libs/rust/src/broadcast_backup_tx.rs:70-76`), which works only because that legacy parent relays standalone. `PROTOCOL.md:759` says it plainly ("no `submitpackage` caller exists") and `PROTOCOL.md:525` records the `watch_pass`-must-be-package-aware amendment that was never built. **(3)** on a spine the anchor slot is contended: TRUC allows an unconfirmed parent exactly one unconfirmed child, and `SP_{i+1}` at CSV 0 occupies it — so the fee-bump remedy and the shape that most needs it are **mutually exclusive**. |
| **Consequence for WP1's spike** | The spike as scoped (one builder + one regtest run at raised `minrelayfee`) **cannot succeed**: it must also choose a **package-capable backend**. Budget accordingly. |
| **Round 3 — the hatch EXISTS. All three round-2 escalations are closed, and the spike was run.** | **(1) The builder.** `lib/src/wallet/p2a_fee_child.rs` builds a v3 child whose input 0 is the parent's P2A anchor (empty witness, kept empty deliberately) and input 1 the owner's funding UTXO; it prices the package, refuses above the TRUC 1 000-vB child cap (`TRUC_MAX_CHILD_VSIZE`) and refuses a change output below `CHILD_CHANGE_DUST = 330`. Its module note says in as many words why `cpfp_tx.rs` was not the starting point. **(2) The transport.** `BumpCapability` carries a Bitcoin Core RPC endpoint precisely because "Electrum has no `submitpackage`" (`clients/libs/rust/src/tesr.rs:9631-9642`), and the capability is threaded into both broadcast loops as `watch_pass_with_bump` / `exit_pass_with_bump`. **(3) Who funds it.** **D31**: the owner. A keyless tower still cannot bump — the capability's ABSENCE is the keyless case, with no default endpoint and no implicit funding source, so a tower cannot acquire it by accident (`clients/libs/rust-sdk/src/config.rs:351`, `:382` — `fee_bump: None` on both presets). The measured spike is recorded in `docs/utexo/notes/WP1-TRUC-P2A-SPIKE.md`. **What this changes for §12:** the six clauses below are all still required, but the last one now has an answer instead of a hole, and the sentence "the remedy is not implemented" must not survive into the spec. **What it does NOT change:** the spine's contended child slot (escalation 3 above) is a TRUC property, not a builder gap, and it stands. |
| **The six clauses §12 must carry, none of which is written anywhere normative today** | committed rate is a hardcoded 2.0 sat/vB on both presets (`lib/src/tesr.rs:210`, `:215`) and the walk is self-funding only at or below it (`:63-71`); above it every tier needs an external fee child; the anchor is `OP_1 <0x4e73>` / 240 sat / anyone-can-spend (`:36-41`); the child must itself be v3 and ≤1000 vB (~152 vB per `SUBECONOMIC-FINALITY.md:55-58`); submission must be a 1P1C **package** because the parent alone is below the floor; at most one unconfirmed child per tier; and **someone must FUND it** — a keyless tower by definition holds no UTXO (`clients/libs/rust-sdk/src/watchtower.rs:3-4`; `PROTOCOL.md:525`'s prepaid tower fee bond is unbuilt). |

### D6 — Which exit-cost model is normative — and note that it is an admission rule, not a figure

| | |
|---|---|
| **Why it blocks** | The repo computes cost and latency from two different mental models of one transaction chain: `tesr_exit_vbytes` still sums `3·TIER + d·(2·TIER + P2TR_OUT)` — the pre-CATS shape (`clients/libs/rust-sdk/src/config.rs:181-188`) — while `tesr_exit_wait_blocks` has already been re-derived for the spine, `720d + 2160` (`config.rs:196-207`). `CATS-B-PHASE1-PLAN.md:486` states the code's own model "over-counts a spine level" and that the correct form is `tesr_exit_txs(d) = 4 + d` with per-level `TIER_VBYTES`. Eight docs carry a *third* set of numbers, overstating the depth-3 wait by 2× (`PROTOCOL.md:419-424` says ≈60 days where code now implies ≈30). |
| **The mis-pricing to correct** | This is **not** a documentation regeneration. `max_exit_txs(base, per_level, epoch) = 3 + 2·max_split_depth(...)` is a **receiver-side admission rule** (`lib/src/transfer/receiver.rs:781-812`), and its doc-comment notes the latency bound and the transaction bound "meet exactly at depth 10 / 23 transactions on mainnet" — a coincidence that exists only under the current model. Re-deriving to `4 + d` moves the transaction cap, un-couples it from the latency cap, and changes **which conveyed bundles a receiver admits**. That touches `enforce_split_depth_cap_shaped` (`clients/libs/rust/src/tesr.rs:5210`) and every test that pins a refusal. |
| **Options** | (a) Re-derive from measured vsizes at depths 0..3, change the admission rule, re-pin the tests, regenerate every doc figure from the constant. (b) Publish `3 + 2d` as a conservative upper bound, stated as such. (c) Defer §10 to v2. |
| **Recommendation** | **(a)**, sequenced **after D7** (the epoch feeds the cap), budgeted with test churn, not as arithmetic. Over-counting is wrong-but-conservative for an internal margin and was a defensible engineering call; it is not defensible in a document a third party sizes fee reserves from. (b) is a trap: a bound published as a cost silently becomes a cost. |
| **Round 2 — widen D6 from cost to cost AND latency; there is a THIRD disagreement in the same family** | The two models differ on exactly the term TRUC makes load-bearing. **Admission side**: `exit_wait_blocks(csvs) = Σ(csv + 1)` (`lib/src/transfer/receiver.rs:720-722`), whose own comment charges "its own timelock PLUS the one block its parent needs to confirm". Charging 1 block per transaction is ≥ the TRUC floor of ~1 block per 2 consecutive CSV-0 tiers, so **the admission gate is conservative and safe under TRUC — adopt it, do not replace it, and say so in the spec.** **Published/SDK side**: `tesr_exit_wait_blocks(p, d) = 720d + 2160` (`clients/libs/rust-sdk/src/config.rs:203-207`) charges **zero** for every parent confirmation, therefore zero for a `SPINE_CSV = 0` tier where the confirmation is the *only* cost. It understates a depth-d TwoTier walk by `3 + 2d` blocks and a spine of length s by ~s blocks — and its doc-comment publishes the wall-clock claims derived from it ("depth 1 falls 4284 → 2880 blocks (29.8 → 20 days)", `config.rs:196-201`). **The doc already has it right**: `PARTIAL-PAYMENT-ECONOMICS.md:397` writes the spine term as `s·1` with the reason spelled out, and `:400` gives the payee `wait = i + 2880`; `SUBECONOMIC-FINALITY.md:59-61` agrees. **Sweep all three models in one pass**, or WP5 fixes the vbyte model and ships a document that is internally consistent on cost and inconsistent on time. |

### D7 — What is the normative network profile, and what does the deployment actually run?

| | |
|---|---|
| **Why it blocks** *(as found at `280ed88`; every clause in this row is fixed at HEAD — see the round-3 row below, and read this one as the problem statement it was)* | This is not two rows of a table disagreeing. `server/Settings.toml:1-2` — the deployed profile — is `network = "testnet"`, `lockheight_init = 1000`; `server/src/server_config.rs:79-82` defaults to `regtest` / `10000`. And `TesrParams::for_network` maps **only** `"bitcoin"`/`"mainnet"` to the mainnet schedule; everything else, testnet and signet included, silently gets the toy regtest schedule of d0 = 24 blocks ≈ 4 hours (`lib/src/tesr.rs:219-224`), pinned as intended by a unit test at `:1072` and stated in no document. So the profile actually deployed is the one the code calls regtest — and on `lockheight_init = 1000` the code's own derivation gives `max_split_depth = 68`, cap **139 transactions** (`lib/src/transfer/receiver.rs:806-812`), precisely the failure the P0-3 comment above it was written to prevent. Separately `PROTOCOL.md:487-488` specifies a hard depth cap of 8 that exists nowhere. |
| **Options** | (a) Publish the depth-cap FORMULA plus its evaluation per profile, give testnet and signet explicit `TesrParams` (no silent fallback), reconcile the two `lockheight_init` values to one normative profile, delete the phantom cap of 8. (b) Make the epoch a deployment parameter and publish only the formula. (c) Keep testnet/signet on the regtest schedule and say so explicitly. |
| **Recommendation** | **(a)**, and treat it as a code change, not a table. Also add `confirmation_target` to the table — it is 2 on regtest and 3 on mainnet (`clients/libs/rust-sdk/src/config.rs:268`, `:296`) and it gates when a deposit becomes spendable and when a claim is accepted. Sequence D7 **before** D6 and D3: the epoch determines the cap, the cap determines the cost table, the cost table determines V_min. |
| **Round 2 — a second, independent failure of the same combination** | The deployed profile's exit-chain cap of **139 transactions** admitted roughly **135 consecutive zero-CSV spine tiers**, whose TRUC stall (~68 blocks, at ~2 tiers per block) **exceeded the entire regtest state schedule** (`d0 = 24`). On the profile then running, the relay-stall term dominated the timelock schedule. Strengthened recommendation (a): give testnet/signet explicit `TesrParams`. |
| **Round 3 — D7 IS TAKEN, AS (a), AND THE COMBINATION ABOVE NO LONGER ARISES** | Three changes, all in the tree. **(i) No silent fallback.** `for_network_checked` names `bitcoin`/`mainnet`, `testnet`/`testnet3`/`testnet4`/`signet`, `regtest`, and returns `None` otherwise; `for_network` **panics** on an unknown name rather than guessing, so `"mainet"` can no longer produce a four-hour ladder on real money (`lib/src/tesr.rs:298-325`). **(ii) testnet and signet run the MAINNET schedule** — `TesrParams::testnet()` is `Self::mainnet()`, on the stated reasoning that a public test network with real 10-minute blocks should exercise the schedule that ships, and only regtest keeps the toy numbers (`:264-284`). Its own doc-comment records this as a deliberate **compatibility break** under D23: ladders built against the deployed testnet coordinator by an older build are refused. **(iii) The two `lockheight_init` values are reconciled at 10 000** and are no longer taken on the coordinator's word: `flat_ladder_params_const` compiles in `(10 000, 100)` for mainnet/testnet/signet and `(1 000, 10)` for regtest, and `info_config` **refuses by name** if the coordinator's `/info/config` disagrees, because `interval` is what INV-5 measures every hop against (`lib/src/tesr.rs:351-372`; `clients/libs/rust/src/utils.rs:41-64`; `server/Settings.toml:2-3` now reads `lockheight_init = 10000`, `lh_decrement = 100`). **Consequence for the numbers this document publishes:** the deployed evaluation of the exit-chain cap is the mainnet one — `max_split_depth = 10`, cap **23 transactions** — and the 139/68/135 figures above are a **superseded baseline**, kept because they are what motivated the change. **What survives of D7**: `PROTOCOL.md:487-488`'s phantom depth cap of 8, still nowhere in the tree; publishing the FORMULA plus its per-profile evaluation; and `confirmation_target`. |
| **Round 2 — `/info/config` cannot carry the profile decision either** | The only version-shaped value a client can read from the coordinator is `version: env!("CARGO_PKG_VERSION")` (`server/src/endpoints/utils.rs:191-198`) — a Cargo build identifier occupying the slot an independent implementation would key compatibility off. See **D20**. |

### D8 — What is the coordinator trusted for?

| | |
|---|---|
| **Why it blocks** | The census — the linchpin of receiver safety — compares against `se_num_sigs` read from `/info/statechain`, which the coordinator merely forwards from the enclave: `response["sig_count"].as_u64().unwrap()` (`server/src/endpoints/transfer_receiver.rs:67`), which also panics the handler on a malformed SE response. A coordinator that **under-reports** `num_sigs` lets a sender hide a co-signed tier and pass the census exactly. But `num_sigs` is not the whole surface: the coordinator is also the sole authority for the `statechain_id ↔ aggregate` binding (migration `0009`; `ladder_binding_precheck`, `clients/libs/rust/src/tesr.rs:8049-8100`), which is what defends the rogue-key decomposition `U := D − E_sid` — the source calls it "the one value in the whole acceptance path that is not restatable by the sender". It also holds the encrypted transfer mailbox and, since `POST /transfer/cancel`, a sender-initiated cancellation. Every adversarial test in the repo models a malicious **sender**; nothing models a malicious coordinator. `TRUST-MODEL.md` R5 lists the census evidence but never states whether the coordinator is trusted for the number. |
| **Migration tail nobody owns** | `aggregate_xonly` was backfilled forward only; pre-0009 rows are NULL and permanently un-laddered. The source names the only complete fix — "coordinator-side: backfill `aggregate_xonly` for the legacy rows from the coordinator's OWN columns" (`tesr.rs:8063-8066`) — and no work item schedules it. |
| **Options** | (a) State an enumerated trust assumption (num_sigs, sid↔aggregate binding, mailbox availability/ordering, tip service if proxied) alongside R1–R9, and name SE-signed counts as the closure path. (b) Bind `num_sigs` to an SE signature the receiver verifies. (c) (a) plus a detection mechanism (cross-check / gossiped counts). |
| **Recommendation** | **Verify first, then (a).** One day in week 1: does anything authenticate `sig_count` end to end? The coordinator's parse carries only the bare integer into the client response, so as far as the coordinator boundary shows, nothing does — **UNVERIFIED** whether the lockbox reply carries an attestation a client could bind. Add the aggregate backfill as a named coordinator task with an observable gate (zero NULL `aggregate_xonly` rows), and fix the `.unwrap()` while you are there. |
| **Round 3 — that verification was done and closure (b) was BUILT for `num_sigs`. Two of the three sentences above are spent.** | The enclave signs the count against a **client-chosen nonce**; the coordinator's only role is to forward the nonce and pass the attestation through verbatim, which it can neither forge nor replay (an older, lower attestation answered a different nonce). The client verifies against the **chain-anchored** `enclave_public_key`, refuses when the attestation is absent, and refuses a half-stated spend budget rather than reading "cannot say" as "no budget" — no phased rollout, under D23 (`clients/libs/rust/src/utils.rs:97-197`; `server/src/endpoints/transfer_receiver.rs:55-108`). The two `.unwrap()`s that panicked the handler on a malformed SE reply are typed errors. **Item (1) of the twelve is closed; the spend budget rides on the same signature.** **The aggregate backfill is NOT** — migrations still stop at 0011, so item (2) and the migration tail stand exactly as written, and the enumeration is now eleven items with one closed rather than four with none. |
| **Round 2 — MERGE D4 INTO D8** | After the D4 proof, the residual is not "is the total enough?" but "**who is trusted for the total AND for the sid↔aggregate binding?**" — both coordinator-supplied, both unauthenticated, both load-bearing for the same property. One ADR. Deciding one without the other publishes half a trust model. |
| **Round 2 — the enumeration is TWELVE items, not four; six are undetectable** | (1) **`num_sigs`** — forwarded unattested, `.unwrap()` panics the handler on a malformed SE reply. **CLOSED at HEAD (round 3): nonce-bound enclave attestation, verified against the chain-anchored key, refused when absent; both panics are typed errors.** (2) **`statechain_id ↔ aggregate_xonly`** — and note the actual justification, which is stronger than a pointer: `validate_tx0_output_pubkey` tests `enclave_pubkey(sid) + transfer_msg.user_public_key == tx0.out[vout]` and the **sender** chooses `user_public_key`, so the rogue-key decomposition `U := D − E_sid` makes any attacker-controlled output pass; both tempting fallbacks are rejected in source (`clients/libs/rust/src/tesr.rs:8085-8100`). `BindingRefusal::NoCoordinatorAggregate` is the **only** cause a caller may read as "this coin legitimately has no ladder" (`:8107-8119`), so a coordinator answering NULL can downgrade any coin to the un-laddered lane. (3) **`x1_pub`**, served by the same endpoint (`:70-75`) — a wrong value drives the enclave to rotate by the wrong tweak and **bricks the coin for everyone**, a worse outcome than an under-reported count. (4) **The mailbox** — availability, ordering and non-deletion (`server/src/database/transfer_sender.rs:236-241` DELETEs; the read filters `IS NOT NULL`). (5) **The pending-transfer lock** (`server/src/database/transfer_sender.rs:104-171`) — the only thing stopping a still-owner sender co-signing a rival while a conveyed receiver holds claimable material, and a SQL predicate the blind SE cannot evaluate. (6) **Both sign gates** (`server/src/endpoints/sign.rs:148`, `:324`) — one extra co-sign simultaneously breaks the receiver's census. (7) **The claim latch** (`server/src/database/transfer_cancel.rs:19`, `:228-241`) — mutual exclusion between claim and cancel across an irreversible SE `keyupdate`. (8) **Batch atomicity** (`server/src/database/transfer.rs:30-50`) — coinswap all-or-nothing rests entirely on coordinator state. (9) **The latch↔batch↔transfer binding**, currently **NOT enforced** (see D15/T2-C8). (10) **The two-clock invariant** written out in-source as "no authorization expires before the latest claim it protects" — one property, two independently maintained queries. (11) **Auth-nonce single-use**, deployed on only three endpoints (`/withdraw/complete`, `/statechain/spend_budget`, `/transfer/cancel` — `server/src/database/auth_nonce.rs:1-12`); every other mutating endpoint takes the static replayable `sha256(statechain_id)` signature. (12) **The `aggregate_xonly` backfill** as an operational obligation. **Nothing in the protocol detects a violation of 2, 3, 5, 6 or 8** — round 2 wrote "1, 2, 3, 5, 6 or 8"; 1 is now detected by construction. |
| **Round 2 — the migration tail is a FAMILY, not a one-off** | Every security-semantic migration is an additive column whose DEFAULT reproduces the **pre-feature** semantics for existing rows: 0002 `single_use DEFAULT false`, 0003 `epoch_deadline` NULL, 0005 `sig_budget` NULL, 0007 `derived_from` NULL, 0008 `partial_sig_issued`, 0009 `aggregate_xonly`. So a coordinator upgrade silently grandfathers every live coin into the weaker rules. Scope the backfill as a family. See **D20**. |
| **Round 2 — note for closure option (b)** | Binding `num_sigs` to an SE signature does **not** cover the aggregate. That is a second, separate binding. |

### D9 — Is the current serde shape frozen as the normative wire format, and how much ancestry must be disclosed?

| | |
|---|---|
| **Why it blocks** | A sender-declared `TransferMsg.protocol_version` selects the receiver's verification branch — 0 legacy, ≥2 flat ladder, ≥3 child bundle, 4 child-with-key-handover (`lib/src/transfer/mod.rs:125-131`, `lib/src/transfer/sender.rs:122-170`) — and the fail-closed floors (`MIN_PREPAY_PROTOCOL_VERSION`, `clients/libs/rust/src/transfer_receiver.rs:458-469`, `:496-502`) exist precisely because declaring version 0 once skipped the coin binding entirely. **None** of PROTOCOL.md, SPEC.md, CHILDREN.md, LIGHTNING.md or TRUST-MODEL.md mentions `protocol_version`, the version ladder, the minimum-version rule or the downgrade attack. The decision content: whether serde's defaulting becomes normative — notably that a MISSING `extension` key deserialises as `None`, flagged at `CATS-B-PHASE1-PLAN.md:93-96` as "absence is free to an attacker" — **and how much ancestry a bundle must disclose**, which is the open question underneath the ancestor value-conservation hole (see WP3). |
| **Options** | (a) Freeze the current serde shape, specify field by field, make absent-vs-null explicit and REQUIRED-to-reject where absence is currently free, and state a disclosure-completeness rule for ancestry. (b) Define a canonical encoding with presence bits and a required version tag; migrate. (c) Specify semantics, leave encoding implementation-defined. |
| **Recommendation** | **(a)**. (c) is not viable where two parties must agree on a signature preimage. (b) is right long-term and is a migration off the critical path. The non-negotiables: every optional field has a stated meaning for "absent", and the ancestry disclosure rule is written here rather than discovered later — a receiver cannot check that an ancestor split state's outputs do not exceed its funding without knowing how much ancestry it is entitled to demand. |
| **Round 2 — D9 is the *serde shape*; the version SELECTOR is now D16** | D9 as written does not touch the field that chooses which census runs. Split it out. The four sub-questions D9 does not ask — unknown-higher version, the v3/v4 child downgrade, which client profiles are conformant, and the one decoder rule — are now **D16**. |
| **Round 2 — one decoder rule fixes four formats at once** | "A decoder MUST reject a version it does not implement" simultaneously repairs the transfer message (**D16**), the recovery bundle (`version: 1` written at `clients/libs/rust-sdk/src/wallet.rs:230`, never inspected by `import_recovery_bundle` at `:249-289`), the invoice (**D11**) and the address (**D11**) — **all four carry a version field that nothing reads**. Write the rule once in §1 and cite it from §13, §23 and §24. |

### D10 — Is the laddered-coin blinded-MuSig residual an accepted limitation or a closure requirement?

| | |
|---|---|
| **Why it blocks** | `validate_backup_chain_v2` is documented in-source as `validate_signature_scheme` **minus** the `tx_n`-keyed `statechain_info` lookup and its `verify_blinded_musig_scheme`, "which cannot run for a laddered coin: the SE indexes `statechain_info` per CO-SIGN, and a laddered coin's tiers consume co-sign slots" (`lib/src/transfer/receiver.rs:295-311`). The stated residual: "the backup chain's blinded-MuSig commitments are not verified for a laddered coin … Closing it needs the SE's `tx_n` ↔ co-sign indexing to distinguish tier co-signs from backup co-signs". This sits directly under the section the roadmap otherwise calls the heart of the spec, and its only tracking pointer aims into `history/` — the folder readers are told is superseded. |
| **Options** | (a) Accept and state it in §14 as a named limitation, with the closure path named. (b) Treat as a closure requirement — which requires SE-side co-sign indexing, i.e. **excluded scope**. |
| **Recommendation** | **Commission the analysis in week 1 alongside D4.** What is actually lost by not verifying the blinding commitments, given that `ladder_decrements_by_interval` (INV-5) still rejects duplicates and inversions? If the answer is "nothing that the retained checks do not already cover", (a) is honest and cheap. If not, this is the **third** excluded-scope collision and the owner needs to know in week 1. Either way, restate the residual outside `history/`. |

### D11 — Interop encodings: freeze or fix before publication?

| | |
|---|---|
| **Why it blocks** | Wire compatibility between independent implementations happens at the invoice and the address, not at `TesrBundle` (which is conveyed inside a message a wallet decrypts from itself). The invoice is `utexoinv1` + `hex(JSON)` with **no checksum, no signature, and an expiry only the payer enforces** (`clients/libs/rust-sdk/src/invoice.rs:11-45`). The receive address derives from hardcoded `m/86h/0h/0h` (user) and `m/89h/0h/0h` (auth) with **coin type 0 on every network** (`lib/src/lib.rs:97-102`). Neither is covered by any normative document. |
| **Options** | (a) Freeze both as-is and specify them. (b) Add bech32/checksum and a version tag to the invoice before publication; decide coin type explicitly. (c) Leave encodings out of v1. |
| **Recommendation** | **(b)**. Freezing an unchecksummed, unversioned payment-request format in a v1 spec is the kind of thing that cannot be walked back, and field-typo losses in the wild are the predictable result. Coin type 0 on testnet/signet is either a deliberate normative choice or a defect — decide which and write it down. |
| **Round 2 — correction: the invoice IS versioned, and the fix changes shape** | `UtexoInvoice` carries `version: u8`, always written as 1 by both constructors (`clients/libs/rust-sdk/src/invoice.rs:11-40`) — and **neither `decode_utexo_invoice` nor `fulfill_utexo_invoice` ever inspects it** (`:83-97`). So the fix is "read the field and reject unknown values", not "add a version tag" — do not add a second version field beside an unread one. The **checksum** half of the round-1 claim is correct and worse than it reads: the only integrity checks are hex validity and JSON parseability, so a corrupted body either fails to parse or parses into a *different valid invoice*, and the fields that would change are `address` and `amount`. `expiry_unix` is payer-enforced only, and a `SystemTime` failure defaults `now` to 0, i.e. **every invoice becomes un-expired** (`:53`, `:74`). Add to (b): decide canonical JSON (serde field order is what any checksum would cover), decide whether `expiry_unix` is advisory or normative, and state the meaning of every absent optional field. |
| **Round 2 — the ADDRESS belongs in this decision at equal weight, for two reasons round 1 did not have** | (i) **`decode_transfer_address` PANICS on a short payload.** It bech32-decodes, checks HRP ∈ {`ml`,`tml`} and Bech32m, converts from base32, then indexes `decoded_data[0]`, `[1..34]`, `[34..67]` with **no length check** (`lib/src/lib.rs:45-62`). A valid Bech32m string under `ml`/`tml` with fewer than 67 data bytes crashes instead of returning `InvalidStatechainAddressError` — reachable from `validate_address`, whose entire job is to judge attacker- or typo-supplied input, and from `create_transfer_update_msg_with_branch` on the send path (`lib/src/transfer/sender.rs:193`). (ii) **Only two HRPs exist**: `MAINNET_HRP = "ml"`, `TESTNET_HRP = "tml"`, and `encode_sc_address` selects `ml` only for `Network::Bitcoin` (`lib/src/lib.rs:22-43`), so **testnet, signet and regtest are indistinguishable** and a signet address validates against a regtest wallet. That is a wrong-chain send with no client-side guard, and it is a **more likely source of real user loss than the coin-type-0 question** round 1 treated as the address-side issue. The address's version byte 0x00 is likewise written and destructured as `_` everywhere. Per-network HRPs are a breaking encoding change: cheap now, impossible after publication. |

---

### D12 — The automatic-exit policy: which clock does a wallet defend, and what is δ's budget?

*New in round 2. Candidate week-1 sitting item alongside D0.*

| | |
|---|---|
| **Why it blocks** | §4 already names liveness/monitoring as "the most implementer-dangerous omission after the crypto" but never turns it into a decision. Two concrete clauses are already sitting in the tree waiting for an answer. **(i) The deposit-anchored deadline never retires.** `auto_exit_due_inner` selects CONFIRMED non-carrier coins with a non-empty exit branch and, when `tip + margin ≥ est.exit_deadline_block`, calls `unilateral_exit` **unconditionally** (`clients/libs/rust-sdk/src/wallet.rs:1655-1662`, `:1706-1721`). That deadline is a pure function of the DEPOSIT's confirmation height and `initlock` (`:2652-2656` → `:2696-2705` → `:3332`); **nothing in that path reads whether F is spent or whether the coin's own trigger is confirmed**. In the triggered state the force is redundant-but-harmless (`exit_pass` is idempotent, `:3030-3033`) — so this is **not a bug fix, it is a request to encode a model the code cannot express**. A laddered coin runs on TWO clocks: an ABSOLUTE one (a prior owner's flat-backup nLockTime over F, deadline `H_deposit + initlock`) and a RELATIVE one (the CSV walk below T). Spending F retires the first *permanently* — the same bit `f_spender` reads (`clients/libs/rust/src/tesr.rs:7503-7620`). **(ii) δ is not a cushion, it is the entire head start.** `state_csv(k) = d0 − k·δ` with `δ = 36` (`lib/src/tesr.rs:210`, `:227-229`), and a superseded state must carry a *strictly higher* CSV than the live one over the same outpoint (`tesr.rs:610-618`). So the current owner's state matures exactly 36 blocks (~6 h) before the immediately-prior owner's — the same 36 for every hop. |
| **Four things already draw on δ and no document adds them up** | tower cadence (drivers are documented as once-per-block, `wallet.rs:3038`); the parent's own confirmation (`exit_wait_blocks` charges +1 per tx for exactly this, `lib/src/transfer/receiver.rs:713-722`); TRUC in-flight stalls on a spine (≥ ~s/2 blocks that no CSV accounts for — see D2/D6); and fee-bump latency — round 2 called it unbounded because the bump did not exist; at HEAD the bump exists (D5, round 3) but ships **off** (`fee_bump: None` on both presets, `clients/libs/rust-sdk/src/config.rs:351`, `:382`), so the term is unbounded on a default wallet and bounded by one package round-trip on a funded one, which is a difference the spec must state rather than average. A wallet that DEFERS an already-mature tier spends the head start block for block; a deferral of ≥36 blocks surrenders it entirely, after which the contest is a fee race a pre-signed, fixed-fee, un-bumpable tier cannot win. |
| **Options** | **(i)** (a) Keep force-driving unconditionally — then the spec MUST say a conforming wallet forces a unilateral exit at `tip + margin ≥ H_deposit + initlock` for **every** coin with a non-empty branch, irrespective of trigger state. That is a normative statement that undisturbed coins hit L1 on a schedule set by the deposit, which changes `PROTOCOL.md:619`'s footprint economics and appears in no user-facing doc. (b) Retire the deadline on trigger confirmation — then the spec must **in the same clause** mandate the handoff to `defend_ladders` (`wallet.rs:2013+`), or the walk strands mid-chain. **(ii)** Publish δ as a BUDGET with its four named consumers plus a normative **maximum deferral** (equivalently: a maximum tolerable tower-pass interval). |
| **Round 3 — the whole-coin clock now HAS a scheduled defender, and the order it uses is itself a normative candidate** | `deadline_safety_due` [D40 / A.2] runs from the background loop (`clients/libs/rust-sdk/src/wallet.rs:1873`) and defends `min(L_k)` in two remedies, in order: **re-anchor** (cooperative, ~112 vB, keeps the coin off-chain), and on failure **sever from `F`** by broadcasting the already-co-signed trigger, which is un-timelocked and therefore beats every retained rung by being valid first (`clients/libs/rust-sdk/src/refresh.rs:458-556`). The reasoning behind the fallback is the part a spec must carry: **the party asked to co-sign the re-anchor is the party that most wants the deadline to pass**, so a defence that can be declined by its adversary is not one. Its own doc-comment also records the shape of the bug it fixed — the safety half was sitting behind the *economics* flag `background_auto_refresh`, which is off by default, so a default wallet had **no** defender for this clock at all. Two residuals for D12 to decide rather than discover: carriers are excluded from **both** remedies (a plain re-anchor and a plain trigger each burn the allocation), and `coin_near_final` reads `coin.locktime` only — the deposit-anchored `auto_exit_due` clock below is still a separate, un-retired one. |
| **Recommendation** | **(i)(b) with the handoff mandated in the same sentence**, and **(ii) yes — publish the budget.** Mandating a deferral *policy* is implementation; mandating the *bound* is not. Without it an implementer reads `δ = 36` as slack and batches broadcasts into it. Blocks §15, not §5/§6. |

### D13 — Is the leaf's runtime deadline defence in v1 scope? — **ANSWERED (a); the port landed**

*New in round 2. The only round-2 finding that is a shipping defect in the v1-normative lane rather than a documentation or decision gap. **Round 3: it is no longer a shipping defect.** Recommendation (a) was taken — the near-deadline loop covers plain `ctesr-` children and spine tips, not only coloured ones — so what is left of D13 is the SPEC obligation (is near-deadline auto-exit REQUIRED of a conformant wallet?) rather than a code gap. The rest of this entry is retained because it is the only written statement of WHY the leaf needs it, and §9's security argument still has to say all of it.*

| | |
|---|---|
| **Why it blocks** | The leaf holder's security argument is **temporal**, enforced structurally **once, at admission**. A leaf carries NO flat backup (`CHILD_V2_BASELINE = 0`, `clients/libs/rust/src/tesr.rs:5321`), while every flat backup of the PARENT spends F at an ABSOLUTE locktime `L_k = L_0 − k·interval`. `min(L_k)` — computed by `epoch_deadline_from_flat_backups`, fail-closed on an empty chain and on locktime 0 (`tesr.rs:5607-5634`) — is the earliest height at which a prior owner of the parent can spend F and render the ENTIRE tree permanently unconfirmable. **Note who that is: the lowest rung belongs to the parent's LAST owner, i.e. the splitter** — exactly the adversary. `check_exit_headroom` refuses at claim time any conveyed leaf whose whole walk cannot complete before that height (`tesr.rs:5857-5866`, `lib/src/transfer/receiver.rs:741-760`). So what stops a prior owner voiding a leaf is **not economic** (the splitter has a positive-value claim on the tree, not a griefing cost) and **not structural** (nothing invalidates their backup) — it is a **deadline**, checked once. **None of this appears in any normative document**; `PROTOCOL.md`/`CHILDREN.md` do not state the epoch deadline at all. |
| **The defect** | After adoption, three runtime paths fail to make the exit happen for a **plain** leaf. (a) `watch_child_pass` is purely EVENT-driven on F — if F is spent by anything other than this coin's trigger it returns `WatchState::Void` (`tesr.rs:7846-7859`). That is **detection of the loss, not defence against it**. (b) The exported keyless watch bundle sets `deadline_block: u32::MAX` for a laddered coin on the reasoning that "an idle ladder never ages" (`clients/libs/rust-sdk/src/watchtower.rs:172-197`) — true of the ladder, **false of the parent's flat backups the leaf hangs under**. (c) The one pass that DOES compute `L_k − head_start` and force-exits **was** gated `Row::Child(cb) if cb.is_colored() => …, _ => continue`, with its own banner admitting it — so under D1's recommended RGB scope-out the v1-normative lane was precisely the lane with no runtime deadline protection. **Round 3 — (c) is FIXED, and two other things were fixed with it.** The gate is gone and its replacement banner states the exposure in the same terms this row does (`clients/libs/rust-sdk/src/wallet.rs:2317-2331`); the head start is now `exit_wait_blocks` rather than a hand-rolled `filter_map` sum that dropped `None` tiers and omitted each parent's confirmation block — i.e. it used to fire LATE, the fail-open direction, and to disagree with the claim-time gate that admitted the coin (`:2354-2370`); and the deadline is read as `min` over the parent's SIGNED flat-backup locktimes instead of `h_deposit + initlock` with a guessed `k`, which was late by `k·interval` (`:2371-2400`). (a) and (b) stand: `watch_child_pass` is still event-driven on `F`, and the exported bundle still sets `deadline_block: u32::MAX` for a laddered ROOT — correctly, since an idle ladder does not age; the leaf entries now carry a real height (see D19). |
| **Options** | (a) Port the coloured loop to plain `ctesr-` rows and specify near-deadline auto-exit as **REQUIRED** of a conformant wallet. Both bundle shapes persist under `ctesr-` and both carry `parent_flat_backups` (`tesr.rs:5348`), so this is a port of existing, tested code. (b) Specify it as RECOMMENDED and name the exposure in §1. (c) Specify the leaf as admission-gated only, and state plainly that a leaf whose owner goes offline past `min(L_k) − Σcsv` is **forfeit to the splitter**. |
| **Recommendation** | **(a).** (c) is a property most readers will not accept from a system whose `README.md:7` claims every coin stays unilaterally exitable. Blocks §9's security argument (see §5). |
| **Round 3 — the code half of (a) is done; the decision half is not** | The port landed, so §9's security argument no longer describes a property the shipped plain lane lacks and **§9 is unblocked**. What still needs deciding in one sitting: whether the pass is **REQUIRED** or RECOMMENDED of a conformant wallet, and — the question the port surfaces — what a wallet must do when it is **BLIND** on a leaf. The implementation's answer is worth adopting verbatim: it returns `Err` rather than a quiet `Ok`, because `Ok` is read upstream as "this wallet is protected", and it deliberately does **not** force-materialise a blind coin on suspicion, since one electrum blip would otherwise dump every off-chain coin in the wallet on-chain (`clients/libs/rust-sdk/src/wallet.rs:2434-2451`). Fail-closed for a WATCHER means refusing to claim protection, not spending money on an unverified premise — that is a normative sentence, not an implementation detail. |

### D14 — What is the receiver's normative margin law? — **TAKEN AS (a), AND SHIPPED**

*New in round 2. Sibling of D6 — it moves what a receiver ADMITS. **Round 3: the decision was taken and the law is enforced at HEAD**, in the stronger per-kind form. WP3c is complete; §7 may now state the δ MUST, because the reference implementation enforces it.*

| | |
|---|---|
| **Why it blocks** | The audit's H-2 had two halves. Half (a) — sender-supplied `TesrParams` — is **CLOSED**: `cap_schedule` compares the conveyed schedule field by field against the receiver's own network preset and refuses by name on the first disagreement, at all three lanes (`clients/libs/rust/src/tesr.rs:5117-5173`; `transfer_receiver.rs:647`, `:1369`; `tesr.rs:5845`). Half (b) was **STILL OPEN at `280ed88`** (closed at HEAD — see the round-3 row): the only relational constraint the receiver enforced was `sup.csv > live_csv`. A patched sender can therefore present `S_old` at 145 and the receiver-paying `S'` at 144 — both inside the honest mainnet band `[144, 1440]` — collapsing the receiver's designed **δ = 36 (~6 h)** race window to **one block**. δ is enforced *exclusively by the honest builder*: `next_rival_state_csv` takes the lowest rival and subtracts `p.delta` (`tesr.rs:7121-7164`), and that is client-side. The SE is blind, so it cannot see, let alone constrain, a chosen CSV. A repo-wide grep for `p.delta` finds it only in build-side subtractions and in `cap_schedule`'s field table (`:5150`). |
| **Options** | (a) **Relational**: require `sup.csv ≥ live_csv + δ`. Minimal, and provably satisfied by honest bundles because the builder emits exactly `lowest − δ` (`tesr.rs:7152-7154`). (b) **Schedule-grid membership**: require every live/superseded state CSV on `d0 − k·δ` and every extension on `e0 − m·δE`. Strictly stronger — it also refuses off-schedule tiers, the class the child-renewal lane already refuses (`:19239-19249`, `:19385-19399`) — but **not obviously safe on the root lane**: the split lane deliberately pins `SP` at `SPINE_CSV = 0`, which is off-grid and outside `[d_floor, d0]` and already needs its own band (`:8777-8783`, `:5346`), and rollover/renew shift the rungs. |
| **Recommendation** | **(a) now, (b) named as v2, with the `SPINE_CSV` exception recorded explicitly in the text** since it is the reason (b) is not free. (a) is additive-safe, so a later strengthening retracts nothing. (b) is **not** decidable in a sitting — it needs an enumeration of honest rung sequences under rollover and renew. Implementation is **WP3c**. Note this is not a theft against a watching receiver — `S'` still wins the maturity race if broadcast — but it collapses the whole safety margin the ladder's liveness argument rests on, and §7 would otherwise state a δ MUST that the reference implementation does not enforce. |
| **Round 3 — SHIPPED, and per-KIND rather than per-δ** | `verify_superseded_segment` now refuses unless `sup.csv >= live.csv + margin`, where `margin` is `δ` for a live **state** and `δE` for a live **extension** (`RivalKind::margin`, `clients/libs/rust/src/tesr.rs:10511-10516`; the refusal at `:10751-10767` states the arithmetic — "A one-block lead is not a margin; it is the rounding error a reorg or a slow relay erases"). **Two design points the spec must carry, because both are load-bearing and neither is obvious.** (i) **The kind is read off the LIVE rival, STRUCTURALLY** — never off which conveyed list the superseded tier arrived in. Keying it on `sup.kind` would be a margin the sender selects: file a superseded STATE under `superseded_extensions` and buy `δE` instead of `δ`. Mainnet is immune only by the coincidence `δ = δE = 36`; regtest (6 vs 3) is where it shows, and a unit test asserts the two budgets stay different on at least one shipped preset so the per-kind form cannot become decorative (`:13483-13521`). (ii) **The law is only satisfiable while `d_floor >= δ` and `e_floor >= δE`** — the tightest case is the spine, where `SP` is live at `SPINE_CSV = 0` and the caps it retires sit at the floor. Both shipped presets clear it (mainnet 144 ≥ 36; regtest 6 ≥ 6, with **no slack at all**), so a future schedule tuned below the floor would make receivers refuse honest CATS bundles at claim time, on the payee's side, with the sender already committed. Publish the inequality with the law. Ordering is pinned too: the relayability refusal must precede the CSV race check, guarded by a source-scanning test that was itself repaired when D14 moved the line it pinned (`:24812-24843`). |

### D15 — Transfer finality: is a conveyed-but-unclaimed transfer reversible, and for how long?

*New in round 2. The residual of audit C-14 that survives every fix that landed.*

| | |
|---|---|
| **Why it blocks** | The coordinator's answer to "when is a payment final?" is **at CLAIM, not at conveyance** — and no normative document says so. A non-batch transfer's lock lives one hour off `updated_at`; a batch transfer's lives until `MAX(lightning_latch.expires_at)` (`server/src/database/transfer_sender.rs:88-102`). Cancellation is now an explicit, consented, audited operation rather than a silent overwrite — `POST /transfer/cancel` (`server/src/endpoints/transfer_sender.rs:319`), `cancelled_at` on the row, `recipient_key_was_cancelled` refusing reuse of a cancelled recipient key (`:163-186`), `latch_claim` as mutual exclusion between claim and cancel (`server/src/database/transfer_cancel.rs:19`, `:228-241`), and `280ed88` re-routing redirection through cancellation instead of overwriting. That materially improves the story but **reframes the question rather than answering it**. `clients/libs/rust/src/tesr.rs:364-375`'s [F1] comment still claims the lockout is PERMANENT — still false. |
| **What the spec must state** | (i) a mailbox payment is not final until claimed; (ii) the exact window, per lane; (iii) which party may cancel and with whose consent; (iv) that a receiver UI **MUST NOT** present a conveyed-but-unclaimed transfer as received — `peek_pending_transfers` already presents them that way to the SSP. |
| **Options** | (a) Specify claim-time finality with the current windows plus the (iv) obligation. (b) Tie the lock's life to `key_updated` rather than a wall clock. (c) Terminalize plain-lane children as the latched lane already does. |
| **Recommendation** | **(a)**, with (b) named as v2. Writing the spec without deciding this either omits payment finality entirely or commits to "payments are reversible for one hour" — exactly the kind of statement that has to be retracted. **Related open coordinator defects to fix alongside, not to decide:** `/transfer/unlock` verifies the receiver-side signature under a **caller-supplied** public key never compared to `new_user_auth_public_key` (audit C-6, `server/src/endpoints/transfer_receiver.rs:137-141`); and **nothing binds a latched coin's `statechain_transfer.batch_id` to its `lightning_latch.batch_id`** (audit C-8 — the guard at `server/src/endpoints/transfer_sender.rs:69-83` enforces only the inverse direction, and `execute_pay` never compares them, `clients/libs/rust-sdk/src/ssp.rs:384-519`). |

### D16 — The `protocol_version` selector: floor, ceiling, and forward compatibility

*New in round 2. Split out of D9, which covers the serde shape but not the field that chooses which census runs. Cross-references D9's downgrade note.*

| | |
|---|---|
| **Why it blocks (ceiling)** | **Every** comparison against `TransferMsg.protocol_version` in the repo is one-sided — `>= 2`, `>= 3`, `>= 4`, `< 2`, `< 3`, `< 4` — with **no upper bound and no unknown-version reject arm anywhere** (`lib/src/transfer/mod.rs:112-131`; `clients/libs/rust/src/transfer_receiver.rs:499`, `:681`, `:1141`, `:1181`, `:1254`, `:1325`, `:1513`). A message declaring version 99 takes every `>=` arm exactly as a version-4 message would, with unknown fields serde-defaulted. There is also **no version negotiation of any kind**: a grep for `negotiat` outside docs returns nothing, and neither the address nor the invoice carries a capability field. So today's rule is "process as the highest version I know, ignore what I do not understand" — the one rule a security-relevant version selector must not have, because a v5 adding a REQUIRED check is silently downgraded by every v4 receiver. |
| **Why it blocks (floor)** | On the child lane the two live versions are **not equivalent** and the SENDER picks. `protocol_version = 4` carries the key handover (SE share rotated, auth re-pointed, sender permanently locked out — the child becomes a first-class coin) AND a `verify_transfer_signature` binding the sender's authorisation to THIS recipient over the child's funding outpoint (`clients/libs/rust/src/transfer_receiver.rs:1181-1200`). **`protocol_version = 3` carries neither**, and the receive path takes an explicit legacy branch — `persist_child`, best-effort `unlock_statecoin`, `status = CONFIRMED`, a "Receive" activity, early return, **no `/transfer/receiver` call** (`:1512-1543`). The enforced floor is 3, not 4 (`:469-473`, `:1141-1147`), and there is no receiver-side policy, config flag or minimum that can demand 4. So a malicious payer conveying at v3 gets a receiver who **books the payment as received while the sender retains both the child auth key and the SE share**; one hour later the non-batch pending lock lapses and the sender can co-sign a rival child state. The v4 arm's own comment states the consequence: "v3 carries no such signature at all, and a mitigation is not the binding". |
| **The flat lane, by contrast, is defended — by an argument, not a check** | The claim path deliberately does NOT impose an unconditional floor of 2 (that would brick legitimately un-laddered coins); it refuses only the version/payload MISMATCH — declared `< 2` while `tesr_ladder` is present (`:1237-1263`). The defence against "declare 0 on a laddered coin" is that a laddered coin's tiers each consume an SE co-sign slot, so `num_sigs > backup_transactions.len()` permanently, and padding the backup vector is defeated inside `validate_signature_scheme` by INV-5's exact-interval decrement plus `verify_blinded_musig_scheme` (`lib/src/transfer/receiver.rs:295-311`). **The argument is coherent and is the right design** — note in particular that the `< 2` arm runs the FULL legacy verifier *including* the blinded-MuSig check D10 says cannot run on the `>= 2` path, so the two lanes' residuals do not compose into a hole. But two premises are external: the SE's `num_sigs` must be honest (**D8**), and the coin must carry at least one tier. And the **tested article is the sender-side refusal** (sdk71, `clients/tests/rust/src/sdk71_unconditional_ladder.rs:12`, `:234`), not a receiver fed a hand-crafted version-0 message over a genuinely laddered coin. |
| **Options** | **(ceiling)** (a) MUST-reject unknown-higher — fails closed, needs a coordinated flag day. (b) Process-as-highest-known — needs a written rule that no future version may add a check whose absence is exploitable. **(floor)** (c) v3 remains a normative supported conveyance, with §Children stating that a v3 child is an exitable claim, not a first-class coin. (d) 4 is the floor; delete the legacy branch. |
| **Recommendation** | **(a) + (d).** (b) is a promise about all future versions that nobody can keep. (d) is the surviving theft half of audit C-14 and it changes what a child **is**, which is why §Children cannot be written without it. **Also in scope of this decision: which client profiles are conformant.** The uniffi FFI silently strips `protocol_version`, `tesr_ladder` and `child_tesr_bundle` in BOTH directions — `ffi_to_transfer_msg` hardcodes `protocol_version: 0, tesr_ladder: None, child_tesr_bundle: None` — so the boundary performs the downgrade itself. It fails closed (for the reason above) but **silently and with a misleading reason**: the user is told "num_sigs is not correct" about an honest transfer. And both JS clients' fail-closed predicate omits `child_tesr_bundle`, so the belt-and-braces the flat lane has, the child lane does not. |
| **Round 3 — the profile half is half fixed, and a NEW divergence opened underneath it** | **Fixed:** the FFI is now lossy in one direction only and says so — `transfer_to_ffi_msg` **REFUSES** a laddered message by name rather than truncating it, on the stated ground that a flat reading of a laddered coin is a value-loss path, while the inbound direction's `protocol_version: 0` is an honest reading of a type that has no field for laddered material (`lib/src/unifii_interface.rs:45-90`). **Fixed:** both JS predicates now include `child_tesr_bundle` (`clients/libs/nodejs/transfer_receive.js:181-194`, `clients/libs/web/transfer_receive.js:229-233`). **New, and it moves the profile question:** the Rust client now demands a nonce-bound enclave attestation over `num_sigs` and refuses without one (D8, round 3) — and **neither JS client sends `attestation_nonce` or looks at the attestation** (zero occurrences of `attestation` under `clients/libs/nodejs` and `clients/libs/web`). Their flat `num_sigs == backups.length` check therefore runs against a **coordinator-asserted** count, which is exactly the value the Rust lane stopped trusting. That is now the sharpest statement of why the Rust SDK is the conformant profile, and it belongs in §1 in place of the softer "they fail closed on laddered coins". The **ceiling** half of D16 is unchanged: no upper bound and no unknown-version reject arm exists for `TransferMsg.protocol_version`, and the floor is still 3, not 4. |

### D17 — What binds the latch clock to the Lightning HTLC deadline — and is the terminalization carve-out still earned?

*New in round 2. Blocks App. B §6 and §8, not the sats lane.*

| | |
|---|---|
| **Why it blocks (the clock)** | `LIGHTNING.md`'s entire RECEIVE safety argument (§3, §8) is "**the latch expires BEFORE the payer's HODL HTLC**". **Nothing in the code establishes or checks that relation.** The RECEIVE latch is a hardcoded wall-clock 3000 s whose justifying comment compares it to 3600 s (`server/src/endpoints/lightning_latch.rs:83-95`) — but 3600 is the BOLT11 **invoice expiry** passed to `/lninvoice` (`clients/libs/rust-sdk/src/ssp.rs:608`, `:636`, `:694`), which bounds when the invoice may be PAID; once paid, the held HTLC's deadline is its **CLTV expiry in blocks**, a quantity this repo never reads (`DecodedInvoice` carries amt/hash/asset only, `ssp.rs:98`; a grep for `cltv` in `clients/libs/rust-sdk/src` returns nothing). The PAY latch is a hardcoded 90000 s = 25 h (`lightning_latch.rs:201-204`). And **`unlock_by_preimage` performs no expiry check at all** (`:214-282`), while `validate_batch` refuses the matching claim as expired (`server/src/endpoints/transfer_receiver.rs:189-208`) — **so the SSP can pay, unlock, and be unable to claim.** |
| **Round-1 note corrected** | `LIGHTNING.md §9` residual 3 is **stale**: `has_open_transfer` no longer uses a hardcoded hour. `OPEN_TRANSFER_WINDOW_SQL` is a CASE keying any batched transfer off `COALESCE(MAX(lightning_latch.expires_at), batch_time + timeout)` (`server/src/database/transfer_sender.rs:94-102`). **Half of the designed `lock_expiry` reconciliation shipped under a different name**; the half that did not ship is the one that matters. |
| **Why it blocks (terminalization)** | The carve-out is mandatory in code, not optional: `in_ladder_pay` calls `set_spend_budget(piece_sid, 0)` **whenever a latch batch exists, and only then** (`clients/libs/rust-sdk/src/transfer.rs:1611-1625`). Its stated justification — "the lock expires with the batch" — is **no longer true**, precisely because of the CASE above: the lock now expires with the LATCH, i.e. exactly when the claim window closes. So the gap the carve-out was built to cover may no longer exist, and the carve-out may now be pure cost (it is what makes a latched piece unreclaimable). Worse, the same argument applied to the **exact whole-coin PAY lane** would demand a carve-out there too, **and there is none** (`ssp.rs:1008-1040` latches and conveys the whole coin with no `set_spend_budget`). Either the post-expiry rival window is closed by the lock (⟹ the child carve-out is redundant) or it is not (⟹ the exact lane is exposed). **Today the two PAY lanes make opposite choices with one argument between them.** |
| **Options** | **(clock)** (a) Read the invoice's CLTV and enforce `latch_expiry < CLTV deadline` at latch creation, publishing the inequality as normative. (b) Publish the wall-clock windows as configuration with the failure mode named. (c) Defer, with §8 rewritten conditionally. **(terminalization)** (d) Drop the carve-out. (e) Retain it and extend it to the exact whole-coin lane. (f) Retain it and write down why the pending lock suffices for the exact lane. |
| **Recommendation** | **(b) for a v1 appendix, (a) before LN is ever normative**; and **decide (d)/(e)/(f) as a 1–2 day item (WP11)** — it is the only LN finding that is both a genuine open decision and cheap to close, and §6 cannot be written normatively until it closes. |

### D18 — The error surface: stable codes, or frozen prose?

*New in round 2. Prerequisite for §26 conformance vectors.*

| | |
|---|---|
| **Why it blocks** | **There is no machine-readable error taxonomy anywhere.** Coordinator: every refusal is `status::Custom(Status::X, Json(json!({"message": "<English sentence>"})))` with **no `code` field on any response** (`server/src/endpoints/sign.rs:65`, `:99`, `:113`, `:135`, `:152`, `:363`). Client SDK: exactly six typed `SdkError` variants, all economic/admission (`clients/libs/rust-sdk/src/types.rs:96-143`); every protocol refusal in the verifier, the ladder builder, the census and the journals is a bare `anyhow!` string, and the one named protocol error is a magic string constant, `ERR_LADDER_COSIGN_INCOMPLETE = "ladder-cosign-incomplete"` (`clients/libs/rust/src/transfer_sender.rs:264`). `SPEC.md:694-716` numbers ERR-1..ERR-16 but each entry maps to a prose string plus an HTTP status, ERR-11 is skipped with no note, and there is no error for cancel, depth-cap, headroom or dust refusals. Meanwhile **82 `contains("…")` assertions** in `clients/tests/rust` already bind the prose as an interface — including semantically load-bearing matches like `contains("epoch")` and `contains("below the required") && contains("10000")`. **The de-facto wire contract for errors is unversioned English.** |
| **Options** | (a) Freeze the strings verbatim as normative — viable, ugly, and it makes every message edit a breaking change. (b) Introduce a stable `code` field alongside `message`, renumber ERR-n against it, re-point the tests. |
| **Recommendation** | **(b)**, sized *with the test churn*, not as a schema edit. §5's note on §19 currently asks only to "complete the error taxonomy", which treats a missing-numbers problem as the whole problem. An independent implementation cannot conform to prose. |

### D19 — The watchtower obligation: what must an implementation watch, how often, with what margin?

*New in round 2. Replaces "§15 is survey-first" — the survey is done and it produced one decision with four numbers in it, not text.*

| | |
|---|---|
| **Why it blocks** | Four things a spec MUST pin are today two-of-four in code. **(1) MARGIN**: the delegated `watch_pass(bundle, electrum, margin_blocks)` takes the margin as a bare caller parameter with **no default and no guidance**, and has **zero non-test call sites** — it is a library entry point nobody drives (`watchtower::watch_pass`, `clients/libs/rust-sdk/src/watchtower.rs:443`; re-verified at HEAD in round 3, and not to be confused with `tesr::watch_pass`, which the in-process defender does drive). The in-process tower has a genuinely derived margin (`auto_exit_margin_blocks_for`, default 860 = 14·10 + 5·144, documented as DERIVED not chosen, `clients/libs/rust-sdk/src/config.rs:237-281`) — but nothing connects the two numbers. **(2) CADENCE**: stated nowhere; the only mention is that a trigger's `csv_blocks` is "the number an operator needs to size their polling interval" — guidance to a human, not an obligation. **(3) STALENESS**: the bundle is a snapshot and the re-export obligation exists only as a doc comment (`watchtower.rs:25-26`, `:88-97`); `WatchBundle` carries no height, epoch or coin-count digest. **(4) REPORTING**: see below. |
| **The three-level asymmetry the spec must resolve** | Coins of the same shape now get three different levels of protection, deliberately. **(i) Reactive, per block, in-process**: `defend_ladders` (wired from `start_background`, `clients/libs/rust-sdk/src/wallet.rs:1526`, `:1579`) covers laddered roots and adopted children on **both** lanes via `watch_child_pass` (`clients/libs/rust/src/tesr.rs:7819-7870`) — this is new since round 1 and §4's original row implied it did not exist. **(ii) Near-deadline, with a Σcsv head start**: COLOURED children and coloured spine tips **only** — this was **D13**, and **round 3 closes it**: the pass now runs on plain rows too (`wallet.rs:2317-2331`). **(iii) Delegated, keyless**: round 2 found `export_watch_bundle` covering laddered ROOTS via a `WatchTrigger` but found **no `ctesr-` read in the export loop**, and marked the row UNVERIFIED pending a full read. **Round 3 resolves it: (iii) is closed.** [S7] added a leaf entry for adopted children (`ctesr-`) and spine tips, with `deadline_block = L_k − head_start` computed from `child_exit_chain_bound` / `spine_tip_exit_chain_bound` and `epoch_deadline_from_flat_backups` — never from the declared timelocks, which are attacker-supplied on conveyed material (`clients/libs/rust-sdk/src/watchtower.rs:87-141`, `:207-240`, `:373-400`). A unit test asserts a leaf is never exported at `u32::MAX`, the sentinel that would disable its height predicate (`:596-603`). So `TRUST-MODEL.md:259-260`, `:391`'s "both coin shapes" claim is now TRUE rather than aspirational. **The asymmetry that remains is a different one**: the whole coloured CARRIER is excluded from every automatic pass — `auto_refresh_due` and `deadline_safety_due` skip carriers because a plain re-anchor or a plain trigger burns the allocation, and the primitive that would serve them, `colored_reanchor` (D1, round 3), has **no scheduled caller**. A built, unscheduled remedy is the same operational outcome as an unbuilt one, and it should be named that way rather than counted as coverage. |
| **Two false-positive classes that make "the tower is loudly wrong" indistinguishable from "the tower is right and the race is on"** | (a) On the laddered lane the fired trigger pushes the **whole CSV-gated tier chain** at once; the tiers are relative-CSV, so the second broadcast is rejected as non-final on every pass until maturity, and `tolerable_rebroadcast` matches only already-known/already-mined strings (`watchtower.rs:322-366`) — a **healthy** defence therefore emits `Acted{failures:[…]}` every pass for up to `csv_blocks` blocks, while the doc-comment tells the operator a rejected broadcast means the exit is being RACED. (b) The laddered entry watches F for **any** spend, so a cooperative withdraw or an on-chain re-anchor fires the trigger and the tower pushes a now-permanently-dead chain forever. |
| **Options** | (a) A normative margin formula plus a maximum polling interval expressed as a fraction of the smallest CSV in the bundle; a REQUIRED re-export trigger set plus a freshness field; and a third reporting state (tolerable-and-pending) distinct from failure. (b) Specify the in-process tower only and declare delegation out of scope for v1. |
| **Recommendation** | **(a)**, moved into weeks 2–4 alongside D5/D6. §4's warning — "an implementation built from a ladder-only spec would be structurally correct and still lose funds" — has a named citable mechanism now: the missing margin and cadence. **What the keyless tower canNOT do is the strongest part of §15 and should be written as normative properties**, not rediscovered: the bundle contains no private material by construction (unit-test-asserted); the worst a malicious tower can do is broadcast EARLY, settling the owner's own coin to the owner; token carriers are exported **without** `backup_tx` so the token-destroying sweep is structurally impossible; several independent towers are safe because they broadcast identical pre-signed transactions; the export fails **closed** (an unreadable ladder, an uncomputable deadline or an unenumerable carrier set aborts it, ordering pinned by sdk72 C6); and `WatchState` distinguishes Idle / Blind / Acted (`watchtower.rs:8-41`, `:136-208`, `:358-392`). |

### D20 — Coordinator protocol migration and capability discovery

*New in round 2. Answers "how does a deployed coordinator move between protocol versions?" — the answer today is: **there is no story at all**.*

| | |
|---|---|
| **Why it blocks** | `sqlx::migrate!("./migrations").run(&pool).await.unwrap()` runs unconditionally at boot and panics the process on failure (`server/src/main.rs:106-109`). There are **no down migrations and no schema-version gate on request handling**. Every security-semantic migration is an **additive column whose DEFAULT reproduces the pre-feature behaviour** for rows that already exist — 0002 `single_use NOT NULL DEFAULT false` (existing coins keep unrestricted re-signing), 0003 `epoch_deadline` NULL = co-signable indefinitely, 0005 `sig_budget` NULL = no terminal-spend enforcement, 0007 `derived_from` NULL, 0008 `partial_sig_issued`, 0009 `aggregate_xonly` backfilled forward only. **So upgrading a live coordinator silently grandfathers every existing coin into the weaker semantics** — and for 0009 that is *permanent un-ladderability* (the tail D8 already tracks). Migrations **0010 (claim latch) and 0011 (`sender_auth_xonly_public_key`) are security-semantic and post-date every round-1 survey.** Nothing lets a client discover which protocol features a coordinator implements: `/info/config` publishes `initlock`, `interval`, `batchtimeout` and a `version` string that is the **crate build identifier** (`server/src/endpoints/utils.rs:187-203`). Combined with D16's absent client-side negotiation, **protocol evolution today is a flag day**. |
| **Options** | (a) Specify a capability / feature-discovery mechanism (a distinct `protocol_version` plus a feature list on `/info/config`). (b) State normatively that coins minted under an older coordinator retain their original semantics **for life**, with a per-column enumeration of what that means. (c) Both. |
| **Recommendation** | **(c)**. (b) alone is honest but leaves a client unable to tell which world it is in; (a) alone leaves the grandfathered coins unspecified. Either way, state in §19 that the published `version` is informational and **MUST NOT** be used for compatibility decisions until it is split. This is §25's second blocker — D9 is the *client* wire format and is a different problem. |

---

## 3. Work packages, in dependency order

| WP | Title | Depends on | Effort | Observable completion gate |
|----|-------|-----------|--------|-----------------------------|
| **WP0** | Freeze + DECISIONS.md | — | 1 wk (sitting + write-up) | `docs/utexo/DECISIONS.md` has one ADR per decision, each naming the doc sections and code sites it binds. A written freeze rule exists and one exception has gone through it. No entry reads "we will decide when we write it". |
| **WP1** | Week-1 unknown reducers (run in parallel) | — | 1–2 wks | ~~(1) D4 proof-or-refutation~~ — **delivered in round 2, see D4**; what remains of it is one adversarial test conveying a child over an **already-triggered** parent, because `verify_conveyed_child` rests that claim on a comment (`clients/libs/rust/src/tesr.rs:5641-5658`). (2) D10 analysis with a verdict — **still open, and still the item that could turn out to be an excluded-scope collision**. ~~(3) a P2A anchor-spending path that bumps a tier on regtest with `minrelayfee` above 2 sat/vB~~ — **DONE (round 3)**: the builder, the Core-RPC package backend and the wiring into both broadcast loops all exist, the run is recorded in `docs/utexo/notes/WP1-TRUC-P2A-SPIKE.md`, and D31 answered who funds it. ~~(4) a one-page answer to "is `sig_count` authenticated end to end?"~~ — **ANSWERED, and then built**: it was not, and it now is, nonce-bound and fail-closed (see D8). ~~(5) survey of the seven unsurveyed subsystems~~ — **six delivered in round 2**; remaining: cryptographic constructions, the SE contract, deposit/onboarding, authorization, the confirmation/reorg half of the chain model, and mailbox availability. **(6) NEW — the TRUC live run** (2 days, shares a fixture with (3)): broadcast three consecutive zero-CSV v3 tiers on regtest and record Core's rejection string; submit one 1P1C P2A anchor package. Gate: the spec's sequential-submission and in-flight-window clauses cite a live run rather than a reading of Core policy. **This retires the single largest UNVERIFIED block in the round-2 corpus** — no test in the tree has ever put two transactions of one exit chain in a mempool simultaneously (the harness mines between every tier, `clients/tests/rust/src/sdk69_transfer_many_inladder.rs:114`). |
| **WP2** | Reproducibility spine | — | 1 wk + | `cargo test -p ci-guards` green at HEAD and red again if `clients/libs/rust-sdk/src/tokens.rs:1522`'s `.ok()?` is restored. A workflow triggers on `feat/spark` and its log shows 528 unit tests, not 118 (`clients/tests/run_all_suites.sh:79-87` runs only `-p mercury-utexo-sdk`). Deliberately breaking one assertion in sdk40 and one in sdk74 reports exactly those two as FAIL. An unknown `SDK_E2E` number errors instead of falling through to the legacy suite and printing "completed successfully" (`clients/tests/rust/src/main.rs:622-631`, `run_all_suites.sh:74`). No `/Users/gofman` paths remain (`run_all_suites.sh:21`, `sdk21_remote_sspclient.rs:79`). **A clean clone on a second machine or in a container builds and reproduces the unit count**, with the run record committed. |
| **WP3a** | Fee-rate binding (the (B) half of the value-law holes) | D5 | 2–4 days | `bundle.fee_rate` bound to the network constant in `verify_bundle_ex`/`verify_bundle_bound` and `verify_child_bundle`, mirroring `clients/libs/rust/src/tesr.rs:5769-5776`. The two tripwires — `gap_a_forged_yardstick_root_ladder_is_still_accepted` (which today asserts a genuinely co-signed ladder declaring 2662 sat/vB over a 1,000,000-sat coin is ACCEPTED, delivering 1,030 sat to the owner and 998,970 to miners, panicking "GAP A HAS BEEN CLOSED" at `tesr.rs:15566` if fixed) and its child-lane twin — are inverted to assert refusal and pass. `honest_root_ladder_is_accepted` and `honest_child_bundle_is_accepted` still pass. `assert_not_an_unrelated_refusal` passes on each new refusal. A new E2E drives a forged rate through the live claim path (`clients/libs/rust/src/transfer_receiver.rs:661`) and the claim fails. **Round-2 warning — `cap_schedule` does NOT close this.** `cap_schedule` binds `params.committed_fee_rate` (`clients/libs/rust/src/tesr.rs:5162-5171`); every value law actually reads the **top-level `bundle.fee_rate`** (`:253`), a different field. A reader who saw `cap_schedule` land could plausibly drop this HARD PUBLICATION GATE; it is still open at `280ed88` and the tripwire still asserts ACCEPTANCE (`:15517-15576`). All three entry points accept the forged bundle, so the binding goes **inside `verify_bundle_ex`**, not into a caller. The child lane has the binding only in the async wrapper (`:5762-5775`); sync `verify_child_bundle` inherits nothing (GAP B, `:15578-15599`). |
| **WP3b** | Ancestor value conservation | D9 | with D9 | An ancestor split state's outputs may not exceed its funding — which requires the ancestry disclosure rule from D9 and root-anchoring to the on-chain F. The `gap_an_ancestor_split_state_may_mint_value_out_of_nothing` tripwire (`tesr.rs:16486`) is inverted; `an_honest_one_tier_spine_ancestor_is_accepted` still passes. |
| **WP3c** ✅ | **NEW (round 2)** — receiver-side δ exit-race margin — **DONE (round 3)** | **D14** | *(spent)* | `verify_superseded_segment` now requires `sup.csv >= live.csv + margin`, per-kind, read off the LIVE rival (`clients/libs/rust/src/tesr.rs:10751-10767`). Two of the three gate items are met by in-crate tests — the per-preset satisfiability law (`:13483`) and the check-ordering guard (`:24812`). **The one gate item NOT met:** no behavioural attack test builds a 145/144 pair and asserts the named refusal; a repo-wide search for the refusal's own wording finds only the production string. Move that single item to WP9 rather than treating the work package as open. |
| **WP4** | Parameter and profile reconciliation + aggregate backfill | D7, D8 | ~~1 wk~~ **days** | ~~A unit test asserts `TesrParams::for_network` for bitcoin/mainnet/testnet/signet/regtest explicitly, with no silent default. One normative `lockheight_init` profile~~ — **both DONE (round 3, D25/D8(f))**: every network is named, an unknown one panics, testnet/signet take the mainnet schedule, and `lockheight_init` is compiled in per network with the coordinator's copy demoted to a cross-check that refuses on disagreement. What is left of WP4 is the documentation half and the backfill. The phantom depth cap of 8 is gone. A constants-consistency script (wired into ci-guards) greps `docs/utexo` for `124`, `1306` and the phantom `8` and is green — `TIER_VBYTES` is 125 (`lib/src/tesr.rs:93`) and `min_child_value` 1310 (`:134-141`, pinned at `:973`), stale at ~12 doc sites including `SPEC.md:138`, which is SPEC's *definition* of a tier. Zero NULL `aggregate_xonly` rows. |
| **WP5** | Exit cost re-derivation + admission-rule change | D2, D6, D7, D3 | 2–3 wks incl. test churn | One test derives txs, vbytes AND wait from **measured** transactions at depths 0..3 through the production finaliser and fails if any of the three drifts (extend the pattern at `clients/libs/rust-sdk/src/invalidation_model.rs:326-360`, which already re-derives coefficients from measured vsizes). `max_exit_txs`/`max_split_depth` and every refusal test pinned to them are updated together. A grep for the old literals (`3 + 2d`, `293d`, "60 days", `2,160·(d+1)`) across `docs/utexo` returns zero outside `history/`. `estimate_exit_cost` returns a non-zero laddered figure (`SPEC.md:542-547` records that it measures un-laddered material only). |
| **WP6** | The price of the RGB scope-out | D1 | 1 wk + 1 day | (i) The receiver-side refusal exists: a conveyed transfer bearing both a plain ladder and an off-chain-commitment envelope is rejected, with a test. (ii) `cargo build` succeeds from a clean clone plus a **tagged** rgb-lib revision — no filesystem-path dependency in any manifest (`clients/libs/rust-rgb/Cargo.toml:15-16`; the git+rev form is already written in the comment one line above). This second item is urgent independent of spec scope: three methods mercury calls exist only in an uncommitted working tree. |
| **WP6b** ✅ | **NEW (round 2)** — small interop fixes, before any encoding is frozen | D11, D16 | ~~1–2 days~~ **hours** | **ROUND 3 — all four are DONE**: (i) the decoder bounds-checks `decoded_data.len() < 67` before slicing (`lib/src/lib.rs:58-63`); (ii) `decode_utexo_invoice` and `import_recovery_bundle` both **probe the version field first** and refuse an unknown one by name, each with a `rejects_unknown_version` test (`clients/libs/rust-sdk/src/invoice.rs:14-58`, `clients/libs/rust-sdk/src/wallet.rs:85-118`, `:4380-4410`); (iii) the FFI refuses a laddered message outbound rather than stripping it; (iv) both JS predicates carry `child_tesr_bundle`. **Still open, and still cheaper now than after publication: the two-HRP problem** (`ml`/`tml` only, so signet and regtest are indistinguishable from testnet), the invoice's absent checksum, and coin type 0 on every network. The original gate, for the record: four cheap defects in the exact surface a spec would freeze, three of which are the **same** defect repeated (a version field written and never read). (i) `decode_transfer_address` length check — it indexes `[0]`, `[1..34]`, `[34..67]` with no bounds test and **panics** on a short-but-valid Bech32m payload under `ml`/`tml` (`lib/src/lib.rs:45-62`), reachable from `validate_address` and from `create_transfer_update_msg_with_branch` (`lib/src/transfer/sender.rs:193`). (ii) Version checks on `RecoveryBundle` import (`clients/libs/rust-sdk/src/wallet.rs:230`, `:249-289`), invoice decode (`clients/libs/rust-sdk/src/invoice.rs:83-97`) and address decode. (iii) The uniffi FFI stops silently stripping `protocol_version`/`tesr_ladder`/`child_tesr_bundle` (`lib/src/unifii_interface.rs:45-73`) — carry them or refuse the message by name. (iv) `child_tesr_bundle` added to both JS clients' fail-closed predicate (`clients/libs/nodejs/transfer_receive.js:184-193`, `clients/libs/web/transfer_receive.js:224-233`). Gate: a fuzz or table test feeds malformed bech32m and truncated invoices to each decoder and gets **typed errors, never a panic**. Fixing these after publication is a retraction; fixing them now is two days. |
| **WP7** | Documentation truth pass | D0, D1, D3 | 2–3 wks, parallel | A reviewer given only `docs/utexo` (excluding `history/`) and asked to state the census equation, the tier vbytes, the min child value, the exit cost, the leaf hop budget and whether CTES-R is wired gets six answers, each matching code. Specifically: the CTES-R "not yet wired" banner is corrected in all six normative docs (`PROTOCOL.md:12-14`, `SPEC.md:13-14`, `TRUST-MODEL.md:20-21`, `LIGHTNING.md:10-11`, `GRANULARITY-SPEC.md:19`, `INVALIDATION-SPEC.md:16`) — the lane exists behind `config.rs:283`. The census equation gains its superseded term in `PROTOCOL.md:494-495` and `CHILDREN.md:43`/`:66-67` (code: `flat_backups + tiers + superseded`, `tesr.rs:9093`; `SPEC.md:307-317` REQ-38 is the one correct statement, and a leaf's baseline is `CHILD_V2_BASELINE = 0`). `CHILDREN.md` is rewritten from an implementation plan (`:3-4`) into a specification of the shipped leaf. The constants-provenance contradiction is resolved toward `TRUST-MODEL.md:119-123` (compiled-in client constants) and against `PROTOCOL.md:200-201` (nostr-published) — code agrees with TRUST-MODEL and it is also the stronger claim. Five shipped-undocumented mechanisms are documented: leaf combine, leaf renewal, `/transfer/cancel` with its authorization table and pending-lock interaction, the spine tip, the dust-floor rule. `AUDIT-2026-07-30.md` gains a status-refresh header (its headline verdict names a defect closed by `7a03799`, and `defend_ladders` is now wired at `wallet.rs:1579`). README's index and `PARITY.md:3-10`'s pointer to a non-existent CI matrix are fixed. Adopt `TRUST-MODEL.md:478-485`'s retired-and-repointed pattern everywhere. **Round 2 — the audit status header's exact content** (six of nine items moved; a header that does not say which is worse than none): **C-1 CLOSED** (`verify_bundle_bound`, `clients/libs/rust/src/tesr.rs:8227`, wired at `transfer_receiver.rs:661` and `:1381`); **C-2 CLOSED** (one `HashSet<Txid>` spanning both superseded lists **and** the live tiers, `tesr.rs:8404-8425`); **H-1 CLOSED except the yardstick** (per-tier Σ forward law anchored to the on-chain F, `tesr.rs:8879-8935` — see WP3a); **H-2 CLOSED as of round 3** — half (a) by `cap_schedule` (`tesr.rs:5117-5173`), half (b) by the per-kind supersession margin (`:10751-10767`); round 2 recorded it as HALF-CLOSED and a status header that still says so understates the tree; **C-7's clock mismatch CLOSED** (`76bbcbb`) with the SSP-side expiry gates still unbuilt; **C-10 mostly fixed** (`watch_child_pass` + wiring + laddered watch bundle) with (c′) delegated-child coverage, (d) the coloured-only near-deadline pass and (e) F-liveness open; **C-11 FIXED** (L1 CONFIRMED allowlist + L2 supersession + A2 durability, `clients/libs/rust-sdk/src/wallet.rs:2028-2081`); **H-5 FIXED** (`batch_id IS NOT DISTINCT FROM $3`, `server/src/database/transfer_sender.rs:20-25` — note the fix is REFUSAL, not the idempotence the audit asked for, so honest retry after a dropped response is now a liveness question); **H-6 mostly fixed** by the migration-0010 claim latch taken before the enclave call (`server/src/endpoints/transfer_receiver.rs:405-419`, `:463-486`) with a **120 s residual** (`CLAIM_LATCH_SECS`) plus two panicking parses (`:488`, `:495`); **C-6, C-8, C-12 and C-14's theft half OPEN and unchanged**. Also strike the C-1 remediation's claim that `cosign_detrigger` is the mitigation — it remains unwired (`tesr.rs:7367`, `:7376`). **Round 2 — five more shipped-undocumented mechanisms** to add to the list: the **leaf EPOCH DEADLINE** and `epoch_deadline_from_flat_backups` (the single most load-bearing undocumented fact in the leaf lane); `/transfer/cancel`'s **rung cost**, its **coloured-ladder dead end** and its pending-lock ordering constraint; the census restated as a **bijection**, not merely "+ superseded"; `LIGHTNING.md §9` residual 3 (stale) and residual 4 (**a failed exact PAY freezes the coin for 25 h**, not `batch_timeout` = 120 s — `lightning_latch.rs:202` via `MAX(l.expires_at)` vs `server/Settings.toml:4`); and `reclaim_lightning_payment`'s doc-comment, which claims an enforcement its laddered branch does not perform (`ssp.rs:1194-1224`). |
| **WP8** | Spec drafting | rolling | 8–14 wks | Per §5. Each section cites the code site AND the test that pins it. An independent reader can, from §Wire format plus §Cryptographic constructions, hand-construct a bundle the reference verifier accepts — committed round-trip vectors demonstrate it. No drafted section forward-references an undecided item. |
| **WP9** | Verification backlog (gates confidence labels, not writing) | WP2 | 4–6 wks, parallel | A tier confirms on regtest after a genuine 1440-block CSV wait (today every E2E runs the toy schedule; sdk44 uses `TesrParams::regtest()`). A tier below the mempool minimum confirms ONLY after its P2A child is attached, and the un-bumped control does not. A malicious-coordinator test with a stubbed under-reporting `/info/statechain`. A reorg test on a funded ladder asserting the coin's resulting status. An honest backup refused when the market drifts past `fee_rate_tolerance`. chaos22 asserts `cheats_total > 0` and fails a deliberately-seeded value-loss run; every `unwrap_or_default` in its oracle propagates or fails the run; the no-op on-chain double-spend backstop actually surfaces. The fuzzer's adversary model reaches the TES-R ladder (today its only two cheats are V1 flat-backup clawbacks). `ta03_multiple_deposits.rs`'s 19 bare `assert!(is_ok)` and the two bare `assert!(is_err)` at `:466` and `:685` — which pass on "server unreachable" — are repaired. **Round-2 additions:** (1) an E2E driving **cancel → reclaim → successful onward transfer**, asserting the next receiver's census balances with the demoted `S'` counted as superseded (the design is sound on source reading but **test-unattested**); (2) a test that a **PLAIN** leaf approaching `min(L_k)` is force-exited — **round 3: the code now does this** (D13 landed), so the item is a missing TEST rather than a missing defence, and it is the one that matters most because the loop's own history is of firing late; **(2b) NEW — the D14 margin's behavioural test**: a 145/144 pair on the mainnet schedule asserting the named refusal, the one WP3c gate item the implementation did not bring with it; (3) a test that a leaf whose parent's flat backup confirms is reported **Void**, not Blind; (4) a receiver-side adversarial test feeding a hand-crafted `protocol_version = 0` message over a genuinely laddered coin through `validate_encrypted_message`, asserting the refusal names the **count** (today only the sender-side refusal is tested, sdk71); (5) the LN suites (sdk23/24/25/53/63–68, tb04) either run from a pinned, in-repo-referenced LN dependency or are explicitly **quarantined and reported SKIPPED with a reason** — today `SKIP_LN=1` covers only `SDK_E2E` 5 and 6 (`clients/tests/run_all_suites.sh:91-93`), so an LN-less run **fails** sdk63–68 rather than skipping them, and `RLN_REGTEST` defaults to a hardcoded `/Users/gofman/…` path (`:32-33`). LN is the one subsystem whose normative claims are **entirely** E2E-backed with no unit corpus (six unit tests total, `clients/libs/rust-sdk/src/ssp.rs:1251`, `:1269-1298`) and whose counterparty is an external, forked, moving implementation. |
| **WP10** | Adversarial review of the spec itself | WP8 | 2 wks | An external-shaped adversarial pass over the document, not the code. The repo's best results have come from adversarial cycles; "internal review" is not a substitute. Every claim labelled VERIFIED / UNVERIFIED / **AUTHOR-ATTESTED** (see §7 note on E2E reproducibility). |
| **WP11** | **NEW (round 2)** — the LN terminalization carve-out | D17 | 1–2 days | Decide (D17 d/e/f) and, if the carve-out is retained, extend it to the exact whole-coin PAY lane (`clients/libs/rust-sdk/src/ssp.rs:1008-1040` performs no `set_spend_budget`) **or** write down why the pending lock suffices there. Gate: the two PAY lanes make the same choice, and App. B §6 states one rule. |

---

## 4. Subsystems: what is still unsurveyed, and what round 2 found

Round 1's three surveys were organised by *method* — doc truth, test evidence, RGB — not by
*subsystem*, so anything neither well-documented nor RGB fell through; §4 originally listed **seven**
unsurveyed subsystems. **Round 2 closed three and a half of them** (liveness/monitoring, durability
and recovery, the encodings, and the relay half of the chain model), added one that was missing
entirely (the coordinator transfer/claim/cancel state machine), and left the rest. §4a records what
was found; §4b is what is left.

### 4a. Surveyed in round 2 — findings

**Liveness / monitoring — surveyed; the picture is both better and sharper than §4 assumed.**
The child lane now has the reactive primitive it lacked: `watch_child_pass` asks
`f_spender(electrum, parent.f_txid, parent.f_vout)`, returns `Idle` on None, `Void` when F was spent
by anything other than the parent's own trigger, and only otherwise walks and broadcasts — keyless
and idempotent (`clients/libs/rust/src/tesr.rs:7819-7870`; its own doc-comment at `:7800-7818`
records "before this existed nothing did"). It **is** wired: `defend_ladders` runs from
`start_background` once per new block (`clients/libs/rust-sdk/src/wallet.rs:1526`, `:1579`) and its
`ctesr-` loop drives it under an L1 CONFIRMED **allowlist** with unparseable bundles reported BLIND
rather than skipped (`:2277-2320`, `:2108-2111`). And `export_watch_bundle` no longer drops laddered
coins (`clients/libs/rust-sdk/src/watchtower.rs:174-196`). What remains is not "nothing exists" but a
**three-level asymmetry** — reactive covers both lanes, near-deadline covers coloured only, delegated
appears not to cover children at all — which is now **D19**, with **D12** (which clock, what δ
budget) and **D13** (the plain leaf) beside it. The concrete instance §4's original warning needed:
**a leaf's clock is an ABSOLUTE height** (`min` flat-backup locktime of the parent) while every
laddered watch path is EVENT-triggered on F and sets `deadline_block = u32::MAX` on the reasoning
that "an idle ladder never ages" — true of the ladder, false of the backups a leaf hangs under
(`watchtower.rs:172-181`, `tesr.rs:7846-7859`).

**Durability and recovery — surveyed; materially better than assumed, and §23 is unblocked.**
`export_recovery_bundle` snapshots the wallet record plus `get_all_backup_txs`, an **unfiltered**
`SELECT statechain_id, txs FROM backup_txs WHERE wallet_name = $1`
(`clients/libs/rust-sdk/src/wallet.rs:213-242`, `clients/libs/rust/src/sqlite_manager.rs:118-122`),
so it sweeps the exit ladder (`tesr-`), child bundles (`ctesr-`), `branch-`/`parents-` **and every
crash journal**, because the journals are rows in the same table under their own key prefixes
(`tesr.rs:19531`). Both reads are taken under `wallet_lock`, so the snapshot cannot be torn. The set
is also stated in prose (`TRUST-MODEL.md` B7 at `:414`, B9 at `:416`). Two residuals, neither
gating the text: the bundle's `version: 1` is **written and never read** on import (`wallet.rs:230`
vs `:249-289` — WP6b), and whether journal replay on wallet open is REQUIRED is undecided (see the
§14 note). Separately the **journal state machines are spec-grade**: four named families with
documented transitions and an explicitly-recorded terminal "irreducible window" variant —
`SplitStage{Planned, Signed, Established, Committed, Stranded}` (`tesr.rs:3082-3106`),
`ConveyanceStage{Pending, Attempted, Conveyed, Stranded}` (`:3244-3280`),
`ChildRenewalStage{Planned, ExtensionSigned, Complete}` (`:19520-19528`), `CombineStage`
(`clients/libs/rust/src/combine.rs:754`), sharing the invariant "advanced ONLY forward, and each
transition journalled BEFORE the network call it describes, never after".

**The encodings — surveyed; worse than assumed.** See D11: the invoice's version field is written
and never read, `decode_transfer_address` **panics** on a short payload, and only two HRPs exist so
signet and regtest are indistinguishable from testnet. WP6b.

**Chain model — the RELAY half is surveyed; the confirmation/reorg half is not.** The relay contract
is now specifiable and is the half an implementer would otherwise have to reconstruct from Core
source. Every tier is `version: 3` (`lib/src/tesr.rs:262`, `:562`) and self-funding at the committed
2 sat/vB, so each **relays standalone** and **no package is required** (`:63-71`, `:100-104`). The
exit walk is submitted **sequentially** — every driver skips known txs, broadcasts the next, and
`break`s at the first rejection to retry next pass (`clients/libs/rust/src/tesr.rs:5526-5548`,
`:7776-7797`, `:7869-7881`) — which is already exactly right; **there is no relay defect**. Under
TRUC at most **2 transactions of one exit chain may be unconfirmed simultaneously**. Applied to the
three live shapes: a depth-0 walk (`T → X(720) → S(1440)`) and a TwoTier split walk
(`… SP(0)` always flanked by CSV-720 rungs, `tesr.rs:5064-5065`, `:5346`) **never bind**; only a
**CATS-B spine** binds, because `SP_{i+1}` spending an unconfirmed `SP_i` on top of an unconfirmed
`X_m` has 2 unconfirmed ancestors and is rejected — so a spine advances ~2 tiers per block
(`:623-633`, `:5502-5510`, `:4603`; the doc's own count agrees, `PARTIAL-PAYMENT-ECONOMICS.md:397-401`).
**Three clauses the spec MUST carry**: (i) all pre-signed tiers are nVersion=3 and the exit is
submitted sequentially, never as a whole chain; (ii) the in-flight window is 2; (iii) a
"too many unconfirmed ancestors" rejection is a **WAIT**, not a failure and emphatically not evidence
the coin is dead — an implementer who reads it as fatal reinvents the Blind/Void conflation `a9187e9`
removed, one level down. **Round 2 had zero live evidence** — the harness mines `s.wait_blocks.max(1) + 1` blocks after every
pass (`clients/tests/rust/src/sdk69_transfer_many_inladder.rs:114`), so no test had ever put two
transactions of one exit chain in a mempool at once. **Round 3: the run happened and this paragraph
is now measured, not read** (`docs/utexo/notes/WP1-TRUC-P2A-SPIKE.md`, Core 30.2.0, isolated regtest
at a 3 sat/vB floor, AUTHOR-ATTESTED). Three tiers chained: 0 ancestors accepted, 1 accepted, 2
**rejected** — `TRUC-violation, tx <txid> … would have too many ancestors`. The in-flight window of 2
is a measurement. A tier below the floor is refused alone and **accepted as a 1P1C package**, both
transactions inheriting one effective feerate; a genuine `OP_1 <0x4e73>` output at exactly 240 sats
relays and is **not** dust to Core, pinned since by `lib/tests/p2a_script_shape.rs`. The
package-capable backend the spike named as unresolved is now `mercuryrustlib::core_rpc::submit_package`,
exercised live in `live_p2a_package_rescue.rs`. What the run did **not** settle, and what §15 still
carries: who funds the child for a **keyless** tower — the spike's child was funded from a second
wallet input, and D31's answer is "the owner", not "the tower".
**One grief to price**: a third party can occupy an unconfirmed spine tier's single child slot for
240 sat, and the pre-signed successor — fixed fee, un-bumpable — cannot out-bid it. **Round 3: the
"no anchor spender" half of that sentence is retired** (D5) — an owner with a funded `fee_bump` can
now build a package. Whether that lets the owner DISPLACE a squatter turns entirely on **TRUC sibling
eviction**, which the spike did not exercise: it is the one BIP-431 semantic this section still takes
on a reading of Core rather than a run, and it is now the deciding input for how this grief is
priced. Until it is run, keep the conclusion as round 2 had it — latency, not theft, bounded by the
exit-length cap — and mark the remedy UNVERIFIED rather than assumed.

**The coordinator transfer / claim / cancel state machine — surveyed (new row, see 4b's note).**
It is where D15, D16 and half of D8 are written from. Migrations **0010 (claim latch)** and **0011
(`sender_auth_xonly_public_key`)** are security-semantic and post-date every round-1 survey.
`OPEN_TRANSFER_WINDOW_SQL` — a CASE keying batched transfers off `MAX(lightning_latch.expires_at)`
(`server/src/database/transfer_sender.rs:88-102`) — is spliced into both halves of the pending lock
(`:144`, `:178`) and read by both sign gates (`server/src/endpoints/sign.rs:148`, `:324`), so it is a
**shared** predicate carrying the invariant "no authorization expires before the latest claim it
protects". Note the **cross-lane coupling**: in this SDK every batched transfer IS an LN latch (all
`Some(batch_id)` producers are LN — `lightning.rs:83`, `ssp.rs:603`, `ssp.rs:1026`,
`transfer.rs:1644`, `tokens.rs:2866`), so a v1 sats spec must either specify the lock as "un-batched:
1 hour from `updated_at`" and declare latched transfers out of scope **by name**, or carry the
latch-keyed branch as normative. Publishing "the lock is one hour" would be false against deployed
code. *By its own doc-comment there is no test database, so the latch branch is unexecuted in CI
(`transfer_sender.rs:86-93`).*

**Provenance warning.** `/Users/gofman/Downloads/mercurylayer-feat-spark-security-audit.md` predates
commits `19e6668` and `76bbcbb`: its §2.1 and §2.4 describe SQL that no longer exists at `280ed88`
(the window is now the CASE above, and the duplicate guard is `batch_id IS NOT DISTINCT FROM $3`,
`transfer_sender.rs:20-25`). Its §2.2 (no latch-expiry gate in `execute_pay` or `unlock_by_preimage`)
and §2.3 (unauthenticated SSP HTTP API) **do** reproduce. Do not merge that document's verdicts
unchecked.

### 4b. Still unsurveyed

Named with enough `file:line` to scope the survey; each maps to a spec section in §5.

| Subsystem | Where it lives | Why it matters to the spec |
|---|---|---|
| **Cryptographic constructions** | `lib/src/transaction.rs:4` (`blinded_musig_pubkey_xonly_tweak_add`, `blinded_musig_negate_seckey`), `:65-85` (`create_and_commit_nonces`, per-session `BlindingFactor`), `:582-626` (parity/negation, `blinded_musig_partial_sig_verify`); key handover `t1 = o1 + x1` at `lib/src/transfer/sender.rs:155-166`; ownership signature = schnorr over `sha256(txid‖vout_le‖new_user_pubkey)` at `lib/src/transfer/receiver.rs:175-196`; ECIES for the transfer message | The whole normative corpus says "blind MuSig2" twice in passing (`PROTOCOL.md:176`, `:181`). A third party cannot produce a single compatible signature. §3 (shapes), §6 (census, which *counts* signatures), §12 (wire format) and §14 (SE-is-blind) all sit on top of this. It cannot be appended late. |
| **SE (lockbox) contract** | Six calls, coordinator-side: `get_public_key` (`server/src/endpoints/deposit.rs:405-409`), `get_public_nonce` / `get_partial_signature` (`server/src/endpoints/sign.rs:54-58`, `:239-243`), `signature_count` (`server/src/endpoints/transfer_receiver.rs:45-49`), `keyupdate` (`:462-466`), `delete_statechain` (`server/src/endpoints/withdraw.rs:65-69`) | *Never propose changes to `enclave/` or `lockbox/`* is a scope constraint on modification, not on description. An auditor cannot check the census without knowing what increments the count; a reader cannot understand claim finality without `keyupdate`'s irreversibility or `delete_statechain`'s effect. Specify descriptively; change nothing. |
| **Deposit / onboarding** | `GET /deposit/get_token` (`deposit.rs:102`), `POST /deposit/get_derived_token` (`:134`, owner-nonce + 64-per-parent lifetime cap), `POST /deposit/init/pod` (`:305`) with a schnorr `signed_token_id`; external token server via `check_token_status`; two unopened top-level crates `token-server/`, `token-server-v2/` | The protocol's entry point and its DoS/fee gate. A spec that starts at "a coin exists at F" has no story for how a coin comes to exist. Decide whether the external token server is in v1 or is a deployment-specific admission policy. |
| **Authorization** | Single-use, endpoint-bound auth challenges: owner signs `sha256(nonce‖endpoint)`, consumed atomically on first valid use, 5-minute window (`server/src/database/auth_nonce.rs:1-12`, migration 0006) | This is the authorization model for every irreversible owner operation. Unspecified. |
| **Chain model — the confirmation / reorg half only** | `confirmation_target` 2/3 (`clients/libs/rust-sdk/src/config.rs:268`, `:296`), gating deposit spendability and claim acceptance; `TRUST-MODEL.md:220` lists the chain source as trusted for tip height, "which drives every locktime floor/deadline decision"; `:235-238` states finality modulo a deeper reorg | Every timelock race is measured in blocks read from an electrum server the client trusts. Publishing 1440-block CSVs with no confirmation policy, reorg assumption, or behaviour for a ladder whose F is reorged out is the omission reviewers notice fastest. HEAD contains a commit of exactly this shape (`a9187e9`, "a coin whose F was re-spent is Void, not Blind"). **The relay half is now surveyed — see §4a.** |

**Still open, UNVERIFIED:** mailbox availability and censorship. The coordinator holds the encrypted
transfer message and, since `POST /transfer/cancel` (`server/src/endpoints/transfer_sender.rs:319`),
a sender-initiated cancellation. **No survey in either round examined withholding, deletion or
reordering** — round 2's coordinator lane covered the transfer/claim/cancel *state machine*, not
mailbox availability.

**No longer open:** "eleven migrations exist … nothing covers state migration or how a deployed
coordinator moves between protocol versions" is now surveyed and is **D20**. The answer is that there
is no story: forward-only additive migrations whose defaults grandfather every live coin into the
pre-feature semantics, and no capability discovery.

---

## 5. Spec table of contents

Writable-now means: no open decision gates the text, and a failed decision elsewhere would *append*
rather than retract. Sections marked **survey-first** cannot be drafted until WP1 item 5 lands.

| § | Section | Status | Blocked by |
|---|---------|--------|-----------|
| 1 | Scope, conformance language, named limitations | writable now | — (but its content depends on D0/D1/D3/D8) |
| 2 | Parameters and per-network profiles | **writable now (round 3): D7 is taken as (a) and enforced in code** — three presets, no silent fallback, a panic on an unknown network, and `lockheight_init`/`interval` compiled in with the coordinator's copy demoted to a cross-check | — (the phantom cap of 8 is a WP7 deletion, not a blocker) |
| 3 | Cryptographic constructions | **survey-first** | WP1.5 |
| 4 | Transaction shapes: F, T, X_m, S_k | writable now | — |
| 5 | The flat-backup ladder | writable now | — |
| 6 | In-ladder split and the spine | blocked | D2 (incl. V5) |
| 7 | Census and receiver verification | **binding half now writable and code-backed** (round 2); hold only the value claim | ~~D4~~ **answered**; ~~D14~~ **shipped (round 3)**; D8's aggregate half, D10 |
| 8 | Value conservation | writable now; **publish only after WP3a/WP3b** | — |
| 9 | The leaf lane | ~~downgraded (round 2)~~ **writable now (round 3): the runtime half of the security argument shipped** | — (D13's REQUIRED-vs-RECOMMENDED wording only) |
| 10 | Renewal, rollover, end of life, compaction, sub-economic finality | blocked | D3 (~~D7~~ taken — the epoch is 10 000 on every non-regtest profile) |
| 11 | Exit: cost, latency, depth cap, admission rule | blocked | D6, D2, D3 (~~D7~~ taken; the cap evaluates to 23 txs / depth 10 on both shipping profiles) |
| 12 | Fee model and relay assumptions | blocked on the DECISION only — ~~the P2A spike~~ **ran, and the mechanism is built** (round 3) | D5 |
| 13 | Wire format and version negotiation | blocked | D9, **D16** |
| 14 | State machines, journals, crash recovery | writable now (round 2: journals are spec-grade) | — (one small decision, see note) |
| 15 | Liveness, monitoring and deadline margins | ~~survey-first~~ **surveyed; now decision-blocked** | **D12, D19** (~~D13~~ shipped) |
| 16 | Chain model: confirmations, reorgs, trusted tip | **relay half writable now, and now measured** (round 3); confirmation/reorg half **survey-first** | WP1.5 (~~D7~~ taken) |
| 17 | Trust model | mostly writable | D8 (twelve items, **one now closed** — the attested `num_sigs`), D10 |
| 18 | SE interface (descriptive) | **survey-first** | WP1.5 |
| 19 | Coordinator API and obligations | **mostly writable; the error taxonomy is blocked** | **D18** |
| 20 | Deposit and onboarding | **survey-first** | WP1.5 |
| 21 | Authorization | **survey-first** | WP1.5 |
| 22 | Cooperative exit and coin termination | writable now | — |
| 23 | Durability and recovery | ~~survey-first~~ **writable now** (round 2) | — |
| 24 | Address and invoice encoding | blocked | D11 (sharpened; ~~WP6b~~ **done except the HRPs**) |
| 25 | Migration and backward compatibility | blocked | D9, **D20** |
| 26 | Conformance and test vectors | last by construction | WP2, **D18** |
| App. A | RGB integration — status and constraints (non-normative) | writable now | D1 |
| App. B | Lightning — status and constraints (non-normative) | **writable now EXCEPT §8 (trust) and §6 (terminalization)** | **D17** |

Section notes that carry real content:

- **§1** is where the honesty lives: D1's scope-out, D3's sub-economic limitation, D8's coordinator
  assumptions, D10's residual, which client profiles implement the spec (Rust SDK only — both JS
  clients fail closed on every laddered coin, at `clients/libs/nodejs/transfer_receive.js:184`),
  and — if D2 resolves without V5 — that §6 and §11 are provisional on the spine-tip floor.
  **Round 2 adds four §1 obligations**: the **one decoder rule** ("a decoder MUST reject a version it
  does not implement", D9/D11/D16 all inherit it — **round 3: implemented for the invoice and the
  recovery bundle, still absent for `protocol_version`**); the **conformant verifier entry points**
  (see §7); the **uniffi FFI's** downgrade, which makes it a non-conformant profile (D16 — round 3:
  it now refuses outbound, so the claim narrows to the inbound direction); and — if D13
  resolves to (b) or (c) — that a **plain leaf has no runtime near-deadline exit** (**round 3: the
  code closed this, so the obligation is now to state the wallet's near-deadline duty, not to
  disclose a gap**). **Round 3 adds a fifth, and it is the sharpest reason the Rust SDK is the
  conformant profile**: the Rust client demands a nonce-bound enclave attestation over `num_sigs` and
  refuses without one, while **neither JS client sends the nonce or reads the attestation** — their
  flat count check runs against a coordinator-asserted number. Say that, rather than the softer "they
  fail closed on laddered coins", which describes a narrower gap than the one that exists.
- **§4** is settled and its constants rest on measurement: v3/TRUC, the 240-sat P2A anchor,
  payload+anchor structure, the dust floor on every payload output (a live theft vector closed in six
  verification lanes and documented nowhere), `TIER_VBYTES = 125` measured against the production
  finaliser at `lib/src/tesr.rs:867-920`. Say so and cite the measuring test.
- **§5** includes [B1] as a stated property — T is un-timelocked and spends F, and any prior owner's
  flat backup also spends F — since it is why half the verifier exists. And the normative rule that
  **leaves carry no flat backups**.
- **§6**: once D2 freezes, promote `clients/libs/rust/src/tesr.rs:5323-5346` into the text
  essentially verbatim. It is the best-written protocol reasoning in the repo and no reader can
  currently see it. **Round 2 — but not verbatim beside `PROTOCOL.md:245-247`**: that argument covers
  the CSV axis only, and the pinning-resistance sentence next to it is false for the spine. See D2.
- **§7** (rewritten, round 2). The MUST-list has two halves and they now have different statuses.
  **(a) The coin binding — writable now, code-backed.** State as a MUST-list the five bindings of
  `verify_bundle_bound` (`clients/libs/rust/src/tesr.rs:8227-8290`): `bundle.statechain_id ==
  coin.statechain_id`; the funding outpoint; the funding value; the declared `agg_address` ↔ the
  on-chain funding key; and the **coordinator-recorded aggregate** ↔ the same on-chain key,
  **fail-closed** when no aggregate is on record. Plus the child lane's parent-F anchor
  (`verify_child_bundle`, `:9277-9311`) and child-sid ↔ conveyed-slot equality at **both** the
  pre-pay and claim sites (`clients/libs/rust/src/transfer_receiver.rs:699-705`, `:1160-1167`).
  The authority struct is built only from the funding transaction plus the coordinator record
  (`tesr.rs:8000-8029`), `tx0_hex` is fetched from Electrum **by txid** (`transfer_receiver.rs:1842-1858`),
  and the laddered arm explicitly refuses a funding tx that came from a sender-supplied un-broadcast
  branch (`:1344-1350`) — so a bundle for coin A cannot be presented as evidence about coin B.
  **(b) The census — write it as THREE conjoined obligations, never as the equation alone** (D4): a
  total, a per-item validation battery, and **slot uniqueness over the union of live and disclosed
  tiers**. Source the total from `SPEC.md:307-317` (REQ-38), still the only correct statement in the
  corpus. **(c) Claim the right property**: "no undisclosed spending path signed under A exists" —
  hand "the receiver cannot be defrauded" to §8, and name the two open value tripwires. **(d) Name
  the conformant entry points**, not "the verifier": `verify_bundle_bound` and `verify_conveyed_child`.
  The crate also exports `verify_bundle` — which passes `f_onchain = None`, and the `None` arm
  **skips the trigger's value hop entirely** (`tesr.rs:8884-8890`) — and `verify_child_bundle`, which
  carries no fee-rate binding (`:15578-15599`). Today the only production callers of either are
  self-checks on locally-built material (`:1830`, `:5895`), so this is conformance language, not a
  live hole; pair it with a cheap ci-guard (hours) asserting zero production callers, in the style of
  the guards already at `:19058-19080`.
- **§14** is settled, undocumented and load-bearing: the SE's `num_sigs` is monotonic, so a crash
  between the two co-signatures of a leaf renewal leaves a leaf whose count nothing can account for —
  **bricked, silently** (`clients/libs/rust/src/tesr.rs:19533-19545`). State that as either a REQUIRED
  recovery procedure or a named unrecoverable state; do not merely describe the journal families.
  Key-prefix routing (`tesr-`/`ctesr-`/`spinetip-`/`crenew-`/`combine-`) is implementation-defined —
  but §23 must then say how a restoring wallet locates its own ladders. **Round 2 adds four things.**
  (i) Name the **four** journal families with their enums and promote the shared invariant —
  *journalled BEFORE the network call it describes, never after; advanced only forward* — to a
  normative MUST. `ConveyanceStage::Stranded` is a model of what a spec needs: the SE reports a
  transfer open on this slot but the wallet holds no `x1`, `t1 = o1 + x1` cannot be rebuilt, the SE
  will not reissue — terminal, named, reportable. (ii) Add a **storage atomicity MUST**:
  `insert_raw_backup_txs` must execute its DELETE+INSERT in ONE transaction, because journal_write
  advances one record through its stages and a crash in a two-statement window destroys the previous
  stage as well as failing to write the new one — "the write-ahead log evaporates at exactly the
  moment it is being relied on" (`clients/libs/rust/src/sqlite_manager.rs:124-145`). An independent
  implementation would otherwise reproduce the original bug. (iii) **A small decision: is replaying
  open journals on wallet open REQUIRED, RECOMMENDED, or the application's choice?** Three of the
  four readers are opt-in APIs with **zero non-test call sites** — `recover_in_ladder_splits` and
  `resume_split_conveyance` (driven only by sdk81, `clients/libs/rust-sdk/src/transfer.rs:2143`,
  `:2320`) and `child_renewal_open` (none at all). So a wallet that crashes mid-split and restarts
  resumes nothing and **looks idle** — the exact silent-degradation shape the journals exist to kill.
  Given `SplitStage::Signed` is documented as "from here the material is unregenerable and recovery
  must REPLAY, never restart", **REQUIRED is the only defensible answer**. (iv) The **`crenew-`
  journal has a writer and a reader but no replay driver and no API surface** — its failure error
  even instructs the operator to "replay from there, never re-sign" while naming no code that can
  (`tesr.rs:19595-19613`, `:19911-19919`). Ship the driver or state a named unrecoverable state with
  a manual procedure; do not let the spec imply a recovery that has no implementation. **Also add the
  cancel-reclaim crash window** as a second unaccountable-count state: the reclaim co-sign is
  irreversible and its persist is not (`tesr.rs:7307-7314`), and it is the one irreversible-co-sign
  path with **no journal family at all** (`:19531`'s prefix list has no entry for it).
- **§9 (round 2, downgraded)**. The leaf lane's *mechanics* are writable; its **security argument is
  not**. That argument has three parts and none of them is in a normative document: (1) STRUCTURAL,
  against rivals under A — every tier below F is a 2-of-2 under the aggregate, the census bounds
  them, and a newly built state is placed strictly below EVERY rival over the outpoint it will spend
  (`next_rival_state_csv` takes the min over live + superseded + outstanding-conveyed, then subtracts
  δ, `clients/libs/rust/src/tesr.rs:7121-7164`); (2) TEMPORAL, against the parent's retained flat
  backups — the [B1] exposure the leaf cannot answer structurally, bounded by `min(L_k)`; (3)
  ADMISSION — `check_exit_headroom` refuses at claim time any leaf whose whole walk cannot finish
  before that height, reading the CSVs off the **signed** nSequence and charging one confirmation
  block per tier (`lib/src/transfer/receiver.rs:713-760`). Every term is receiver-derived, and the
  census's count-pinning is what stops the minimum being flattered by dropping a low entry
  (`tesr.rs:5809-5822`). Round 2 said **hold §9's security claim on D13**, because publishing it
  as-is would state a property the shipped plain lane did not have at runtime. **Round 3 releases the
  hold**: the plain lane has the runtime defence now (D13), so all three parts are true of the code
  and §9 may be written whole. Write part (3) with the number it actually uses — `exit_wait_blocks`,
  the same function the near-deadline pass calls, so the admission gate and the defender provably
  agree rather than agreeing by inspection.
- **§15 (round 2 — no longer survey-first).** The survey is done and produced **one decision with
  four numbers in it** (D19), plus D12 and D13. Write the tower's *negative* properties as normative
  MUSTs and MUST-NOTs — they are the strongest and most citable part of the section (see D19). Do not
  write a cadence, margin or re-export rule until D19 closes.
- **§19**: round 2 — the API surface is writable, but **the error taxonomy is blocked on D18**, which
  is a bigger problem than the missing ERR numbers below suggest. Also specify what the coordinator
  MUST do from the enumerated D8 list, not from four items.
- **§23 (round 2 — unblocked).** State the retention set NORMATIVELY (it is currently a doc-comment
  plus a trust-model row): the wallet record plus every `backup_txs` row, which includes the ladder,
  the child bundles, `branch-`/`parents-` and **every crash journal**; token wallets additionally need
  the whole `rgb_data_dir`, which the bundle deliberately does not embed. Say what a restoring wallet
  does about journal rows it finds (§14 (iii)).
- **§17** states the SE-is-blind property *precisely*, not as a slogan: the SE sees a sighash plus a
  caller-supplied prevout amount and never deserialises outputs, therefore every content gate is
  coordinator-or-client. Include sdk15's single-SE double-sign race as a normative **assumption**, not
  a defect — and name its dependency on the P2A bump. **Round 3: the bump exists** (D5), so the
  sentence to write is that the defence is real but **opt-in** — a wallet with no `fee_bump` fee
  source has only "be first".
- **§19** enumerates from the actual route list, not `SPEC.md §3`, which omits `/transfer/cancel`
  entirely and names `/deposit/init` where the route is `/deposit/init/pod`. It must also specify what
  the coordinator MUST *do* — the pending-transfer lock, claim rotation atomicity, cancellation's early
  lock release — which matters more than usual because the SE is blind. Complete the error taxonomy:
  ERR-11 is skipped with no note and there is no error for cancel, depth-cap, headroom or dust refusals
  — **but round 2 found the real problem is one level up: there is no machine-readable error surface
  at all. See D18.**
- **App. B (rewritten, round 2).** `LIGHTNING.md` is still the most self-honest document in the
  corpus and both self-flagged unbuilt items verify true — but **it cannot be carried forward
  as-is**, because the two paragraphs a reader would quote (§8 trust, §6 terminalization) are stated
  more strongly than the code supports. **Reproducing §8 verbatim is the dishonest scope-out.**
  Residual 3's *mechanism* is stale (the window is now the CASE at
  `server/src/database/transfer_sender.rs:94-102`); what remains genuinely unasserted is
  `lock_expiry ≥ HTLC CLTV + grace` — **D17**. Beyond the two flagged items, **four more mechanisms
  are refused or absent** and the normative text elsewhere reads as if the lane were complete:
  coloured LN on the CTES-R child lane (`clients/libs/rust-sdk/src/tokens.rs:3292-3295`), non-exact
  RGB PAY (`ssp.rs:1093`), a remote `/receive_asset` endpoint (`ssp.rs:1174-1183` takes `&SspService`,
  local only), and the coordinator-side `sig_count` reconcile (`ssp.rs:1206-1226` is a comment).
  So §9.6's "the colored bridge … wired into `pay_lightning_invoice` and `create_receive`" is true
  only on the **legacy un-laddered** lane. **The acceptance criterion for the appendix**: it states
  RECEIVE's soundness with its two preconditions explicit (`latch_expiry < inbound HTLC CLTV
  deadline`; `grace ≥ worst-case settlement latency` — a 300 s wall-clock constant), and states PAY as
  "the existing operator-trust bar **plus** a pre-pay census that is real and fail-closed". The
  census is genuinely load-bearing and stronger than the doc says: `execute_pay` refuses unless every
  latched sid is a pending transfer this wallet could **decrypt** (i.e. genuinely addressed to the
  SSP) **and** carries `ladder_census_ok` (`ssp.rs:412-467`), computed fail-closed with no
  trivially-passing coin shape (`clients/libs/rust/src/transfer_receiver.rs:922-968`). One page,
  writable in a day once D17 closes.
- **App. B — the one coupling that must be carried, not scoped out.** LN's dependency runs one way:
  LN *consumes* the sats primitives (its pre-pay census is literally the claim-path verifiers hoisted
  earlier); nothing in them consumes LN. **Except** the pending-transfer lock, whose *window function*
  is defined by reference to the `lightning_latch` table (§4a). Name it explicitly or specify the
  latch-keyed branch as normative — silence would publish a false statement about the sats lane.

---

## 6. What v1 does not cover

| Scoped out | Statement in the spec |
|---|---|
| **The RGB / coloured lane** | "RGB integration is specified separately and is not yet frozen." Appendix A states plainly: both lanes exist; the shipped default gives carriers no SE-free exit; CTES-R is the intended lane; here are the five open sub-decisions. The sats lane still carries the ladder-establishment precondition (D1) and the receiver-side refusal that enforces it (WP6). |
| **Lightning** | Non-normative appendix. Two self-flagged unbuilt items verify true; the deadlock caveat has no test since sdk03 was deleted; LN does not compose with the coloured child lane at all (`clients/libs/rust-sdk/src/tokens.rs:3292`). **Round 2 confirms LN is cleanly scope-out-able — the dependency runs one way** (LN consumes the sats verifiers; nothing in them consumes LN) — **but the appendix cannot be published until D17 closes**, and the pending-lock coupling must be named (see App. B notes). |
| **The package-aware watchtower** | `PROTOCOL.md` is already honest about this in three places (`:536-539`, `:710-713`, `:812-813`). State the committed-fee-only relay model normatively. **Round 3 — the scope-out narrows to the KEYLESS tower and should be re-titled that way.** The mechanism is built and wired into both broadcast loops, but only for a caller holding a `BumpCapability`; a keyless tower still cannot bump, by construction and on purpose (D31). So the out-of-scope item is not "package-aware bumping" but "**a tower that can bump without keys**", and §12 carries D5's limitation for exactly the population that has no fee source. |
| **Whole-Carrier Handover (WCH)** | **RETIRED by owner decision, 2026-08-10 — done, not pending.** Fully designed in `COLORED-FORWARDING.md §3`, never built (`colored_whole_transfer`, `WCH_SAFETY_MARGIN`, `min_backup_output`: zero occurrences, verified), functionally superseded by `transfer_colored_child` (`clients/libs/rust-sdk/src/tokens.rs:4332`) and by `build_colored_receiver_state` (`clients/libs/rust/src/tesr.rs:1903`) for the whole-carrier hop itself. Its §2.1 recommendation was the *cheap alternative* to the coloured ladder, gated in §2.2 on whether unilateral exit was worth the cost — CTES-R was then built, so the either/or expired. The retirement is written into `COLORED-FORWARDING.md`'s header, scoped so that §1 (the structural-leaf proof) survives: four normative docs cite it as background and those citations stay valid. **Carry forward, because retiring WCH did not fix it:** the legacy-lane leaf defect is still live at `clients/libs/rust-sdk/src/tokens.rs:3325` (bare refusal, no whole-carrier fallback) for as long as `SdkConfig::colored_ladder` ships `false`. Skipping WCH is a bet that D1 resolves toward CTES-R; if D1 ever resolves the other way, un-retire the document rather than redesigning. |
| **The {level, m, k} counter machine and `POST /renew/init`** | **Scoped out — unconditionally. D4's proof HOLDS (round 2).** Delete `PROTOCOL.md §5.5`'s endpoint and §5.11 R4′'s per-level checks. The former "if the proof fails this is a blocker" branch is closed: a hidden co-sign at any level raises the same total, and decoy-vs-hidden is separated by per-item validation, slot uniqueness and root anchoring, not by per-level counters. **No excluded-scope collision.** The proof must be published **with** its two coordinator-trust premises (D8). |
| ~~**Package-relay / `submitpackage` transport**~~ **— NO LONGER SCOPED OUT (round 3)** | Round 2 made this explicit because every client-library broadcast was `electrum_client::transaction_broadcast_raw` and no package submission existed, so the P2A hatch was unreachable through the reference transport. **It is reachable now**: `mercuryrustlib::core_rpc::submit_package` is an opt-in Bitcoin Core RPC route — electrum still has no equivalent, which is why the capability carries its own endpoint rather than reusing the wallet's. The spec must therefore say something harder than "future work": **a conformant implementation needs TWO transports**, an electrum-shaped one for ordinary broadcast and a package-capable one for the rescue, and an implementation with only the first is correct until the mempool floor rises above 2 sat/vB. Keep the sequential-submission model and the in-flight window of 2 as normative (§4a) — those are now measured. |
| **`transfer_many` as a separate operation** | Do **not** specify it as one. It shipped and is the same in-ladder split with K payload outputs; parameterise the split shape over K (D2). Specifying it separately would duplicate §6 and create two places where the fan-out's value floors and depth cap must agree. |
| **Automatic renewal inside `transfer()` and the depth-3 compaction *threshold*** | Specify the primitives and the end-of-life rule (D3); the automatic policy and the threshold are v2. Note that compaction *at the cap* is required for liveness and stays in scope. |
| **CATS-B's V4 (key prefix), the coloured spine, K>1** | Named as v2 work in §6. V5 is **not** in this list — see D2. |
| **The sub-economic viability *gate*** | The limitation is published (V_min as a function of (d, r)); the receiver-side gate is v2. This is a genuine limit of every off-chain system with unilateral exit; naming it costs nothing and pretending to have solved it is what gets a spec retracted. |
| **The legacy un-laddered lane** beyond §5's ladder and the version-0 downgrade rule | Do not specify the legacy transfer path as a supported profile. |
| **JS / web / nodejs client profiles** | Both fail closed on every laddered coin — correctly, since their flat `num_sigs == backups.length` check cannot detect a retained hidden state. §1 names the Rust SDK as the conformant profile. |

---

## 7. Timeline

**Resourcing assumption, stated because every number depends on it: one author plus agents, with the
owner as sole reviewer.** Three concurrent tracks look like parallelism but share one review
bottleneck, and this project's history is that agent-parallelism produces claimed greens that were
never run. If the answer is "the owner plus agents", re-cost Track 3 at roughly a third of its
nominal parallelism.

**Round-2 effect on the schedule, stated in both directions.** *Down*: D4 is answered, so one of the
three unnumbered items is retired and one week-1 workstream disappears; three and a half subsystems
are surveyed, so the survey-first drafting block shrinks from seven sections to four-and-a-half and
§23 moves into the early block. *Up*: **nine new decisions** (D12–D20), three new work packages
(WP3c, WP6b, WP11), and a week-1 item added to WP1. Net: the **decisions** track lengthens, the
**full-protocol** total shortens, and the ladder-only total is unchanged but rests on better evidence.

**Round-3 effect, in one direction only.** Nothing was added. Two decisions are taken (D7, D14), one
is answered in code (D13), one week-1 unknown is retired outright (the P2A mechanism) and one is
answered (`sig_count`), and two work packages are done (WP3c, WP6b). The numbers in the table below
are round-2 numbers and are now **conservative**: the week-1 block is roughly half spent, the
publication gate is one item rather than two, and §2 and §9 move from blocked into the early drafting
block. They are deliberately not re-cut here — the 2026-08-11 re-cost at the head of this document is
the current schedule, and a second set of numbers derived from a re-verification pass rather than a
re-plan would only compete with it.

```
WEEK 1 — buy down the unknowns (WP1), in parallel, before anything is scheduled around them
  [D4 proof attempt — DONE, round 2]
  [P2A/CPFP spike + TRUC live run — DONE, round 3: measured, and the mechanism built]
  ["is sig_count authenticated?" — ANSWERED, round 3: it was not; it now is, fail-closed]
  [WP6b interop fixes — DONE, round 3, except the HRPs]
  D10 analysis  |  survey of the 4 remaining subsystems + mailbox
  Also: WP6's rgb-lib pinning (1 day, urgent independent of scope), WP2 started.
  → GATE: how many excluded-scope collisions do we have — D10 is the only candidate left.

WEEKS 2-5 — decisions (WP0)   [was 2-4; nine decisions were added]
  D0 first (deliverable + freeze).  [D7 — TAKEN as (a) and in code, round 3; the epoch is 10 000]
  Then D1, D2, D9, D16, D11.  Then D5 (the P2A + transport result is IN), D6,
       D3 (needs D6), D8 (now merged with D4; its num_sigs half is closed, the aggregate half is not).
  Then the round-2 policy block: D12, D19 together (one liveness sitting), D15, D17, D18, D20.
       [D13 — code landed; decide only REQUIRED vs RECOMMENDED.  D14 — TAKEN and shipped.]
  → GATE: DECISIONS.md complete. Freeze in force.

DRAFTING (starts week 1, does not wait)
  Weeks 1-5:   §1, §4, §5, §8, §14, §19 (less the error taxonomy), §22, §23 and App. A
               — no open decision gates these.  §7's BINDING half joins this block (round 2).
  Weeks 3-7:   WP7's truth pass, feeding the drafts.
  Weeks 5-10:  §2, §6, §7's census half, §9, §10, §11, §12, §13, §15, §16's relay half,
               §24, §25 and App. B as their blockers close.
  Weeks 6-11:  the remaining survey-first sections (§3, §16's reorg half, §18, §20, §21).
  Then:        §26, integration, WP10 adversarial review, publication.

CODE AND EVIDENCE (parallel; gates PUBLICATION, not writing)
  Weeks 1-2:  WP2 reproducibility spine.  [WP6b — DONE except the HRPs, which are hours]
  Weeks 3-5:  WP3a fee-rate binding — HARD PUBLICATION GATE, still open.  WP4 (now: docs + the
              aggregate backfill only — the parameter half landed).  WP11 (1-2 d).
  [WP3c receiver-side δ margin — DONE, round 3; its one missing behavioural test moves to WP9]
  Weeks 5-8:  WP5 exit-cost AND LATENCY re-derivation and the admission-rule change, with test churn.
  Weeks 3-6:  WP3b (moves with D9).  WP6 receiver-side refusal.  [D13's leaf-loop port — DONE]
  Weeks 2-10: WP9 verification backlog — sets VERIFIED/UNVERIFIED labels, not content.
```

| Deliverable | Estimate | Change |
|---|---|---|
| Unknowns bought down (WP1) | 1–2 weeks | unchanged (one workstream removed, one added) |
| Decisions closed (excluding D10) | **5–7 weeks** | **up** from 3–5: nine new decisions |
| Ladder-only v1 draft complete | 8–10 weeks | unchanged, on firmer evidence |
| Full protocol v1 draft complete | **12–16 weeks** | **down** from 14–18: three-and-a-half subsystems surveyed, D4 answered |
| Reviewed, evidence-labelled, published | + 2–3 weeks | unchanged |

**ONE thing I will not put a number on** (was three: D4 was retired in round 2, and **the
P2A/CPFP mechanism is retired in round 3 — someone tried, and it took days rather than an unknown**):

1. ~~**The P2A/CPFP mechanism.**~~ **CLOSED.** It was the right item to refuse to estimate — round 2
   correctly widened it from a builder to a builder *plus* a backend choice — and it came in at a
   measured spike plus a builder plus a Core-RPC route, all of which are in the tree
   (`lib/src/wallet/p2a_fee_child.rs`, `mercuryrustlib::core_rpc::submit_package`,
   `docs/utexo/notes/WP1-TRUC-P2A-SPIKE.md`). What replaces it on the risk list is not a number but a
   **population**: the mechanism serves only a wallet that has been given a fee source, and `fee_bump`
   is `None` on both shipped presets.
2. **D10 and the mainnet-schedule verification.** D10 may still be an excluded-scope collision. The
   mainnet schedule has never touched a chain — BIP-68 enforcement at 720/1440 magnitudes and the
   leaf budget are exercised only by arithmetic. Round 2 added a second never-exercised regime, the
   TRUC in-flight model; **round 3 retires that half** (the ancestor limit is now a measurement, not a
   reading of Core policy), leaving the mainnet CSV magnitudes and D10 itself.

**And two sequencing rules that are worth more than the estimates:**

- Do not wait for decisions to start drafting. §7 is written so a failed decision appends a
  subsection rather than retracting the MUST-list, and roughly half of the document depends on no
  open decision.
- Writing and publishing are different gates. Sections can be drafted against open items; publication
  requires WP3a landed, because a spec that contradicts a green test in its own repo is worse than no
  spec — and WP3a is **still open at HEAD**: the forged-yardstick tripwire still asserts that a ladder
  declaring 2 662 sat/vB over a 1 000 000-sat coin is ACCEPTED (`clients/libs/rust/src/tesr.rs:18145-18194`).
  Round 2 added WP3c to that gate for the same reason; **round 3 removes it — WP3c landed**, so §7 may
  state the δ MUST. One gate remains, not two.

**One honesty note about evidence.** WP2 makes the *unit* corpus reproducible. It does not make the
E2E corpus reproducible: the ~69 E2Es need docker plus regtest plus lockbox plus an RGB proxy, and
putting that stack in CI is not a 3-day job. So §26 must carry three labels, not two — VERIFIED,
UNVERIFIED, and **AUTHOR-ATTESTED** (a run with a commit and a timestamp, not reproducible by a third
party). Do not let "verified, unreproducibly" hide inside "verified".

**Round 2 sharpens that note in three places.** (i) **Tier 1's closure is the case where a wrong
label costs most.** C-1/C-2/H-1 have genuine in-crate unit coverage — named-refusal assertions at
`clients/libs/rust/src/tesr.rs:11229-11292` and `:11340`, the single-shared-function guard at
`:21457`, and the skim/exemption/dust attack rigs — but the end-to-end demonstration against **real
SE co-signatures** is `SDK_E2E=70`, which needs the regtest + lockbox stack. §7 may state the
coin-binding MUST-list as settled and code-backed; the live-SE half carries **AUTHOR-ATTESTED** until
WP2 lands. (ii) **Lightning is the worst case of all**: no unit corpus (six tests), an external
forked LN node reached through a hardcoded absolute path, and every §1 "what ships" claim
E2E-only. (iii) **Round 2 itself ran nothing.** Every "sdkNN pins X" statement in this document,
old and new, is asserted-not-observed.
