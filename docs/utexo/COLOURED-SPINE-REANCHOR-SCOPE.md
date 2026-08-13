# Coloured spine and coloured on-chain re-anchor — scope, design and re-cost

> ⚠️ **[D53] CORRECTION, 2026-08-14 — the depth cap in this document is STALE.**
> Every statement here of `max_split_depth = 10` / **23 transactions** (mainnet) or
> `max_split_depth = 68` / **139 transactions** (regtest) was measured against the BARE latency rule
> `exit_wait_blocks <= epoch`. The rule a conveyed child is actually ADMITTED by adds
> `exit_slack_margin`, so the shipped caps are **depth 8 / 19 transactions** (mainnet) and
> **depth 54 / 111 transactions** (regtest). Depths 9 and 10 were unadoptable at every tip. The
> 9 383-block / 65.2-day figure remains correct as the WAIT of a depth-10 walk — it is not the cap.
> Read `DECISIONS.md` D53 before carrying any depth figure from this document into the spec.


**Read-only investigation, `feat/spark`, 2026-08-11. No code written. Adversarially re-reviewed
2026-08-11 (both lenses: DEFECTIVE) and amended in place.** Every claim cites source. Unknowns are
marked **UNVERIFIED** and are not smoothed over.

> ### ⚠ PROVENANCE — how much scrutiny this document has had
>
> **The adversarial review HAS NOW RUN, and it changed the document.** The original run (three
> parallel investigations → a design → two critiques → this write-up) lost both critique agents to API
> errors, so §§1–4 shipped with nothing having attacked them. A second pass with the same two lenses —
> skeptical implementer and soundness — completed on 2026-08-11. **Both returned DEFECTIVE.** What
> they changed, all of it now folded into the text below rather than appended as caveats:
>
> * **A theft-class defect in the plain spine that ships today** (§1.8, new): the intermediate spine
>   `SP` is the one tier in `verify_child_bundle` with no Σ-payload law and no payload-count law
>   (`tesr.rs:9590-9601`, the code's own comment names the gap). Found by the soundness lens; not in
>   the original document at all. It is now **S0**, a Phase-0 blocker.
> * **The spine's headline claim was wrong.** §1.7 said the spine lifts the sender-side cap
>   "completely". Depth grows by exactly +1 per batch and never resets (`tesr.rs:4746-4749`,
>   `:5003-5006`), so a coloured carrier gets a bounded number of batches, not unlimited ones. Found
>   by the implementer lens. §1.6/§1.7 are rewritten and the re-anchor is re-ranked accordingly.
> * **A fourth re-anchor design exists and dominates two of the three.** **CR-D**, a coloured
>   *de-trigger*, needs no rival over `F` at all and no SE change (§2.3). Found by the soundness lens.
>   It demotes G2 from "the experiment that selects the product" to an optimisation.
> * **§1.5's mutual-exclusion argument was false**, and G1 was scoped as a spike that had already run.
>   Both rewritten; the remaining gap is a *build* workstream (**W1**), previously costed at zero.
> * **§5 items 1 and 2 were both wrong**: D8-CLOSE is built and live-verified but wired to nothing
>   (now **R0**, a hard prerequisite of the flag), and the "SE-attested census removes the absolute
>   clock" idea does not work.
> * Four citation errors corrected, and the already-struck `DECISIONS.md:104` recommendation deleted.
>
> **The original design work was done under a scope rule that has since been revoked.** The first-run
> agents were briefed "never propose changes to `enclave/` or `lockbox/`", lifted mid-flight by **D22**
> (`DECISIONS.md:212-247`). The re-review was not so constrained. Its finding on that point is
> negative and worth recording: **D22 opens no new re-anchor design.** The binding constraint is the
> nLockTime of flat backups already signed and already in prior owners' hands (§5 item 2), which no SE
> change revokes; and CR-D — the option the original menu missed — was reachable under the old rule
> too. The SE-shaped options that D22 *does* open are D8-CLOSE's wiring (R0) and D4's per-slot
> counters, neither of which is a re-anchor.
>
> **Independently corroborated:** the unbuilt fee-bump remedy was confirmed the same day by a live
> measurement on Core 30.2.0 (`notes/WP1-TRUC-P2A-SPIKE.md`): TRUC admits **two** in-flight tiers and
> refuses the third (`TRUC-violation … would have too many ancestors`), package-CPFP rescue **does**
> work, and **no `submitpackage` caller existed anywhere in this tree** (verified independently:
> `grep -rn 'submitpackage|submit_package' --include='*.rs'` = 0 hits; 63 `transaction_broadcast_raw`
> call sites under `clients/` + `lib/`, all electrum). *That grep no longer returns zero — W1 built
> `mercuryrustlib::core_rpc::submit_package` and the P2A fee child on top of exactly this
> measurement.*

Purpose: D1 (`docs/utexo/DECISIONS.md:88-114`) chose CTES-R as the normative RGB lane and flips
`SdkConfig::colored_ladder` to `true` (`clients/libs/rust-sdk/src/config.rs:283`, `:312`, both
currently `false`). D2 chose "land all remaining CATS-B units, then freeze". The coloured spine sits
behind both, and behind it sits the coloured on-chain re-anchor, which does not exist. Because these
were unscoped, `DECISIONS.md:268-272` withdrew the 12–16 week roadmap estimate. This document scopes
them and produces the number that un-withdraws it.

---

## 0. Verdict

### 0.1 Two blockers, both found by the adversarial pass, both ahead of any coloured work

> **BLOCKER 1 IS CLOSED (2026-08-11).** Both premises of the attack were removed, and both are now
> exercised rather than argued:
>   * the intermediate `SP` carries a **Σ-payload law** (`verify_conveyed_child`'s ancestor loop),
>     so the committed fee is no longer sender-chosen. Reproduced end to end as an attack test —
>     the starved slot is refused with *"Σ over its payload outputs is 990, but its funding of
>     198530 at 2 sat/vB requires exactly 198040"* — and the rig starves the slot AFTER the builder,
>     because the honest builder refuses it and an attacker does not use the honest builder;
>   * the supersession race no longer **presumes** relayability. It reads a
>     `LiveRival { csv, relayable, implied_fee, vsize }` and refuses when the live tier sits under
>     the relay floor, so the "it can never confirm" proof stops being a maturity argument about a
>     transaction that cannot enter a mempool. Recorded as D26 (both halves).
>
> The verdict below still names BLOCKER 1 as "the single largest risk"; that sentence is now
> historical.
>
> **BLOCKER 2 IS ANSWERED AND BUILT; ITS ACCEPTANCE RUN IS NOT YET OBSERVED.** It was the largest
> for one further step — its open design question, "who funds the bump", priced in
> `notes/CPFP-WHO-FUNDS-IT.md` — and then it was answered and built. **D31** took option (A),
> owner-funded, with the funded tower as a documented deployment option; all four parts of W1 exist
> in the tree (`core_rpc::submit_package`, `p2a_fee_child::build_p2a_fee_child`, `tower_float.rs`),
> and W1's own acceptance criterion is **written as a test** — `live_p2a_package_rescue.rs` builds an
> under-paying v3 parent, proves the node refuses it alone, and rescues it through repo code rather
> than a hand-run `bitcoin-cli`. **What is not evidenced is that it has been seen green:** the test
> needs `CORE_RPC_URL` and it skips loudly when the node's `minrelaytxfee` leaves no room to build an
> under-paying parent — *"Start bitcoind with a higher -minrelaytxfee to exercise the rescue. NOTHING
> was verified"* — and §4.1's W1 row still books that raised-floor E2E, with hardening, at 0.5–1 wk.
> So: **neither blocker is open as a design question or as a missing code path**, and BLOCKER 1 is
> closed outright (its attack test refuses by name, above); BLOCKER 2's remainder is a measurement on
> a configured node, not engineering. What §1.5 and §3's W1 row describe as unbuilt is the state they
> were written in; the landed state is recorded in §4.1.

**BLOCKER 1 — a theft path in the plain spine, live today (§1.8).** The intermediate spine `SP` — the
tier this whole document is about — is the single transaction in `verify_child_bundle` with **neither
a Σ-payload law nor a payload-count law** (`clients/libs/rust/src/tesr.rs:9590-9601`; the comment
there names the gap and the two lines below it bind only the anchor and the opret count). Nothing
ties `Σ(SP.outputs)` to the funding value, so the committed fee is sender-chosen. A sender can
co-sign an `SP` committing ~1 sat over 211 vB — three orders of magnitude below minrelay — hand out
pieces whose own arithmetic verifies perfectly (the leaf's law bases on `sp_out.value`,
`tesr.rs:9784-9800`), and then sweep the level back with the disclosed cap `C_i` after `d0` blocks.
The cap's "it can never confirm" proof is a maturity race (`verify_superseded_segment` marks a
superseded tier dead purely on `sup.csv > live_csv`, `tesr.rs:8420-8433`) and that race presumes the
live tier can relay. This is the D13 shape again: a structure that looks like tested code and
destroys pieces already conveyed to payees. It is **S0**, before S1, and it must be fixed
colour-branched (`crate::rgb::colored_tier_out_total` vs `mercurylib::tesr::tier_out_total`, the
pattern already at `tesr.rs:8866-8874`) because the coloured spine multiplies the exposure by depth.

**BLOCKER 2 — the fee-bump remedy is costed at zero and is unbuilt (§1.5, W1).** G1 was scoped as a
spike; the spike has run (`notes/WP1-TRUC-P2A-SPIKE.md`, commits `d797bce` / `5475d79`) and its
verdict is that package-CPFP rescue works *in Core* and **nothing in this tree can invoke it**: zero
`submitpackage` callers, all 63 broadcast sites electrum, the only CPFP builder pinned to
`input_vout = 0` / `version: 2` (`lib/src/wallet/cpfp_tx.rs:66`, `:110`) and therefore unusable on a
v3 parent, no builder anywhere constructing a `TxIn` on a P2A outpoint, and a keyless watchtower with
no UTXO to fund a child (`clients/libs/rust-sdk/src/watchtower.rs:3-4`). No item in the original
Phase 0/1/2/3 built any of it. That is a new **3–5 week workstream with an unresolved design question
inside it** (who funds the bump), not a gate that is already satisfied.

### 0.2 The rest of the verdict

`colored_ladder = true` is reachable, and the coloured **spine** is a bounded engineering unit rather
than a research problem — far more of it already exists in the tree than D1's table implies, and it
requires no `enclave/` or `lockbox/` change. But it buys **less than the original draft claimed**:
depth grows by exactly +1 per spine batch and never resets (`tesr.rs:4746-4749`, `:5003-5006`), and
the exit-chain length cap is enforced with a hardcoded two-tier per-level shape
(`exit_cap_shapes`), so a coloured root carrier gets `max_exit_txs − 4`
sequential batches — **19 on the mainnet schedule at `lockheight_init = 10 000`, which is the
deployed profile** — and is then refused while the tip is still whole (§1.6). The re-anchor is therefore not a follow-on to the spine: **it is
what makes the spine's slots renewable**, and it bounds a coloured carrier in payments as well as in
days.

The coloured **re-anchor** is not buildable in the shape D1 assumes ("colour the refresh
transaction"), because `refresh` routes through `withdraw`, an RGB-unaware builder that refuses
carriers one level up (`clients/libs/rust-sdk/src/refresh.rs:209-212`,
`clients/libs/rust-sdk/src/wallet.rs:2670`, `:2680-2691`). But the original three-design menu was not
exhaustive. **CR-D — a coloured de-trigger — needs no RGB rival over `F` at all**, costs 2
transactions and zero CSV wait, needs no SE change, and reuses a role tag that is already allocated
(`TierRole::Detrigger = 0x06`, `clients/libs/rust/src/rgb.rs:775`). It dominates CR-B on every axis
and removes most of CR-C's motivation (§2.3). Consequently **G2 no longer selects the product** — it
selects one transaction versus two.

The shortest honest path: **(i)** fix S0 and wire R0 (the built-but-uncalled census attestation);
**(ii)** start W1, the package/anchor/fee-child workstream, in parallel; **(iii)** build the spine,
whose long pole is the receiver's N-deep seal/witness walk — which also gates every re-transfer of
every piece the spine mints (§1.4); **(iv)** build CR-D, and run G2 only to decide whether CR-A's
single-transaction form is also available. **Steps (i) and (iv) are done, and (ii) is built with its
acceptance E2E still unrun on a raised-floor node** (§0.1, §4.1); the path that remains is (iii), and
S5 inside it.

**Re-cost (2026-08-11, §4.1): 11–21 engineer-weeks single-threaded, ≈7–13 calendar weeks with two
tracks — or, in the unit this work is actually done in, ≈6–9 AGENT SESSIONS (§4.2)**, re-costing the roadmap at **12–22 weeks to a full protocol v1 draft, +2–3 to publication**.
*(The 19–27 / 20–27 figures below were the previous pass; they are superseded by §4.1, which
re-checked every line against the tree. S0 and R0 are done, W1's four parts are built (its rescue
E2E unrun — §0.1), and the plain spine landed in the meantime. **§4.1's own 11–21 is now a superseded baseline too** — CR-D, P2, P3, P5, G3, G4 and the
S1–S4b/S7/S9 stretch have since landed, P1 shipped as corrected by D35, and P4 went to zero on D33,
leaving **≈4.5–6.5 engineer-weeks** of open work: S5, S6, S8's remaining half, G2, and W1's remaining
hardening plus its raised-floor E2E. That total is the sum of §4.1's own surviving rows on §3's own
bands — 17–24 d of spine, 2–3 d of G2, 2.5–5 d of W1 remainder, 0–1 d for P1's residual call-path
test = 21.5–33 d — and it deliberately does **not** carry P1, P2 or P5, all of which have landed.
§4.2 restates the open build items as **~2.5–3.5 agent sessions**; the W1 E2E is node-bound rather
than typing-bound and has no session row.)* The movement from
the original 14–20 is itemised in §4: +3–5 for W1 (previously zero), +0.5–1 for S0 and R0
(previously not present), +1–2 from re-banding S5, less the re-anchor branch narrowing now that CR-D
replaces the CR-B/CR-C fork. ~~**The single largest risk is BLOCKER 1** — a live theft path in
shipped code — followed by W1's unresolved "who funds the bump".~~ **BLOCKER 1 is closed outright and
BLOCKER 2 is closed as a design question and as a code path, with only its raised-floor E2E unrun
(§0.1). The single largest remaining risk is S5**, the receiver's N-deep seal/witness walk, which is
also a sender-side gate on every re-transfer of every piece the spine mints.

---

## 1. The coloured spine

### 1.1 Mechanism — what the plain spine is, precisely

The plain spine ships and is the reference shape. `spine_batch_split`
(`clients/libs/rust/src/tesr.rs:4631`) spends the current tip's **un-broadcast** funding outpoint
`SP_i.out[K]` — read from the signature-committed record via `SpineTipBundle::funding_outpoint()`
(`:684`) — with a single split state `SP_{i+1}` at `SPINE_CSV = 0` (`:5346`), built by
`mercurylib::tesr::build_split_state_from` (`lib/src/tesr.rs:535`, which exists because after batch 1
the tip's vout is ≥ 1 and the vout-0 builder would sign over a payee's outpoint). `SP_{i+1}` carries
K payee payload outputs plus one more, and:

* each **payee leg** gets a two-tier ladder off `SP_{i+1}.out[j]` — extension at `p.ext_csv(0)`,
  state at `p.state_csv(0)` (`establish_child_journalled`);
* the **change leg** gets ONE tier, the **cap**, re-anchoring directly on `SP_{i+1}.out[K]` at
  `p.state_csv(0)` via `build_state_from` (`lib/src/tesr.rs:443`), built by
  `establish_spine_tip_journalled` (`tesr.rs:3935`).

A tip differs from a child in exactly three ways: one tier not two; it re-anchors on its own funding
outpoint rather than on an extension's payload; and its CSV sits in the slow `[d_floor, d0]` band,
never at 0. That last is load-bearing and non-obvious — at 0 the cap ties with every future `SP`, the
`cap_csv <= SPINE_CSV` guard (`:4727`) refuses the next batch, and the tip is stranded permanently at
the one moment its owner cannot undo it (`:4608-4615`).

The retired cap `C_i` is disclosed into the new segment's `superseded_states` by `spine_segment`
(`:4578`), with `superseded_extensions` kept **empty** — a spine segment has no extension rung, and
the verifier refuses a non-empty list because that is exactly the route a two-tier segment would take
to re-balance the census after dropping a tier (`:1270-1278`). Census per spine slot:
`CHILD_V2_BASELINE(0) + 1 live + 1 superseded = 2`, exact equality against the SE's `num_sigs`.

### 1.2 What colouring it means, tier by tier

CTES-R colours a tier by handing the built v3/TRUC transaction to rgb-lib before signing:
`build_colored_tier` (`clients/libs/rust/src/rgb.rs:1171`) takes a raw `sequence` (`:987`) and an
arbitrary-length payload list (`:992`); rgb-lib's `opreturn_first` inserts the opret at index 0 so
payload vouts shift by one and are **derived, never assumed** (`colored_payload_vouts`, `:869`;
cross-checked output-by-output at `:1308-1326`). The seal blinding is derived from
`TierSeal{statechain_id, role, tier_index, rung}` (`:806`, `tesr.rs:1419-1428`).

| tier | spends | pays | commits to |
|---|---|---|---|
| `SP_{i+1}` (K+1 payloads, nSequence 0) | `SP_i.out[K]` (un-broadcast) | K payee aggregates + new tip's aggregate + P2A | transition closing the seal at `(SP_i.txid, K, blinding(tip_sid, SplitState, 0, rung 0))` |
| piece `ext_child_j` | `SP_{i+1}.out[j]` | payee aggregate + P2A | `blinding(piece_sid, ChildExtension, 0, e0)` |
| piece `state_child_j` | `ext_child_j.out[payload]` | payee exit key + P2A | `blinding(piece_sid, ChildState, 0, d0)` |
| **cap `C_{i+1}`** | `SP_{i+1}.out[K']` | this wallet's own exit key + P2A | `blinding(tip_sid, **Spine**, 0, d0)` — a **new role tag** |

Two facts make this sound rather than hoped. `build_colored_ladder` (`tesr.rs:1569`) already colours
`T → X_0 → S_0` over consecutive un-broadcast outputs in one engine phase (`:1622-1682`), and
`build_colored_receiver_state` / `build_colored_renewal` (`:1903`, `:2085`) already colour **rival**
transitions over an outpoint a retained tier has consumed. `build_colored_in_ladder_split`
(`:2548-2563`) already builds a coloured `SP` at `sequence = csv_blocks(SPINE_CSV).0`. **A zero-CSV
coloured tier is not a new kind of tier.**

Seal uniqueness falls out of the spine's shape: every level is a fresh statechain slot, so
`(sid, role, rung)` never repeats across levels; within a level the cap (role `Spine`, rung `d0`) and
`SP_{i+1}` (role `SplitState`, rung `0`) differ in both role and rung. `TierRole` tags are a stable
wire format (`rgb.rs:767-781`) — **append `Spine = 0x0C`, never renumber.**

### 1.3 Builders required — the four sites that refuse by name

The supporting infrastructure was written ahead of the shape and is dead-but-correct: `ColoredTip`
(`tesr.rs:592`, deliberately not `ColoredChild` because that struct's contract is a two-entry
`consignments` list), `SpineTipBundle::rgb` (`:663`), `colored_exit_move`'s `LadderRecord::Tip` arm
(`:974-985`), `colored_spine_tip_floor` (`:2366`), `SplitLegRole::colored_min_value`'s `SpineTip` arm
(`:3178`), and `refuse_uncolored_over_colored_tip` (`:885`).

| # | Site | Refusal | What must be built |
|---|---|---|---|
| 1 | `establish_spine_tip_journalled` | `tesr.rs:3965-3972` — *"leg {j} … is a COLOURED spine tip — no builder produces one yet, and a cap co-signed without its RGB transition would move the allocation nowhere"* | The coloured cap builder: one `build_colored_tier` over `SP.out[K]` at `p.state_csv(0)`, one payload, whole remaining allocation, role `Spine` |
| 2 | `spine_batch_split` | `tesr.rs:4646`, `refuse_uncolored_over_colored_tip` as its first statement | `build_colored_spine_batch` / `cosign_colored_spine_batch`, siblings of `build_colored_in_ladder_split` / `cosign_colored_in_ladder_split` (`:2440`, `:2691`) rooted at a tip's funding outpoint instead of `X_m`'s payload |
| 3 | `change_leg_role(SplitLane::Colored)` | `tesr.rs:3232` still returns `Piece` | Must become `SpineTip` **in the same commit as (1)** — `SplitLane::SpineBatch` already maps there. The doc-comment at `:3218-3228` is explicit that flipping early **fails open**: payment admitted at the 906-sat floor, parent terminalized, two-tier builder then cannot fund the second rung — coin stranded to unilateral-exit-only |
| 4 | `SplitJournalRecord::spine_tip` | `tesr.rs:3484-3490` refuses `c.rgb.is_some()` | The journal's per-leg RGB field is `Option<ColoredChild>` (`:3299`), contract = two consignments; a one-cap tip does not fit. Needs a tip variant plus a `PendingTier` for the cap (`PendingTier` at `:3119` is coloured-lane-only precisely because a coloured tier cannot be rebuilt at recovery time — the RGB engine phase is gone) |

Plus registration in `uncoloured_builder_census` (`:10245-10309`), which fails the build if a new
tier builder carries none of the three `refuse_uncolored_over_colored*` guards.

### 1.4 What the receiver must validate — the one genuinely new part

Today `colored_child_txids` (`tesr.rs:467-478`) hard-refuses any child with non-empty `ancestors`,
and `colored_child_seals` (`:509-552`) emits a **hard-coded five-entry** schedule
`[T, X_m, SP, ext_child, state_child]`. A piece at spine depth `d` needs `4 + d` entries: `T`, `X_m`,
`SP_1..SP_d`, `ext_child`, `state_child`.

**This is not receiver-side bookkeeping — it gates the SENDER side of every piece the spine mints.**
`build_colored_child_retransfer` calls `cb.colored_child_seals()?` as its second statement
(`tesr.rs:6363`, *"Depth-1 + single-level parent + a derivable seal schedule, all in one"*), and
every piece `spine_batch_split` mints carries `ancestors: ancestors.clone()` (`tesr.rs:4941`), i.e.
depth ≥ 1. So until S5 lands, **no piece produced by a coloured spine can be re-transferred at all**.
§1.7's "re-transferable whole ~36 times" is a property of depth-0 pieces, and after the spine there
are none. S5 is a hard gate on the spine's usefulness, not a nicety — which is why it is re-banded in
§3.

This is bookkeeping, not an RGB limitation. `accept_offchain_ladder` is arity-generic in
`offchain_txids` and `seals` (`/Users/gofman/Claude/utexo-rgb-lib/src/wallet/rust_only.rs:756-825`);
the receive path uses only the **leaf** consignment plus the txid list
(`clients/libs/rust-sdk/src/tokens.rs:2235-2301`); and `ChildSegment` already carries
`statechain_id` and `funding_vout` (`tesr.rs:1240-1248`), which is everything a receiver needs to
derive an intermediate `SP`'s seal. Every witness stays `Tentative`, resolved from the consignment's
own bundle by the `OffchainResolver` — never through the indexer, where `Unresolved → Archived`
silently and recursively invalidates the branch (`tesr.rs:1330-1334`).

**The one real gap is a wire-format decision.** `verify_colored_shape` enforces
`rgb.consignments.len() == tiers.len()`, *"indexed by exit order"* (`tesr.rs:9183-9190`), with the
child-lane twin at `tokens.rs:1676-1684`. `ChildSegment` has no consignment field. Either the
invariant is re-expressed as leaf-only for an N-segment chain, or `ChildSegment` gains an optional
consignment — and that struct's doc-comment explicitly forbids `#[serde(default)]` on conveyed
fields because a mailbox message could simply omit them (`:1263-1266`). A second, related invariant
must be **stated and enforced**: every intermediate spine segment's renewal counter is 0. There is no
`m` field on `ChildSegment` and no tip renewal exists, so it is structurally true today — but nothing
checks it, and the seal rung packs `m` (`:1419-1428`).

### 1.5 The unbuilt remedy — and the argument that was wrong about it

The original draft made two claims here. One is true and is BLOCKER 2. The other was false and is
withdrawn.

**TRUE, and unbuilt AT THE TIME OF WRITING (W1).** The four bullets below are the problem statement
W1 was built from; each is now closed, and the landed shape is in §4.1's W1 row. They are kept
because the design still turns on *why* each one was load-bearing. Every tier's fee is frozen at `committed_fee_rate = 2.0` for both presets
(`lib/src/tesr.rs:210`, `:215`), baked in at signing. Above 2 sat/vB a tier does not relay standalone
(`lib/src/tesr.rs:85-88`), leaving the P2A anchor as the only remedy — and **the remedy has no
implementation anywhere in this tree**:

* No anchor spender. `SPEC-ROADMAP.md:161`, `:164` records the grep: `P2A_SCRIPT_BYTES` /
  `p2a_script()` / `P2A_VALUE` appear only at definition, attach and vbyte-accounting sites; no
  builder constructs a `TxIn` on a P2A outpoint.
* No usable fee child. The only CPFP builder fixes `input_vout = 0` (`lib/src/wallet/cpfp_tx.rs:66`)
  and `version: 2` (`:110`); a v2 transaction may not spend an unconfirmed v3 output, and the
  vout pin means it is a rewrite, not a parameter.
* No package route. `grep -rn 'submitpackage|submit_package' --include='*.rs'` returns **zero** hits;
  all **63** broadcast sites under `clients/` + `lib/` are
  `electrum_client::transaction_broadcast_raw`, and electrum's protocol has no `submitpackage`
  equivalent (`notes/WP1-TRUC-P2A-SPIKE.md:101-108`). This is a new transport, not a flag.
* No funding source. A keyless watchtower holds no UTXO by definition
  (`clients/libs/rust-sdk/src/watchtower.rs:3-4`) and `PROTOCOL.md:525`'s prepaid fee bond is unbuilt
  (`notes/WP1-TRUC-P2A-SPIKE.md:109-113`). **This one is design, not coding.**

**WITHDRAWN: "the fee-bump remedy and the shape that most needs it are mutually exclusive."** That
does not follow. The argument was that TRUC allows an unconfirmed parent exactly one unconfirmed
child and `SP_{i+1}` at CSV 0 occupies it. But WP1 measured that the **third** in-flight v3 tier is
refused outright (`TRUC-violation … would have too many ancestors`), so a deep exit walk cannot be
submitted in bulk under any policy — it is sequential regardless. In that regime `SP_i` sits alone
and unconfirmed, its one TRUC child slot is empty, and a 1P1C `submitpackage` with a fee child on its
P2A anchor is exactly the case WP1 measured as **working**. The residual is therefore a **latency**
cost — roughly one extra confirmation per level, spent on the bump instead of on the next tier — not
a shape contradiction. It belongs in S8's latency model, which currently charges zero for the
per-level parent confirmation (§1.6).

What survives unchanged: `PROTOCOL.md:245-247`'s claim that *"each tier confirms before the next is
valid, so no long vulnerable chains"* is **false for exactly the spine**, and `DECISIONS.md:148-155`
already records that §6 must be rewritten.

Consequence for costing: a coloured spine exit is executable at ≤ 2 sat/vB and **not executable above
it** on the tier alone — the rescue is the P2A package, not the tier. W1 never gated the spine's
*design*; it gated the published exit-cost headline and the product claim that every coin stays
unilaterally exitable. **W1 has since shipped**, so the rescue path exists in repo code and the
headline is writable — bounded by what D31 actually promises: the owner funds the bump, a keyless
tower cannot, and during a fee spike the defence is the owner being online.

The coloured economics are already thin, which is why this matters rather than being an edge case. A
coloured leaf walking its own two rungs loses 1 152 sat of a 3 066-sat piece (37.6%), leaving a
**1 914-sat** owner output; generously accounted (leaf's own two rungs only, ~152 vB v3 CPFP child
per `SUBECONOMIC-FINALITY.md:55-58`), the external-sats break-even for exiting it is ≈ **3.3
sat/vB**. **STILL UNVERIFIED, but now measurable**: that 152 vB is a cited figure, not a
measurement. It was unmeasurable when written because no anchor spender existed; W1 has since built
one, and `p2a_fee_child::estimate_child_vsize` is the function that should replace the citation.

### 1.6 Numbers

`COLORED_TIER_VBYTES = 168` (`rgb.rs:916`) vs `TIER_VBYTES = 125` (`lib/src/tesr.rs:93`) — the
difference is exactly one `P2TR_OUT_VBYTES = 43`, the opret and nothing else, and both are MEASURED
through the production finaliser (`rgb.rs:910-926`). `colored_tier_vbytes(n) = 168 + (n−1)·43`
(measured 168 / 211 / 254 at n = 1 / 2 / 3).

| quantity | plain | coloured |
|---|---|---|
| spine level at K=1 (`SP`, 2 payloads) | 168 vB / 336 sat committed | **211 vB / 422 sat** (+43 vB, +86 sat) |
| exit walk of a piece at spine depth `d` | `4 + d` txs, `500 + 168d` vB (668 @ d=1) | `4 + d` txs, **`672 + 211d` vB** (883 @ d=1) |
| change-leg admission floor @ 2 sat/vB | 1 310 → **820** (saves 490) | 1 482 → **906** (saves 576) |

* **Depth cap — and it binds, which the original draft's §1.7 denied.** A spine level costs one
  block of latency and a whole transaction, so the *latency* rule never refuses it; the **length**
  cap does. `enforce_exit_chain_length` measures with `exit_cap_shapes`, whose `per_level` is
  hardcoded `SplitLevelShape::TwoTier` — so `max_exit_txs` is computed as if
  every level were two-tier even when the chain is all spine. A spine leaf's real chain is `4 + d`
  transactions, hence **`d ≤ max_exit_txs − 4`**.
  * Mainnet at `lockheight_init = 10 000`: `base_wait = 1+721+1+721+1441 = 2 885`; `TwoTier` is
    `[ext_csv(0), SPINE_CSV]` = `[720, 0]` (`SplitLevelShape::csvs`), i.e. 722 blocks; so
    `max_split_depth = 1 + (10 000 − 2 885)/722 = 10` and `max_exit_txs = 23` ⟹ **d ≤ 19**. This
    reproduces `mercurylib::transfer::receiver::max_exit_txs`'s own doc-comment exactly — both of
    its figures check out — and `honest_spine_batch_pieces_and_tips_are_admitted_up_to_batch_nineteen`
    pins the consequence: batch 19's piece is the last that fits, batch 20's is 24 transactions and
    is refused.
  * Regtest profile at `lockheight_init = 1 000` on the regtest schedule: `base_wait = 53`,
    `TwoTier` = 14, `max_split_depth = 68`, `max_exit_txs = 139` ⟹ **d ≤ 135**, whose TRUC stall
    alone (~68 blocks) exceeds the entire regtest state schedule (`DECISIONS.md:73-77`). **This is
    no longer the deployed profile** — see the next bullet.
  * **The mainnet epoch is now configured, and it is the DEPLOYED one.** *(Superseded problem
    statement, kept because it is what G3/D7 were built to fix: this document was written when the
    only `lockheight_init` in the tree was **1 000** and `network = "testnet"` fell through to the
    **regtest** `TesrParams`, so mainnet params at `initlock = 1 000` gave `max_split_depth = 0`,
    `max_exit_txs = 3`, and every in-ladder split refused before depth was considered — "d ≤ 19"
    presumed an epoch no file set. The requirement that statement carried, and the reason the
    obvious weaker gate would not have been enough: **G3's gate must assert
    `initlock ≥ exit_wait_blocks(base)` for each profile, not merely that every network string
    resolves without a silent default** — a profile can name itself cleanly, resolve to a real
    `TesrParams`, and still be unable to finish a depth-0 exit inside one epoch, in which case the
    horizon pre-flight refuses every in-ladder split on it, permanently and for every user. That is
    the assertion `g3_every_profile_can_finish_its_exit` makes, over all six profiles and at depth 1
    as well as depth 0 — see §3's G3 row.)* **Both halves are closed at HEAD.** D25 makes
    `testnet`/`testnet3`/`testnet4`/`signet` run the **mainnet** schedule
    (`TesrParams::testnet() = Self::mainnet()`), `for_network_checked` names every network and
    returns `None` rather than guessing, and `server/Settings.toml` now reads
    `network = "testnet"` with **`lockheight_init = 10 000`** / `lh_decrement = 100` — matching
    `flat_ladder_params_const`'s compiled `(10_000, 100)` for that network, which
    `server_config.rs` refuses to boot against a mismatch of. **So the deployed profile IS the
    mainnet schedule at a 10 000-block epoch**, and every mainnet figure in this section is the
    deployed one: `max_split_depth = 10`, `max_exit_txs = 23`, **d ≤ 19** spine batches.
* **Latency — the two models that disagreed have been reconciled; the half S8 still owns is the
  spine's transaction COUNT.** *(Before: `tesr_exit_wait_blocks` gave `720d + 2160` and charged
  **nothing** for the one block per level the parent needs to confirm, while
  `exit_wait_blocks` (`mercurylib::transfer::receiver`) summed `csv + 1` per transaction and **did**
  charge it — and it is the model `max_exit_txs` and the whole depth cap derive from. The two were
  off by one block per tier, one of them a receiver-side admission rule.)* **After:**
  `tesr_exit_wait_blocks` now returns `csv_total + tesr_exit_txs(d)`, i.e. **`722d + 2163`** on the
  mainnet schedule, and the code carries the reasoning under an `[S8]` heading — *"a tier's RELATIVE
  timelock starts counting from its parent's CONFIRMATION"*, and under-counting was *"wrong in the
  dangerous direction"* because this figure feeds `auto_exit_due`. `tesr_exit_csv_total` was split
  out so the locks-only question keeps its own name. **What is still open under S8** is the other
  term: `tesr_exit_txs(d) = 3 + 2d` still charges every level as two-tier, so a spine's real `4 + d`
  is over-counted in the SDK cost model (next bullet). `DECISIONS.md:205-211` records the same class
  of disagreement for the watchtower head-start. Under TRUC the practical floor is ~1 block per 2
  consecutive zero-CSV tiers, or 1 per tier if the anchor slot is spent on a fee bump (§1.5).
* **Cost model over-counts a spine level in both lanes.** `tesr_exit_txs(d) = 3 + 2d`
  (`config.rs:166`) charges every level as two-tier; the truth for a spine is `4 + d`
  (`CATS-B-PHASE1-PLAN.md:486`). Over-counting refuses depth and inflates the auto-exit margin, so it
  is conservative, not a correctness bug — but the published depth/latency headline for a coloured
  spine is **not currently derivable from the code**. That is WP5/D6, already scheduled; note D6
  moves a **receiver-side admission rule**, so it is test churn, not arithmetic.

### 1.7 What the spine buys — stated precisely

`CTESR_CARRIER_SEND_DEPTH = 1` (`tokens.rs:131`) is documented as *"not a sizing choice — a
structural property"*: the change of a coloured split is persisted as a depth-1 **child** and three
independent guards refuse to carve a second piece out of it (`tesr.rs:571`, `:467-471`,
`tokens.rs:117-123`). The spine changes what the change **is**: a tip, not a child. So it raises the
cap on the sender side from **one** payment to **`max_exit_txs − 4`** payments — **19** on the
mainnet schedule at `lockheight_init = 10 000`, which is the schedule and the epoch the coordinator
actually ships (135 on regtest, §1.6).

It does **not** lift the cap. The original draft said the spine lifts it "completely — batch 1 and
batch 1000 built by the same function". That is false: `spine_batch_split` rebuilds `levels` from
`tip.ancestors` and pushes one more `SplitLevelShape::Spine` on every batch
(`tesr.rs:4746-4749`), the new tip inherits `ancestors: ancestors.clone()` after the segment was
already pushed (`:4891`, `:4913`), and the cap's own doc-comment states the rule flatly: *"Depth
grows by exactly +1 per in-ladder payment and never resets"* (`:5003-5006`). Batch 20 on mainnet is
refused — cleanly, while the tip is still whole and exitable (`:4750`, checked before the tip is
terminalized), but refused. **This is why the re-anchor is not a follow-on to the spine: it is what
makes the spine's slots renewable.** A carrier under CR-C is bounded in *payments* as well as in
days, which makes CR-C materially worse than §2.3's original framing.

It does not lift the cap on the receiver side either. After the spine lands, the honest end state of
a coloured coin is:

* **Sender / root carrier** — up to `max_exit_txs − 4` partial payments (**19** on the deployed
  mainnet schedule at `initlock = 10 000`, 135 on regtest), then refused; and bounded independently
  by the absolute epoch deadline (§2.4), whichever binds first.
* **Received piece (leaf)** — re-transferable **whole** ~36 times via
  `build_colored_child_retransfer` (`tesr.rs:6350`, δ=36 from d0=1440 to d_floor=144), then terminal.
  It can never be **split** (`tesr.rs:571-582`: *"a coloured child-level SPLIT does not exist; split
  at the root instead"*), never **renewed** (`plan_child_renewal` refuses coloured first, `:19322`),
  never **combined** (`combine_leaves` refuses coloured leaves fail-closed,
  `clients/libs/rust/src/combine.rs:22-26`), and never **re-anchored** — structurally, since its
  funding `SP.out[j]` is un-broadcast, so there is no confirmed outpoint to spend.

Two capabilities also **disappear silently when the flag flips** and are not in D1's table:
coloured **multi-carrier combine** (`refuse_if_colored_ladder` per input, `tokens.rs:4566-4570`;
`CTESR-GATE.md:497` records it as intended) and coloured **leaf consolidation**
(`combine.rs:22-26`). The advertised replacement `colored_multi_carrier_transfer` is a multi-*piece
payment*, not a combine, and is explicitly **non-atomic with no F7 journal**
(`tokens.rs:4145-4171`) — a failure on leg k>0 leaves the recipient short-paid with legs 0..k already
conveyed. That belongs in the inventory before D3 drafts a `V_min` table.

**And one privacy property the spine converts from bounded to transitive.**
`AssetColoringInfo.static_blinding` is a single `Option<u64>` per transition
(`/Users/gofman/Claude/utexo-rgb-lib/src/wallet/rust_only.rs:19-20`) applied to **every** vout via
`GraphSeal::with_blinded_vout(vout, blinding)` (`:480-483`), and `colored_tier_seal` is a pure
function of `(sid, role, level, m, csv)` — all fields a payee already holds from the conveyed bundle
(`tesr.rs:527-552`, `ChildSegment::statechain_id` at `:1240-1248`). So a payee **derives** the shared
blinding rather than attacking it, and `colored_child_seals`'s doc-comment claim that *"revealing
`(sp_vout_j, blinding)` opens child j's assignment and no other"* (`:505-508`) holds only against a
receiver that does not derive. At K=1 today this leaks the sender's own change seal to the one payee
already transacting with them — `tokens.rs:208-218` names that cost and accepts it explicitly, for
the one-level case. **On a spine that reasoning no longer holds**: a payee at depth `d` is handed `d`
intermediate `SP` seals and must derive each to validate, so it can open the sender's running-balance
leg at every level back to the root. Not theft — a seal is not spendable without the key — but it is
a new cost the spine creates, and it can corrupt the payee's own accounting, since
`accept_offchain_ladder` `store_secret_seal`s every supplied seal before validating
(`rust_only.rs:779-782`) and `unspendable_as_btc_outpoints` unions `list_allocations`
(`tokens.rs:1625-1631`). Either P2 moves in front of S4, or §10 states that concealment across a
coloured spine is worth zero bits. It is **not** true, as §3's P2 row originally said, that this only
gates K>1.

### 1.8 BLOCKER — the intermediate `SP` has no committed-fee law, and that is a theft path

Found by the adversarial pass; absent from the original draft, which read the dust and opret binds
three lines away and did not notice what sat between them.

**The gap.** In `verify_child_bundle`'s ancestor loop, a two-tier segment's EXTENSION gets a Σ law
(`tesr.rs:9468-9487`, `expect = tier_out_value(fund_out.value, cb.parent.fee_rate)` summed over every
non-anchor, non-opret output). The segment's `state` — which for **every spine level IS `SP_{i+1}`** —
gets only `refuse_dust_payloads` (`:9596`), `bind_single_p2a_anchor` (`:9601`) and `bind_opret_count`
(`:9602`). The code's own comment says it: *"the ONE tier in the whole structure with neither a
Σ-payload law nor a payload-count law"* (`:9590-9595`).

**Why nothing else catches it.** `verify_tier_cosigned` proves only that the SE signed it. The dust
floor passes with every leg well above dust. The leaf's own law bases on `sp_out.value`
(`tesr.rs:9784-9800`), so the payee's arithmetic is internally consistent *against the tampered
parent*. And the census is a count, not a value check (`:9093-9098`).

**The attack.** The sender builds `SP_{i+1}` committing ~1 sat of fee over 211 vB (≈ 0.005 sat/vB,
three orders of magnitude under minrelay), pays every payee leg exactly honestly, and parks the
surplus on its own tip leg. Every payee verifies and accepts. Later the sender broadcasts
`T, X, SP_1..SP_i` on chain, waits `d0` = 1 440 blocks, and broadcasts the retired cap `C_i` — which
pays the sender the entire outpoint. The payees' `SP_{i+1}` can never enter any mempool, so no piece
beneath it can ever be broadcast. The disclosed cap's whole "it is dead" argument is
`verify_superseded_segment` marking it dead on `sup.csv > live_csv` (`tesr.rs:8420-8433`) — a maturity
race that is only decisive if **both** transactions can relay. `spine_segment` puts `C_i` into the
same segment's `superseded_states` over the same outpoint `SP_i.out[K]` (`:4578-4590`), so the
attacker needs no extra co-sign and the census balances.

**This is live in the plain spine today.** The coloured spine multiplies it by depth and makes the
fix harder, because the correct law must branch on colour.

**The fix (S0, Phase 0, before S1).** Add the Σ + payload-count law to the ancestor `state` in
`verify_child_bundle`, colour-branched exactly as the root lane already does at `tesr.rs:8866-8874`
(`if bundle.is_colored() { colored_tier_out_total(prev, n, rate) } else { tier_out_total(...) }`),
with `n` derived from the transaction (outputs less P2A less oprets) and `prev = fund_out.value`.
Adversarial test first: a co-signed spine segment committing a 1-sat fee must be refused **by name**.

**And fix the adjacent contradiction in the same commit.** The ancestor EXTENSION's Σ law is
colour-**blind** — `tesr.rs:9468` calls `mercurylib::tesr::tier_out_value` with no
`cb.parent.is_colored()` branch, i.e. the plain 125-vB fee — while `bind_opret_count` three lines
below (`:9501-9505`) requires exactly one opret when the record is coloured. A coloured extension
necessarily forwards `prev − colored_committed_fee(1, rate) − P2A`, which is `43·rate` sat less than
the plain law expects (`COLORED_TIER_VBYTES = 168` vs `TIER_VBYTES = 125`, `rgb.rs:924`,
`lib/src/tesr.rs:93`), so the two checks can **never** both pass: any coloured two-tier ancestor
segment is structurally unverifiable. It fails closed, so it is not theft — but S5 is exactly the
commit where someone generalises this loop, and copying the extension's line into the newly
load-bearing state law would propagate the bug into a check that does not fail closed.

---

## 2. The coloured on-chain re-anchor

### 2.1 Not buildable as assumed

D1's table row assumes "colour the refresh transaction". That is wrong at three levels.

**It is not a flag on the existing driver.** `reanchor`
(`clients/libs/rust-sdk/src/refresh.rs:131-234`) mints a fresh deposit aggregate for exactly
`amount − ceil(112 · rate)` using a *free* derived-slot voucher (`:184-198`) and then calls
`self.withdraw(&fresh_addr, …)` (`:209-212`). `withdraw` (`clients/libs/rust-sdk/src/wallet.rs:2670`)
refuses a named carrier one level up (`:2680-2691`) and `withdraw::execute → new_transaction` is a
plain, RGB-unaware builder (`clients/libs/rust/src/withdraw.rs:73-84`). A coloured re-anchor needs
its own driver end to end.

**The refusal at `refresh.rs:152-157` is precise, not conservative.** For a CTES-R coin the
allocation is sealed on the confirmed funding outpoint `F`: `build_colored_ladder` builds the trigger
with `prev_txid/prev_vout = (coin.utxo_txid, coin.utxo_vout)` (`tesr.rs:1621-1640`). The re-anchor
spends that exact outpoint. Built plain, it closes the seal with an RGB-unaware witness and the
allocation is gone — silently, because the transaction is perfectly valid Bitcoin, and irrecoverably,
because the outpoint is spent on chain. The guard is also better than it looks:
`unspendable_as_btc_outpoints` (`tokens.rs:1625-1631`) is the union of rgb-lib's `list_allocations`
and `consignment_bearing_outpoints` (`:1528-1562`), the latter quarantining every coin whose `tesr-`
row is or might be coloured, failing **closed** on an unreadable row. Flipping the flag opens no hole
in `refresh`.

**The RGB-aware half that exists cannot be broadcast as written.**
`create_colored_backup_tx(is_withdrawal = true)` (`clients/libs/rust/src/rgb.rs:66-186`) is called
with `true` only from a test that deliberately does not broadcast
(`clients/tests/rust/src/rgb09_send_receive_blinded_witness.rs:219`). Its fee comes from
`create_tx_out`, which hard-codes `BACKUP_TX_SIZE = 112` and adds nothing for the opret
(`lib/src/transaction.rs:116-132`). A ~155-vB coloured re-anchor built through it commits 112 sat at
1 sat/vB — **0.72 sat/vB, below mainnet minrelay**. It would not propagate. That constant is shared
with the plain flat-backup lane, so it must **not** be edited; the coloured re-anchor needs its own
fee path.

### 2.2 The question CR-A must answer — no longer the question that selects the product

⚠ **Re-ranked by the adversarial pass.** Everything below is true **of CR-A**, and CR-A alone. The
original draft called it *"the highest-value thing to measure in the whole programme"* and branched
all of Phase 2 on it. **CR-D (§2.3) makes the rivalry over `F` self-inflicted** — the ladder's own
trigger `T` is already the canonical coloured consumer of `F`'s allocation, so broadcasting `T`
creates no RGB rival at all. G2 is therefore an *optimisation* gate: pass ⟹ the re-anchor is one
transaction; fail ⟹ it is two, plus one confirmation. It no longer selects a product tier.

After a coloured re-anchor there are **two RGB transitions consuming the same parent Opout at `F`**:
the ladder's trigger `T` (Tentative, un-broadcast, in the sender's own stash) and the re-anchor
(Mined). This is exactly the collapse class `CTESR-GATE.md:113-172` measured — five rivals over one
parent output with a shared blinding produced **one OpId and one BundleId**, the winner was the
numerically smallest internal txid ("an arbitrary hash lottery"), and hop 2 could not succeed,
intermittently, at ~1/n. Deriving a distinct blinding under a new role tag addresses *identity*, and
the build-time collision assert (`rgb.rs:1377-1389`) is the pattern to copy. It does not address two
prior questions:

1. **Will rgb-lib colour a second transition spending `F` while the trigger transition already
   consumes that Opout as Tentative?** Within one process the analogue is proven
   (`build_colored_receiver_state`, `build_colored_renewal` both do it over `T`'s payload output) and
   the stock is persisted — but no code path in the tree does it over `F`, and none does it in a
   **later process**. If `color_psbt` answers `available (0)`, the design below is not the design;
   `clients/libs/rust-rgb/src/lib.rs:877-894`'s [D3] note documents a lane where exactly that
   happened. **UNVERIFIED. Highest-value thing to measure in the whole programme.**
2. **Once mined, does the re-anchor beat the Tentative trigger in state selection?** Inferred yes
   from `WitnessOrd`'s derived ordering (Mined < Tentative < Ignored < Archived,
   `rgb-consensus-0.11.1-rc.10/src/vm/contract.rs:267-307`) plus the minimum-selection in
   `select_valid_witness` (`rgb-ops-0.11.1-rc.10/src/persistence/state.rs:117-139`). **Not run.**

And a third, structural: **the dead ladder cannot be retired.** The old tiers are permanently
unconfirmable and should be archived, but `update_witnesses` is exactly what silently destroyed a
coloured ladder in `CTESR-GATE.md:175-249` §2.3 E7 (`succeeded=2, failed={}`, no error, while
`get_asset_balance` kept reporting a full spendable balance) and is banned by
`scripts/ci/deny-rgb-witness-apis.sh:17-21`. The fork has the repair primitives
(`revalidate_offchain_bundles`, `force_witnesses`, `backup_invalid_bundles`);
**`clients/libs/rust-rgb/src/lib.rs` exposes none of them** — they appear in the repo only inside the
CI deny-script's text. So this needs new bridge surface and a deliberate rev bump of
`clients/libs/rust-rgb/Cargo.toml:30`.

**Verdict: CR-A is a 2–3 day experiment away from being costable, and CR-D is buildable now without
it. The programme should build CR-D and run G2 to decide whether CR-A's one-transaction form is also
available — not hold Phase 2 hostage to it.**

### 2.3 The four designs

**CR-A — cooperative coloured re-anchor (the assumed shape), *if* the spike passes.**
1-in / 2-out, `[opret, P2TR(fresh aggregate)]`, nVersion 2, **no** P2A — it is broadcast and does not
need to relay as a package, so `build_colored_tier` is not reusable (it hard-codes v3 + P2A +
locktime 0, `rgb.rs:1204-1223`). Transition assigns the whole allocation to the re-anchor's own
witness output — `RgbWallet::color` with an `output_map`, not `color_blinded`. Fee against the
**coloured** vsize (~155 vB, to be MEASURED through the production finaliser, not derived). Output
value must equal exactly what the fresh deposit was initialised for; `check_deposit` matches on exact
value and is index-agnostic (`clients/libs/rust/src/coin_status.rs:33-46`). SE cost: 5 blind signing
sessions (1 re-anchor + 1 `create_tx1` + 3 new coloured ladder tiers). Census on the new slot is the
ordinary coloured `4 = 1 + 3 + 0` (`tesr.rs:1733-1752`, `:1827`); the old slot retires WITHDRAWN and
its arithmetic dies with it. `create_backup_transactions` must keep running on the new coin
(`transfer_sender.rs:1283`) — `flat_backups = 0` makes `expected = 3` against a live `num_sigs` of 4
and kills every coloured coin at claim.

⚠ **Three hazards the original CR-A write-up did not name, all in the window between broadcast and
confirmation.** (1) Over that window `F` has **two live un-timelocked spenders**: the re-anchor, and
the trigger `T`, of which *every prior owner retains a co-signed copy* ([B1], `tesr.rs:8830-8840`).
A prior owner broadcasting `T` is a plain mempool race the current owner can lose. (2) The
re-anchor's co-sign is **irreversible and lands on the OLD slot**: if it never confirms, that slot's
`num_sigs` is permanently one ahead of anything it can disclose (`expected = flat_backups + tiers +
superseded`, `tesr.rs:9093-9098`), so the coin can never be conveyed again — and being coloured,
`refresh` refuses it too, leaving only a unilateral coloured exit. This is the failure class the repo
already records at `tesr.rs:1493`, `:1606`. (3) R2b's gate wants the old tiers "retired"; archiving
before confirmation, with `T` then winning the race, destroys the only valid witness chain for the
allocation — precisely `CTESR-GATE.md:175-249` E7. So CR-A additionally needs: build-and-broadcast
strictly before any archival, archival gated on N confirmations with a fail-closed re-read, a
pre-flight refusing the re-anchor when `F` already has a conflicting mempool spend, and an E2E that
**races a prior owner's `T` against the re-anchor and asserts the allocation is walkable in both
outcomes**. (CR-D removes this whole class, because there `T` *is* the `F`-spender.)
**Cost: 5–7 weeks. Best outcome; highest variance.**

**CR-D — coloured DE-TRIGGER (found by the adversarial pass; needs no G2 answer and no SE change).**
Two transactions, zero CSV wait. Broadcast the coin's own coloured trigger `T` — the canonical,
un-timelocked, *already coloured* consumer of `F`'s allocation, so **there is no RGB rival over `F`
at all** — wait one confirmation, then spend `T`'s payload output with a coloured, timelock-disabled
de-trigger paying a fresh deposit aggregate. The primitive is half-built: `cosign_detrigger`
(`tesr.rs:7367-7378`) is one blind co-sign of exactly that shape, and `build_detrigger`
(`lib/src/tesr.rs:576-593`) uses `TRIGGER_SEQUENCE` (`lib/src/tesr.rs:250` = `0xFFFFFFFD`, relative
lock disabled). Safety is the de-trigger's existing argument: an un-timelocked spend confirms before
any pre-signed extension can mature (`e_floor` = 144).

*The single refusal in the way is cosmetic.* `refuse_uncolored_over_colored(bundle,
"cosign_detrigger")` (`tesr.rs:7376`) exists solely because `build_detrigger` hard-codes
`UNCOLORED_PAYLOAD_VOUT`, which on a coloured trigger names the opret — its own comment says so. The
coloured builder is `build_colored_tier` with `sequence = TRIGGER_SEQUENCE` and role
`TierRole::Detrigger`, a tag **already allocated** at `0x06` (`rgb.rs:775`); `build_colored_tier`
takes the sequence as a plain parameter (`rgb.rs:1204-1223`), so this is a call site, not a fork.

*The rivalry it does create is the one already certified.* The de-trigger rivals the retained `X_m`
over `T`'s payload output — the exact outpoint class `build_colored_renewal` (`tesr.rs:2085-2176`)
and `build_colored_receiver_state` (`:1903`) already rival over in production. What remains unproven
is narrower than G2: mined-rival-beats-Tentative in `select_valid_witness`, and doing it in a *later
process*. That is G2's question 2 only, over an outpoint where question 1 is already answered yes.

*Costs and residual risks.* 2 txs (~336 vB coloured) vs CR-B's 4 / ~659; **0 blocks** of CSV wait vs
CR-B's 2 160; one on-chain confirmation of `T` before the de-trigger can be built. It inherits CR-B's
one real hazard — broadcasting `T` voids every flat backup (they all spend `F`) and races any prior
owner's retained `T'` — but incurs it once rather than across a 15-day walk. The de-trigger must pay
**exactly** the value the fresh deposit was initialised for; `check_deposit` matches on exact value
and is index-agnostic (`clients/libs/rust/src/coin_status.rs:33-46`). `cosign_detrigger` has **zero
callers anywhere in the tree**, so the driver is new work either way.
**Cost: 2.5–3 wk. Strictly dominates CR-B. Removes most of CR-C's motivation.**
**BUILT (D29):** `build_colored_detrigger` + `cosign_colored_detrigger` in
`clients/libs/rust/src/tesr.rs`, driven by `UtexoWallet::colored_reanchor`. The estimate above is
what it was costed at, not what it took; §4.2 records the actual.

**CR-B — exit-and-redeposit. Superseded by CR-D; retained for the record.** Walk the coloured ladder on chain (`T`, `X_m`,
`S_k`: 3 coloured tiers, 504 vB — this is sdk75, already proven, and `colored_exit_move` already
books the move, `tesr.rs:974-985`), then sweep `S_k`'s payload output into a fresh deposit aggregate
with a coloured builder. Requires **no new RGB rival behaviour whatsoever** — the ladder is genuinely
spent, not shadowed. Costs 4 transactions / ~659 vB against CR-A's 1 / ~155, and — the real price — a
full CSV wait of `e_m + d_k` = 2 160 blocks ≈ 15 days on the mainnet schedule before the coin is
usable again. The sweep still needs a coloured builder for a key that is **not in rgb-lib's BDK
descriptor** (every tier pays a Mercury seed-derived key), so it is not free. CR-D does the same job
in 2 transactions and 0 blocks; there is no axis on which this wins.
**Cost: 3–3.5 weeks. Dominated by CR-D — do not build it.**

**CR-C — build nothing; make the bound explicit and enforced.** Accept that a coloured coin is
bounded on **two** axes, not one: by the absolute flat-backup horizon `L0 = H_deposit + initlock`
(**10 000 blocks ≈ 69.4 days** on the deployed profile — `server/Settings.toml:2-3` reads
`lockheight_init = 10 000` / `lh_decrement = 100` against `network = "testnet"`, which D25 runs on
the mainnet schedule) **and by `max_exit_txs − 4` spine batches** — **19** there, 135 on regtest
(§1.6, `enforce_exit_chain_length` / `exit_cap_shapes`). The original draft priced only the first. Build the two things that convert a silent loss into a clean refusal: the sender-side
pre-flight (§3, R1) and a coloured near-deadline force-exit. Write **both** limitations into §10/§12
rather than around them.
**Cost: 1.5–2 weeks. Honest and cheap, but with the payment bound now stated it makes D1's "a real
ladder" claim thinner than the original draft implied — and CR-D is only ~1 week more.**

### 2.4 What renewal is *not*

`build_colored_renewal` (`tesr.rs:2085-2176`) is not a substitute and must not be presented as one.
It replaces `X_m` over the trigger's payload output off-chain at zero vB — it never touches `F` and
never touches the flat-backup chain. Three budgets, and renewal refills one:

| budget | size | refilled by renewal? |
|---|---|---|
| state rungs | `d0=1440, δ=36, d_floor=144` ⟹ 36 hops/epoch | yes |
| extension rungs | `e0=720, δE=36, m_max=15` ⟹ 15 renewals, then `needs_rollover` — and **coloured rollover does not exist** (`tesr.rs:7033`, `:1351-1362`, `:9172`) | partially, then terminal |
| **absolute clock** `L_k = H_deposit + initlock − k·interval` (`epoch_deadline_from_flat_backups`) | **10 000 blocks ≈ 69.4 days** deployed (`server/Settings.toml:2`; the 1 000-block ≈ 6.9-day figure this row used to carry was the regtest epoch, and it is not what ships) | **no — renewal cannot reach it at all** |

Only an on-chain spend of `F` changes an on-chain fact. Nothing calls `renew_colored_ladder`
automatically — its only non-library caller is
`clients/tests/rust/src/sdk74_colored_ladder.rs:519`. And the obvious-looking shortcut is **unsound**,
not merely out of scope: re-issuing a fresh `tx1` at `H_now + initlock` on the same slot would give
the current owner a *later* maturity than every previous owner's retained backup, inverting the
decrementing ordering the entire invalidation model rests on.

The cost of doing nothing is already MEASURED, not theoretical
(`clients/tests/rust/src/sdk32_token_over_time.rs:48-72`): with `initlock = 1000`, after 1 500 idle
blocks the sender's `transfer_tokens` **succeeds** and the receiver's `claim()` fails forever
(*"conveyed parent flat backup chain is invalid … the ancestor census term is unusable"*), because
`verify_conveyed_child` runs `validate_backup_chain_v2` over the conveyed parent chain
(`tesr.rs:5783-5796`) while `colored_in_ladder_pay` has no horizon check anywhere in its body. The
test discloses this in its SUCCESS line (`:472`) and was reordered to send inside the horizon
(`:288`). A KNOWN GAP disclosed in a passing test's success string is not a gate, and this lane is
now normative. The missing sender pre-flight is **lane-independent** — `in_ladder_pay` lacks it too —
but the irrecoverability is coloured-only, because `refresh` refuses carriers. Hence R1 runs
regardless of branch.

One more door that looked closed and was not: `cosign_detrigger` refuses coloured outright and has
no callers, so at the time of writing **a coloured coin had no cooperative primitive of any kind**.
That refusal was a hard-coded payload vout, not a design limit, and lifting it for the coloured lane
was the whole of CR-D (§2.3). **CR-D has since shipped** (D29): `build_colored_detrigger` and
`cosign_colored_detrigger` exist in `clients/libs/rust/src/tesr.rs`, and the SDK entry point is
`UtexoWallet::colored_reanchor` (`clients/libs/rust-sdk/src/refresh.rs`), which broadcasts the
coin's own trigger if it is not already in a mempool and then the coloured de-trigger. The plain
`reanchor` path now **dispatches** on colour instead of refusing every carrier it recognises: a
coloured carrier is told to call `colored_reanchor`, a plain carrier is told CR-D cannot help it.
So a coloured coin now has exactly one cooperative primitive, and a plain RGB carrier still has
none.

---

## 3. Dependency-ordered work plan

Gates are observable — a test that goes green, a measured number, a written verdict — never
"reviewed".

### Phase 0 — the two blockers, then the unknown reducers (2.5–4 wk; W1 runs in parallel throughout)

| # | Item | Gate | Band |
|---|---|---|---|
| **S0** ✅ | **DONE (D26 half 1).** The Σ + payload-count law is live in `verify_conveyed_child`'s ancestor loop and refuses by name — *"ancestor {i} state (the intermediate spine SP): Σ over its payload outputs is …"*. **BLOCKER — the intermediate-`SP` committed-fee law** (§1.8). Add the Σ + payload-count law to the ancestor `state` in `verify_child_bundle`, colour-branched per `tesr.rs:8866-8874`; fix the ancestor EXTENSION's colour-blind Σ law at `:9468` in the same commit | An adversarial test: a co-signed spine segment committing a 1-sat fee is refused **by name**, and `assert_not_an_unrelated_refusal` passes. A second test builds a coloured two-tier ancestor segment that verifies (today that shape is unreachable) | 4–6 d |
| **R0** ✅ | **DONE (D8-CLOSE).** `verify_sig_count_attestation` now has a client caller outside `lib/` — `clients/libs/rust/src/utils.rs`, whose comment names the attack the two-value disagreement closes — and `deny_unattested_terminality` pins the call site so it cannot be dropped. **BLOCKER-adjacent — wire the census attestation** (§5 item 1). `verify_sig_count_attestation` (`lib/src/transfer/receiver.rs`) was built and live-verified but had **no client caller**; the census still consumes a bare integer at `tesr.rs:9093`. Call it on the receive/claim path and in `verify_conveyed_child`'s ancestor pass; decide the `Option` policy (**fail-closed for coloured**); add the replay/freshness binding `DECISIONS.md:241` names | A test that a coordinator under-reporting `num_sigs` is refused at claim; a test that a missing attestation is refused when `colored_ladder = true`. **Hard prerequisite of the flag** | 2–4 d |
| **W1** ✅ **BUILT — acceptance run not yet observed** | **All four parts exist** — (a) `mercuryrustlib::core_rpc::submit_package`; (b)+(c) `mercurylib::wallet::p2a_fee_child::build_p2a_fee_child`, a v3 child spending the P2A outpoint; (d) **decided by D31** (owner-funded; the funded tower is a documented option) and built as the tower float rail (`clients/libs/rust/src/tower_float.rs`). W1's acceptance criterion is **written** as `live_p2a_package_rescue.rs`, which rescues an under-paying tier through repo code rather than a hand-run `bitcoin-cli` — but it needs `CORE_RPC_URL` and it **skips loudly** when the node's `minrelaytxfee` is too low to build an under-paying parent (*"Start bitcoind with a higher -minrelaytxfee to exercise the rescue. NOTHING was verified"*), so the rescue is **unrun on a raised-floor node**. Remainder is that E2E plus hardening. **BLOCKER — build the fee-bump remedy** (§1.5). Four parts: (a) a **package-capable node route** — electrum has no `submitpackage`, so this is a new transport (Core RPC or a proxy), not a flag; (b) a **P2A anchor spender** — no builder constructs a `TxIn` on a P2A outpoint today; (c) a **v3 fee child** — `lib/src/wallet/cpfp_tx.rs` is v2 and `input_vout`-pinned (`:66`, `:110`), so a rewrite; (d) an **owner decision on who funds the bump** for a keyless tower, under TRUC's one-child and 1000-vB limits | An under-paying v3 tier is rescued **through this repo's own code path**, on a node with `minrelayfee` above 2 sat/vB — not through a hand-run `bitcoin-cli`. (d) is design, and is the reason the band has a tail | **3–5 wk**, (d) unresolved |
| **G2** | **RGB rival-colouring spike over `F`** — now an **optimisation** gate, not a product gate. Colour a second transition spending `F` while the ladder's trigger consumes that Opout as Tentative, **in a second process**; mine it; check `select_valid_witness`; validate the new ladder's leaf consignment at a receiver | Pass ⟹ CR-A (1 tx). Fail ⟹ CR-D (2 txs, 0 CSV). Either way Phase 2 ships | 2–3 d |
| **G3** ✅ | **D7 profile reconciliation** | **DONE.** `for_network_checked` gives every network string an explicit profile with no silent default (D7/D25), and `g3_every_profile_can_finish_its_exit` now asserts the load-bearing half: `initlock ≥ tesr_exit_wait_blocks(profile, 0)` **and** `≥ depth 1`, for all six profiles. Below it, the horizon pre-flight refuses **every** in-ladder split on that profile, permanently and for every user — the state this item says the mainnet schedule was once in. **Both caps are now MEASURED, and they are two different quantities — an earlier note here reported them as a correction of one by the other, which they are not.** `max_split_depth` on mainnet at `initlock = 10 000` is **10** (regtest: 68) — the deepest TWO-TIER child the latency rule blesses, and exactly the number §1.6 derives. The cap on SPINE batches is the LENGTH cap built from it: `max_exit_txs = 3 + 2·10 = 23`, and a spine leaf's chain is `4 + d`, so **d ≤ 19** stands, pinned by `honest_spine_batch_pieces_and_tips_are_admitted_up_to_batch_nineteen` (*"batch 19 is the last one that fits"*; batch 20's piece is 24 transactions and is refused, while its tip at 23 keeps its exit so the sender is never stranded) | 3–5 d |
| **G4** ✅ | **Measure the coloured re-anchor / de-trigger vsize** | **DONE, and the answer is better than a new constant: there is no new constant.** `build_colored_detrigger` builds ONE payload output and sizes it with `colored_tier_out_value` = `colored_tier_out_total(.., 1, ..)`, so a de-trigger IS an ordinary one-payload coloured tier and rides the vsize already measured through the production finaliser: **168 vB**. The `112 + 43` estimate was **13 vB short** — at 2 sat/vB it commits 310 sat against a 168-vB transaction, 1.845 sat/vB, under target on the one transaction whose job is to relay when the owner must walk away from a live ladder. Pinned by `the_coloured_detrigger_is_a_measured_one_payload_tier_not_a_112_plus_43_estimate` | 1 d |

**G1 is deleted.** It was scoped as a spike, and the spike ran: `notes/WP1-TRUC-P2A-SPIKE.md`
(`d797bce`, `5475d79`) closed the literal-P2A-relay question and left the *implementation* open. Its
written verdict is already in hand; what it revealed is W1.

### Phase 1 — the spine (sequenced after S0 and G3; W1 runs alongside, not before)

| # | Item | Dep | Gate | Band |
|---|---|---|---|---|
| **S1** | `TierRole::Spine = 0x0C` appended; cap seal derivation. Tags run `0x01..0x0B` (`rgb.rs:770-780`), so `0x0C` is free | S0 | Round-trip test; tag census test still green | 0.5 d |
| **S2** | Coloured cap builder + `ColoredTip` population + journal tip variant (a `SplitJournalChild` shape holding a one-tier coloured leg + `PendingTier` for the cap) | S1 | The refusal at `tesr.rs:3965` is deleted and a unit test builds a coloured cap whose consignment contains its own witness | 2–3 d |
| **S3** | `change_leg_role(SplitLane::Colored) → SpineTip` — **same commit as S2** | S2 | A test asserts floor 906 and the one-tier builder together; a test asserts the two can never disagree | 0.5 d |
| **S4** ⚠️ **HALF** | `build_colored_spine_batch` / `cosign_colored_spine_batch` | S2 | **The coloured `SP` is built; everything BELOW it is not.** `spine_batch_split_ex`'s coloured branch still constructs every leg with the PLAIN `establish_child_journalled` / `establish_spine_tip_journalled` and persists each with `rgb: None` — an uncoloured tier over `SP.out[j]`, where the allocation lives after the batch, which BURNS it. `spine_batch_split_colored` has **zero callers**, so it was latent, not live. Now refused by name (`(true, true)` arm) with a self-retiring guard | 3–5 d |
| **S4b** ✅ | **The coloured spine batch's LEGS**: per-payee `ext_child`/`state_child` with consignments and seals rooted at `SP.out[j]`, a coloured cap for the next tip, allocation conservation, `rgb: Some(..)` on every persisted record, and the leaf-consignment pre-flight the root split already runs. Plus SDK dispatch (a coloured carrier that is a TIP) and the RGB bookkeeping for the spent tip outpoint | S4 | **DONE and VERIFIED LIVE.** Shared leg builder, `build_colored_spine_batch`, `cosign_colored_spine_batch`, the lane fork (blind to tips in **four** places — it routed coloured change to the PLAIN lane, which would have burned it), and the SDK driver `colored_spine_batch_pay`. The last blocker was in the RECEIVER, not the batch: `verify_child_bundle`'s ancestor conservation applied `tier_out_value` — the plain, single-payload formula — to a coloured multi-payload `SP`, refusing every honest batch-minted piece by 172 sat (the opret plus the second payload output). sdk77 now runs the second sequential payment and the depth-2 claim by default | ~S3-sized |
| **S5** | **N-deep coloured seal schedule — gates receiver adoption AND every re-transfer of every piece the spine mints.** N-deep `colored_child_txids` / `colored_child_seals`; resolve `consignments.len() == tiers.len()` for intermediate segments; enforce `m = 0` on spine segments. Touched sites include `build_colored_child_retransfer` (`tesr.rs:6363`), which calls `colored_child_seals` and therefore refuses every spine-minted piece until this lands (§1.4) | S4 | A receiver adopts and books a piece at spine depth 2; a depth-2 piece is **re-transferred**; a hand-crafted bundle with a mislabelled segment gets a **named** refusal and `assert_not_an_unrelated_refusal` passes | **2–3 wk** (was 6–9 d; re-banded because the sender-side re-transfer path is inside its blast radius) |
| **S6** | Crash-recovery replay for a coloured tip leg (`SplitJournalRecord::spine_tip`, `resume_in_ladder_split`) | S4 | A fault-injected crash between co-sign and conveyance resumes; a test asserts no phantom rival is ever co-signed over `SP.out[K]` | 3–5 d, low confidence |
| **S7** ✅ | Booking / health / watchtower / `auto_exit` head-start over an N-level chain | S5 | **DONE.** The trigger type already existed; the real defect was that `export_watch_bundle` emitted **no leaf entry at all** (a `ctesr-`/spine-tip row is neither `tesr-` nor `branch-`, so every leaf took the flat-deposit `continue` and a delegated tower silently watched none of them). A leaf now arms both predicates, with `head_start` over the **bound** N-level chain via the same `exit_wait_blocks` call `auto_exit_due` uses | 3 d+ |
| **S8** ⚠️ **HALF** | **The model reconciliation is DONE; the spine's transaction COUNT is not.** `tesr_exit_wait_blocks` now returns `csv_total + tesr_exit_txs(d)` — it charges the per-level parent confirmation, agrees with `exit_wait_blocks`'s `csv + 1` per tier, and carries the `[S8]` reasoning in place; `tesr_exit_csv_total` splits out the locks-only question. **Still open:** `tesr_exit_txs(d) = 3 + 2d` charges every level as two-tier, so a spine's real `4 + d` is over-counted (§1.6), plus 211 vB coloured, floors, and W1's per-level bump confirmation | W1, WP5 | One test derives txs, vBytes **and wait** from measured transactions at depths 0..3 under **both** models and fails if they disagree. Note this moves a receiver-side admission rule (`max_exit_txs`), so budget test churn | 4 d + churn |
| **S9** ✅ | Live-stack E2Es: a coloured carrier making two sequential payments; a receiver claiming at depth 2; a crash-recovery replay | S5–S7, **S4b** | Green on the live stack, on the **G3** profile. **All three scenarios covered.** Live on the stack (sdk77, ungated): a coloured carrier making two sequential payments, and a receiver claiming at depth 2 — a 6-tier chain (`T → X_0 → SP_0 → SP_1 → ext_child → state_child`) validating through the intermediate segment. The crash-recovery replay is `s9_colored_batch_replay_tests`: a `Signed` depth-2 batch record rebuilds its payee (depth 2, coloured, with the retired cap disclosed) and its next tip (one cap, its own allocation) from the record ALONE — no RGB engine, no SE — and the two reconstructors refuse each other's legs, so a replay cannot cross them | 5 d+ |

### Phase 2 — the re-anchor (CR-D is the baseline; G2 decides whether CR-A replaces it)

| # | Item | Gate | Band |
|---|---|---|---|
| **R1** | **Sender-side horizon pre-flight** in `colored_in_ladder_pay` *and* `in_ladder_pay` — fetch tip + parent flat chain, run the same `validate_backup_chain_v2` the receiver will run (`tesr.rs:5783-5796`), refuse **before the first co-sign** | sdk32's reorder at `:288` is undone; the post-horizon send **refuses on the sender** and the SUCCESS-line KNOWN GAP at `:472` is deleted. **Do this regardless of branch — lane-independent, and it is days** | 3–5 d |
| **R2d** | **CR-D (baseline, build regardless of G2): coloured de-trigger.** (i) builder = `build_colored_tier` over `(trigger.txid, trigger.payload_vout)` at `TRIGGER_SEQUENCE`, role `Detrigger` (tag `0x06` already allocated, `rgb.rs:775`), one payload paying the fresh deposit aggregate at exactly the value `check_deposit` expects (`coin_status.rs:33-46`); (ii) lift `refuse_uncolored_over_colored` at `tesr.rs:7376` **for the coloured lane only, in the same commit as the builder**; (iii) driver — broadcast `T`, wait 1 confirmation, broadcast the de-trigger, let the watcher claim the fresh deposit | Allocation survives across `T` + de-trigger + fresh ladder; a post-re-anchor conveyance claims at the far end; a test asserts **zero** CSV wait; a test asserts the de-trigger is refused if `T` is unconfirmed | **2.5–3 wk** |
| **R2a** | CR-A *only if G2 passes*: bridge surface (fork repair primitives exposed) + rev bump + coloured broadcast shape | `cargo build` from a clean clone at a tagged rev; a unit test archives a dead ladder without touching `update_witnesses` | 1–2 wk |
| **R2b** | CR-A *only if G2 passes*: builder + driver + measured fee + routing of `refresh` / `auto_refresh_due` with a fail-closed `is_colored()` gate read from the coin's own `tesr-` row. **Plus the three window hazards named in §2.3**: broadcast strictly before archival, archival gated on N confirmations with a fail-closed re-read, and a pre-flight refusing the re-anchor when `F` already has a conflicting mempool spend | sdk30's coloured sibling: allocation survives; a fresh coloured ladder establishes over `F'`; old tiers are unbroadcastable **and** retired; a post-re-anchor conveyance claims at the far end; **an E2E races a prior owner's `T` against the re-anchor and asserts the allocation is walkable in both outcomes** | 2–3 wk on top of R2d |
| ~~R2c~~ | ~~CR-B: coloured sweep-to-deposit after a full exit walk~~ | **Dropped — dominated by CR-D on transactions, latency and risk (§2.3)** | — |
| **R3** | Fix `build_colored_child_retransfer`'s message at `tesr.rs:6375`, which still offers a re-anchor a leaf **structurally cannot have** | The source-inspecting test at `:20063-20074` that banned that exact phrase from the plain sibling is extended to the coloured one | 0.5 d |

### Phase 3 — the rest of D1's table

| # | Item | Gate | Band |
|---|---|---|---|
| **P1** ✅ | WP6(i) receiver-side refusal: a conveyed transfer bearing both a plain ladder and an off-chain-commitment envelope is rejected (**was** sender-side only, `wallet.rs:866`; `transfer_receiver.rs:885`, `:967` persisted `rgb_consignment` with no cross-check) | **DONE, and corrected by D35.** `validate_encrypted_message` calls `crate::tesr::verify_flat_backup_lane` on every conveyed message that carries a ladder; a PLAIN ladder with an `rgb_consignment` on any flat row, or with an OP_RETURN in any flat backup, is refused **by name** (*"a plain ladder's tiers carry no RGB state transition, so exiting through them would move the sats and permanently BURN the allocation"*). D35 is the half this row did not anticipate: the first cut keyed on "a ladder is present" and enforced the **union** of the two lanes' shapes, which both missed the coloured lane's real defect (an unbound opret on a coloured coin's hop backup = capture, not griefing) and refused the coloured rescue lane at its last hop. The rule is now read off `is_colored()`, pinned by `d35_flat_backup_lane_tests` and by `ci-guards/tests/deny_colored_backup_on_a_colored_ladder.rs`, which asserts against **comment-stripped source**. Residual: the adversarial test is one level below this row's literal gate — it drives `verify_flat_backup_lane`, not `validate_encrypted_message` end to end | 2–3 d — **spent** |
| **P2** ✅ **shipped** | Per-output blinding in the rgb-lib fork + rev bump; lift `refuse_colored_multi_payee` | **DONE and CONSUMED.** rgb-lib `ae8439e` adds `AssetColoringInfo::output_blinding` (per-vout, falls back **per output** to `static_blinding`, then random); pushed, and `clients/libs/rust-rgb/Cargo.toml` is bumped `38e344e → ae8439e`. The bridge derives each vout's blinding as `H("utexo/p2/per-output-blinding/v1" ‖ base ‖ vout)` — distinct per output and REPRODUCIBLE across reruns, because `new_random_vout` would separate the legs while destroying the byte-for-byte rebuild the un-broadcast lane depends on. **The lift is deliberately NOT taken:** the refusal named seal privacy, and that gate is now closed, but `PARTIAL-PAYMENT-ECONOMICS.md` §4.7 lists others and one still loses money — the coloured lane conveys serially after the carrier is terminal and journals no `recipient_address`, so a failure at recipient *j* strands *j..K* permanently. The refusal now states THAT, and records that the privacy prerequisite is done so nobody re-solves it | 1–1.5 wk |
| **P3** ✅ | LN, D21 = build | **DONE.** The CTES-R lane is LN-latchable (latch created on the piece, after the split and before the conveyance; SE hash returned so an invoice can be made); the batch lane refuses a latched send **by name** rather than conveying an unlatched piece into a swap; the latch is journalled into `latch_batch_id` between creating it and conveying it (F7); and **the pre-pay gate proper is built**. It needed NO wire-format change — `child_tesr_bundle` was already conveyed, so the child's `colored_child_txids()` is derivable at the receiver; `PendingTransferInfo::child_witness_txids` now surfaces it and `validate_pending_token_ex` resolves the consignment against it instead of the flat lane's `branch_txs`, which a child does not have. The refusal now fires only when NEITHER chain is present | 1.5–2 wk |
| **P4** | JS/web/Kotlin. **Smaller than D1's table says on JS, larger on Kotlin.** The *product* SDK (`clients/libs/nodejs-utexo`) is a JSON-lines client over the Rust daemon and is unaffected (`clients/apps/web-wallet/server.js:22`); the fail-closed guards bite only the legacy `clients/libs/nodejs` (consumer: `clients/apps/nodejs/index.js:140`) and `clients/libs/web` (**no in-repo consumer found**). Kotlin's `FFITransferMsg` has no field for laddered material at all and refuses rather than truncates (`lib/src/unifii_interface.rs:55-81`) | **Owner answered — D33 (`DECISIONS.md`): `clients/libs/web` has no external consumer; leave it fail-closed.** The unbounded tail is closed and the item is 0 | **0** (was 0, 2–3 wk, or unbounded) |
| **P5** ✅ | Inventory the two capabilities the flip silently removes (coloured multi-carrier combine, coloured leaf consolidation) into D3's `V_min` work | **DONE — D32** records both, names `V_min` as what they feed, and notes the migration hatch bounds the damage without restoring either. The gate was "they appear in DECISIONS.md before §10 is drafted"; they do | 1 d |

---

## 4. Effort estimate — the number that un-withdraws the roadmap

**Engineering to make `colored_ladder = true` defensible: 19–27 engineer-weeks for one engineer, or
12–17 calendar weeks with two working the independent tracks.**

⚠ **This moved from the original 14–20 as a direct result of the adversarial pass.** Three of the four
deltas are work the original plan contained no line item for at all.

| Block | Range | Δ vs original | Confidence |
|---|---|---|---|
| Phase 0 blockers: **S0** (intermediate-`SP` fee law) + **R0** (census attestation wiring) | 1–2 wk | **+1–2 wk (new)** | High — both are named code sites |
| Phase 0 **W1** (package route + anchor spender + v3 fee child + who funds the bump) | **3–5 wk** | **+3–5 wk (was costed at zero as "G1, a spike")** | **Low — (d) is unresolved design** |
| Phase 0 remaining gates (G2, G3, G4) | 0.5–1 wk | −0.5 (G1 deleted) | High |
| Phase 1 spine (S1–S9) | 7–11 wk | +1–2 (S5 re-banded 6–9 d → 2–3 wk; S8 2 d → 4 d) | Medium |
| Phase 2 re-anchor: **CR-D** 2.5–3 baseline, **+CR-A** 3–5 more if G2 passes (+ R1, R3) | 3–8 wk | Narrower floor, same ceiling — CR-B dropped, CR-C no longer the fallback | Medium — CR-D needs no G2 answer |
| Phase 3 P1 + P2 + P5 | 2–3 wk | — | Medium-high |
| Phase 3 P3 (LN) | 0 or 1.5–2 wk | — | Blocked on D21 |
| Phase 3 P4 (clients) | 0, 2–3 wk, or **unbounded** | — | See below |

Feeding back into the roadmap: the 12–16 weeks was a **specification** deliverable, and Phases 0–3
overlap its drafting rather than preceding it. The spine and re-anchor gate §5, §6, §10 and §11
specifically. **Re-cost: 20–27 weeks to a full protocol v1 draft, +2–3 to publication.** The single
largest driver of the increase is W1, which is not overlappable with drafting — until it exists, the
protocol cannot state that a coin is unilaterally exitable above 2 sat/vB, and §6 and §15 cannot be
written. If G2 fails and only CR-D ships, the total lands near the low end; if W1's funding question
turns out to need a protocol change (a prepaid tower bond, `PROTOCOL.md:525`), it lands past the high
end.

### 4.1 RE-COST 2026-08-11 — measured against the tree, not against the plan

Every line below was re-checked in code. The headline moves from **19–27 engineer-weeks to 11–21**,
and the reason is not optimism: two whole blocks were built and verified, and a third turned out to
be resting on scaffolding that landed after this document was written.

| Block | Was | **Now** | Evidence |
|---|---|---|---|
| **S0** intermediate-`SP` fee law | part of 1–2 wk | **0 — DONE** | Σ-payload law in `verify_conveyed_child`'s ancestor loop; the starving-slot attack test refuses by name (D26 half 1) |
| **R0** census attestation wiring | part of 1–2 wk | **0 — DONE** | `verify_sig_count_attestation` wired both sides, coordinator deployed, verified live at `sig_count=2` (D8-CLOSE) |
| **W1** fee-bump remedy | **3–5 wk** | **0.5–1 wk** | (a) `core_rpc::submit_package` exists; (b)+(c) `p2a_fee_child` builds the v3 anchor-spending child; (d) **decided** (D31) *and* built — the tower float rail, with its capacity bound measured (`live_tower_float.rs`). The acceptance criterion — a tier under the floor rescued through repo code — is **written as `live_p2a_package_rescue.rs` and not yet observed green**: it skips, printing *"NOTHING was verified"*, unless the node runs a raised `-minrelaytxfee`. Remainder is that E2E + hardening |
| Gates G2/G3/G4 | 0.5–1 wk | **G2 only, 2–3 d** | G3 and G4 have since landed with their own pinning tests (`g3_every_profile_can_finish_its_exit`, `the_coloured_detrigger_is_a_measured_one_payload_tier_not_a_112_plus_43_estimate`), so what is left of this row is G2 at §3's own band of 2–3 d — and G2 is now an optimisation question (D29), not a product selector. *(The `0.5–1 wk` in the "Now" column was the three gates together; it is kept in the "Was" column as the baseline.)* |
| **S1–S9** coloured spine | 7–11 wk | **5–8 wk** | **The plain spine landed in the meantime (CATS-B).** `spine_batch_split`, `persist_spine_tip`/`load_spine_tip`, `establish_spine_tip_journalled`, `watch_spine_tip_pass`, `exit_spine_tip_pass`, `spine_segment`, `colored_spine_tip_floor` and a large adversarial test body all exist. *(This row used to read "the COLOURED half is still gated — `TierRole::Spine` is unallocated". That is false at HEAD.)* **The coloured half has largely landed too:** `TierRole::Spine => 0x0C` is allocated (S1), the coloured cap builder and journal tip variant exist (S2/S3), and `build_colored_spine_batch` / `cosign_colored_spine_batch` / `colored_spine_batch_pay` ship with a CI guard against uncoloured legs under a coloured `SP` (S4/S4b) — with S7 and S9 marked done in §3. **What remains of the spine is S5, S6 and S8's open half** — on §3's own bands that is S5 2–3 wk (10–15 d) + S6 3–5 d + S8's open half within its 4 d + churn = **17–24 d, i.e. ≈3.5–5 wk** (an earlier line here totalled the same three rows at "≈3–4.5 wk"; the bands did not change, the addition did) — and **S6 is materially de-risked**: the journal and replay this document called "the least grounded number" are built and tested on the plain lane |
| **Phase 2** re-anchor | 3–8 wk | **0 — CR-D DONE** | D29 selected CR-D and it has since landed: `build_colored_detrigger` + `cosign_colored_detrigger` (`clients/libs/rust/src/tesr.rs`), driven by `UtexoWallet::colored_reanchor`, with the plain `reanchor` path now dispatching on colour instead of refusing every carrier. *(The "no builder exists — `colored_detrigger`: 0 hits" line this row used to carry was true when §4.1 was written and is false at HEAD.)* `+3–5` for CR-A only if G2 is pursued and passes |
| Phase 3 P1 + P2 + P5 *(this row read `2–3 wk / unchanged, unstarted`)* | 2–3 wk for the three | **0–1 d — all three have landed** | **All three are done, and the "unstarted" this row carried was stale for every one of them.** P2: rgb-lib `ae8439e` adds per-output blinding and `clients/libs/rust-rgb/Cargo.toml` is bumped to it — the fork half is consumed, and the `refuse_colored_multi_payee` lift is deliberately not taken for a *different* reason (§3). P5: **D32** records both silently-removed capabilities against `V_min`. P1: `validate_encrypted_message` now calls `verify_flat_backup_lane`, refusing a plain ladder that carries RGB material by name (D35, §3) — the residual is a call-path test rather than the check itself, hence 0–1 d, not 2–3 |
| Phase 3 P3 (LN) | 0 or 1.5–2 wk | **0 — DONE** | **D21 is a recorded decision — "RGB over Lightning: BUILD IT" (`DECISIONS.md`, owner, 2026-08-11).** *(This row previously read "still blocked on D21, which is still not a recorded decision"; that was true when written and is false at HEAD.)* The build followed — see §3's P3 row |
| Phase 3 P4 (clients) | 0 / 2–3 wk / unbounded | **0 — closed** | **D33: `clients/libs/web` has no external consumer; leave it fail-closed.** The open owner question that made this unbounded is answered. `wasm/Cargo.toml` still depends on `mercurylib` only while `verify_bundle` lives in `mercuryrustlib` — the port is still not takeable, it is simply no longer required |

**Total: 11–21 engineer-weeks single-threaded, ≈7–13 calendar weeks with two tracks.**

⚠️ **That total is already a superseded baseline.** Since §4.1 was written, CR-D, P1, P2, P3, P5,
G3, G4 and the S1–S4b/S7/S9 stretch of the spine have all landed, W1's four parts were built, and P4
went to zero on D33. Summing only the rows still open, on these same bands, in days so the addition
is checkable:

| still open | band | days |
|---|---|---|
| S5 N-deep seal walk | 2–3 wk | 10–15 |
| S6 crash replay | 3–5 d | 3–5 |
| S8's open half (the spine's `4 + d` transaction count) | 4 d + churn | 4 (carried at the full band; the reconciled half is spent, so 4 is an upper bound at both ends) |
| G2 rival-colouring spike | 2–3 d | 2–3 |
| W1 remainder — hardening + the raised-floor E2E | 0.5–1 wk | 2.5–5 |
| P1's residual — a refusal test driving `validate_encrypted_message` itself | — | 0–1 |
| **total** | | **21.5–33 d = ≈4.5–6.5 engineer-weeks** |

*(An earlier line here read "**≈6–9 engineer-weeks**" over an itemisation of G2 + the spine + P1 that
adds to 3.9–6.1 wk. The gap was this table's `P1+P2+P5 | 2–3 wk | unchanged, unstarted` row, left
unstruck: the total counted all three, the itemisation counted only P1 — and in fact none of the
three was still open. Both figures are now derived from the same rows: the itemisation gains W1's
remainder, which it had silently dropped, and the total loses P2, P5 and P1's build.)* §4.2 restates
the open build items in the unit the work is actually done in; the W1 E2E is bounded by a node
configuration rather than by typing, so it carries no session row.

### What this does to the spec date

The old figure was **20–27 weeks to a v1 draft**, and this document named the reason plainly: *"the
single largest driver of the increase is W1, which is not overlappable with drafting — until it
exists, the protocol cannot state that a coin is unilaterally exitable above 2 sat/vB, and §6 and §15
cannot be written."*

**W1 exists.** §6 and §15 are writable now, and the fee-spike limit they must state is settled and
normative (D31: the owner funds it, a keyless tower cannot, and the spec says so). Drafting therefore
overlaps the remaining build instead of queueing behind it.

**Re-cost: 12–22 weeks to a full protocol v1 draft, +2–3 to publication.**

The remaining gates on the *document* are §5/§6/§10/§11, which need the coloured spine and CR-D — so
the draft date is now governed by S1–S9, not by an unresolved design question. **CR-D has since
shipped, so Phase 2 is off the critical path entirely**, and the draft date is governed by what is
left of the spine. The largest single item is **S5** (2–3 wk, unchanged: the receiver's N-deep seal
walk, which is also a sender-side gate). ~~The largest *unbounded* one is **P4**, and it is
unbounded on an owner question — does `clients/libs/web` have an out-of-repo consumer?~~ **D33
answered that: no external consumer, leave it fail-closed. There is no unbounded item left.**

### The three things that would move this number again

1. **S5 needing the `consignments.len() == tiers.len()` invariant to change shape.** Costed at the
   top of its band already; if it also forces a wire-format revision the band moves.
2. ~~**P4 turning out to have an out-of-repo web consumer.**~~ **Closed by D33 — there is none.**
   The unbounded tail this list existed to flag is gone.
3. ~~**D21.**~~ **Recorded — D21: build it.** P3 is no longer a coin-flip in the middle of the
   estimate; it is built (§3).

So of the three things that could have moved this number, **two are closed and only S5 remains.**

### 4.2 RE-ESTIMATE IN AGENT SESSIONS (2026-08-11) — and where the compression stops

Engineer-weeks are the wrong unit for how this work is actually being done. Re-expressed in **agent
sessions** (Claude Code, ultracode, Opus 5 Fast), calibrated on **measured throughput from the
session that produced this section** rather than on a conversion factor.

**The calibration point.** One session closed: S0, R0, all four parts of W1 (package route, P2A
spender, v3 fee child, tower float rail, plus the rescue's acceptance test — written, not yet run on
a raised-floor node, §0.1), CR-D + its wiring, P1, P5, S1, S2, audit Tiers 1–3, RGB
Stage 0, the build-infra fix, a coordinator deploy, and seven decisions (D26–D33). Against this
document's own estimates that is **≈12 engineer-weeks in one session**, at 644 green tests and clippy
clean throughout.

That is the honest number, and it comes with an honest caveat: **most of those items had scaffolding
that already existed** — W1's `build_colored_tier`, CR-D's whole spec type, S2's seal machinery. The
compression is real but it is partly a selection effect, because the items reached first were the
ones whose foundations were already in the tree.

| Remaining block | Engineer-weeks | **Agent sessions** | What sets the floor |
|---|---|---|---|
| S4 / S4b coloured spine batch | 3–5 d | **DONE** | Builders, SDK driver and the uncoloured-legs CI guard all landed |
| S3 (rode with S4) | 0.5 d | **DONE** | One line, gated by its own guard test |
| **S5** N-deep seal walk | 2–3 wk | **1–2** | The wire format and an exact-equality census; the item this document says "has the smell" |
| S6 crash replay | 3–5 d | **~0.5** | Plain-lane journal exists and is tested |
| S7 booking | — | **DONE** | The leaf now arms both predicates (§3) |
| S8 latency reconcile | ~1 wk | **~0.5** | Model halves agree; the open half is the spine's `4 + d` transaction count. Arithmetic and test churn |
| **S9 live E2Es** | 5 d+ | **DONE** | Green on the live stack (sdk77, ungated) — this was the row that "does not compress", and it is spent |
| **CR-D** | — | **DONE** | Landed this session |
| **P2** per-output blinding | 1–1.5 wk | **DONE** | Fork bumped; the lift deliberately not taken (§3) |
| **P3** LN (**D21: build**) | 1.5–2 wk | **DONE** | SSP pre-pay RGB gate + F7 journal |
| **P4** clients (**D33: none**) | — | **0** | Closed at zero |
| P1 receiver-side WP6(i) refusal | 2–3 d | **DONE** | `verify_flat_backup_lane`, called from `validate_encrypted_message`, corrected by D35 and pinned against comment-stripped source (§3) |
| G2 | 2–3 d | **~0.5** | ⚠ Measurement against the live stack. G3/G4 are **DONE** |

**Total: ~2.5–3.5 agent sessions of work still open** — S5 (1–2) + S6 (~0.5) + S8's remaining half
(~0.5) + G2 (~0.5) = **2.5–3.5**, the sum of this table's own surviving rows.
*(An earlier line printed the same "~2.5–3.5" over a table that still carried **P1 at ~0.5**, i.e.
over rows summing to 3.0–4.0 — the number was right and its arithmetic was not. What closes the gap
is that P1 has landed (D35, §3), so the row leaves the sum rather than the sum being rounded down.
And the "~6–9 agent sessions" this table originally totalled was the figure before S4/S4b, S7, S9,
P2, P3, G3 and G4 landed; that one is kept as the baseline the compression is measured against.)*
Not in this table: **W1's remaining hardening and its raised-floor E2E**, which
§4.1 books at 0.5–1 engineer-week — the E2E is bounded by starting a node with a raised
`-minrelaytxfee`, not by typing (§0.1). The non-compressing kind still in front: **G2**, bounded by
regtest cycles, and that same W1 run.

**Spec draft: ~2–4 sessions after the spine lands**, since §5/§10/§11 are the only gated sections and
§6/§15 became writable when W1 shipped.

### What does NOT compress, and why it is worth stating

1. **Live-stack E2Es.** A regtest lifecycle takes the wall-clock it takes; mining blocks and waiting
   for confirmations is not parallelisable by thinking faster. S9 and G2–G4 were bounded by this;
   S9, G3 and G4 are spent, and **G2 is the one that remains.**
2. **The external fork — and this one is a SPENT cost, not a forward-looking one.** P2 lived in
   `utexo-rgb-lib`, which has its own build, its own tests and a rev-bump that has to propagate. It
   was paid: rgb-lib `ae8439e` carries per-output blinding and `clients/libs/rust-rgb/Cargo.toml` is
   pinned to that rev (§3, P2). The shape is worth keeping because it recurs — no open item in §3
   names a fork change, but if S5's `consignments.len() == tiers.len()` resolution turns out to need
   one, this cost comes back and it is not compressible by thinking faster.
3. **Owner decisions.** Zero engineering time and unbounded calendar time. D21 and D33 were both
   answered on the day they were asked and each removed ~1 session of ambiguity; the three open
   questions in §6 have the same shape.
4. **Anything where the failure mode is silent.** S5 is the example: its cost is not typing, it is
   the adversarial tests that prove a mislabelled segment is refused BY NAME. This session spent more
   turns proving CR-D and S2 correct than writing them, and that ratio is the right one.

### Which items can blow up, and why

**S5 — the receiver's N-deep seal/witness walk. This has the smell.** It touches a conveyed wire
format whose own doc-comment warns that a defaulted field is downgrade surface
(`tesr.rs:1263-1266`); every check on that path is an exact-equality census where a miscount means
*"no receiver can adopt any piece of the batch"*; it must resolve a shipped invariant
(`consignments.len() == tiers.len()`, `:9183-9190`) that nobody wrote with an N-segment chain in
mind; and it is a **sender-side** gate too, since `build_colored_child_retransfer` refuses every
spine-minted piece until it lands (`:6363`, §1.4). Re-banded 2–3 weeks, and three is the realistic
number if the `consignments.len()` invariant has to change shape.

**S6 — crash-recovery replay. This also has the smell.** Crash-safety code where a wrong replay
co-signs a phantom rival over `SP.out[K]` — the exact failure `SplitLegRole`'s doc-comment exists to
prevent (`tesr.rs:3134-3146`) — and "only ever after a crash" is precisely where this repo's
silent-degradation defects live. `resume_in_ladder_split` was not read in full; **3–5 days is the
least grounded number in this document.**

**W1 — was the largest unbanded item in the plan, and it contained an open design question. Both
are closed.** Who funds a bump, from which UTXO, under TRUC's one-child and 1000-vB limits, and what
stops a third party pinning an anyone-can-spend anchor, was open design nobody had started
(`SPEC-ROADMAP.md:161-175`, `notes/WP1-TRUC-P2A-SPIKE.md:109-113`). Parts (a)–(c) were ordinary
engineering against named sites and are built; (d) was answered by **D31** — owner-funded, with the
funded tower a documented deployment option, and the spec now says plainly that a keyless tower
cannot bump. **The lesson survives the item:** treating this as a spike — which the original plan
did — is what put a zero next to the thing §0 calls a blocker, and it is the failure mode to watch
for on S5.

**G2 — a fail no longer changes the product.** With CR-D on the table a G2 failure costs one extra
transaction and one confirmation, not 4× bytes and 15 days (CR-B) or a bounded-life coin (CR-C). This
is the single largest de-risking the adversarial pass produced.

**P4 — the client port has an unbounded tail.** Porting `verify_bundle` is a crate **move**, not a
rewrite (~860 lines: `verify_bundle_ex` 445, `verify_colored_shape` 77, `verify_bundle_bound` 64,
`verify_conveyed_child` 271), and it is genuinely pure — but it lives in `mercuryrustlib`, which
pulls electrum/reqwest/sqlx/tokio/rgb-lib, while the wasm crate depends only on `mercurylib`
(`wasm/Cargo.toml:20`). Even a *completed* verifier port leaves a JS client able to verify the ladder
and unable to book the allocation, because `accept_colored_ladder` / `accept_colored_child_bundle`
(`tokens.rs:2061`, `:2195`) go through rgb-lib.

### Named unknowns

* **UNVERIFIED** — whether rgb-lib will colour a rival transition over `F` at all (G2). **CR-A hangs
  on it; Phase 2 no longer does** — CR-D creates no rival over `F`. What CR-D still needs, and is
  also unverified, is narrower: that a MINED rival beats a Tentative one in `select_valid_witness`
  over `T`'s payload output, in a later process. Question 1 over that outpoint is already answered
  yes by `build_colored_renewal` (`tesr.rs:2085-2176`) in production.
* **UNVERIFIED** — whether the old ladder needs explicit archival, or a confirmed rival suffices.
  Decides whether R2a (new bridge surface + rev bump) is required or optional.
* **UNVERIFIED** — whether the `level` dimension in `colored_tier_seal` (`tesr.rs:1419`) is sound for
  depth > 1 or is a placeholder never exercised. Both coloured seal accessors hard-refuse above one
  level (`:1355-1366`, `:467-478`), so it has zero coverage. This is the largest unknown behind S5.
* **UNVERIFIED** — consignment **size** growth across a deep spine. RGB transfer consignments carry
  history to genesis and every level adds a bundle; no size bound was found anywhere in the receive
  path and no measurement exists. Could bite near d = 19.
* **UNVERIFIED** — whether rgb-lib builds to `wasm32` at all. Not attempted; the pinned fork carries a
  bdk/sqlite/filesystem stack. If it does not, the honest design for P4 is a daemon/remote-verifier
  split, not a port, and that is a different project.
* **UNVERIFIED** — whether an sdk32 loss is truly unrecoverable or merely unsupported. `claim()`
  refuses, so `adopt_child_bundle` never persists — but whether the mailbox message (which carries
  the full bundle, and whose `child_state` pays the receiver's own exit address) is retained
  server-side such that a manual persist could still let the receiver walk the exit was not
  established. "No supported recovery" and "value destroyed" are materially different severities.
  Ten minutes of work; not spent.
* **UNVERIFIED** — whether the plain in-ladder lane's identical missing horizon pre-flight has ever
  fired in practice. The code path is the same for both lanes; if it has, R1 is independently urgent.
* ~~**UNVERIFIED** — the watchtower story for a coloured spine.~~ **VERIFIED, and it was worse than
  §4.7 said.** `watchtower.rs` has since been read. The trigger type (`WatchTrigger`), the outpoint
  predicate and the `Blind` state all existed already — but `export_watch_bundle` emitted **no leaf
  entry at all**, because a `ctesr-`/spine-tip row is neither the `tesr-` row nor the `branch-` row
  it looks for, so every leaf took the flat-deposit `continue`. Expressing the trigger was never the
  binding constraint; nothing was reaching it. Fixed under S7 — see §4.7.
* **UNVERIFIED** — whether `verify_bundle_ex`'s 445 lines are genuinely free of `mercuryrustlib`-only
  dependencies beyond the bitcoin re-export. Its `use` line pulls only
  `electrum_client::bitcoin::{…}` and its doc asserts purity, but the body was not audited. Budget a
  day to find out rather than assuming the crate move is mechanical.

---

## 5. SE / lockbox — owner decisions, not silent workarounds

**Nothing in the coloured spine or the coloured re-anchor requires an `enclave/` or `lockbox/`
change.** Colouring adds **zero** SE co-signatures — a coloured tier is still one input, one sighash,
one `cosign_tier` (`tesr.rs:1557-1559`, `:2682-2684`) — so a coloured spine batch costs the same
`1 + 2K + 1` co-signs as the plain one and the census arithmetic is byte-for-byte identical. The
coloured re-anchor is one blind partial signature over an ordinary taproot key-spend sighash:
`get_partial_sig_request_for_colored_tx` (`lib/src/transaction.rs:528-565`) differs from the plain
path only in which bytes it hashes, and
`grep -rn 'rgb|consignment|colored|opret|OP_RETURN' server/src lockbox/src` returns nothing at all.
**The SE stays RGB-blind throughout, and that must remain a hard invariant of any implementation.**

Three places where an SE-shaped answer bears on this work. **Both of the first two were stated wrong
in the original draft and are corrected here.**

1. **The census attestation is BUILT, live-verified — and now WIRED. This was R0, not an owner
   decision.** The original draft called it an unbuilt owner decision; both halves were wrong.
   Commit `f7b7aac` ("D8-CLOSE verified END TO END against the running enclave") landed
   `verify_sig_count_attestation` (`mercurylib::transfer::receiver`), the lockbox signs
   `sha256("utexo/sig_count/v1" ‖ statechain_id ‖ u32_be(num_sigs) ‖ nonce32)`, the coordinator
   forwards `attestation` / `attestation_pubkey` verbatim
   (`server/src/endpoints/transfer_receiver.rs:107-108`, `:128-129`), and
   `lib/tests/live_sig_count_attestation.rs` pins a captured live signature that **verifies at
   count=2 and fails at count=1**. The gap this item named was that `grep -rn
   verify_sig_count_attestation` found **no caller outside `lib/`**, both fields being
   `Option<String>` whose doc-comment defers the accept/reject policy to *"a verifier"*, while the
   census still consumed the bare integer — so a coordinator under-reporting by *k* hid *k*
   co-signed rival states and the exact-equality census still balanced. **That is closed.** The
   client calls it from `clients/libs/rust/src/utils.rs`, where the comment states the attack the
   two-value comparison refuses, and `ci-guards/tests/deny_unattested_terminality.rs` pins the call
   site so a later edit cannot quietly drop it. **Was 2–4 days of plumbing; done, and no longer a
   prerequisite standing between the flag and the flip.**
2. **The absolute clock is NOT replaceable by an SE attestation. This was a false lead and is
   withdrawn.** The original draft suggested that resting the census on SE-attested per-slot state
   would stop the absolute clock binding and make CR-A/B/C optional. It would not. The clock is not a
   census artefact — it is the **nLockTime of flat backup transactions that are already fully
   co-signed and already in previous owners' hands**, prevout-pinned to `(F.txid, F.vout)`.
   `epoch_deadline_from_flat_backups` reads that locktime straight off the signed transaction, and
   its own doc says why nothing moves it: *"each backup's own nLockTime IS its rung … The locktime is
   inside the signed transaction, so moving it invalidates the signature"* (`tesr.rs:5590-5600`). No
   key is needed to broadcast a transaction that is already signed and distributed, so no SE change
   revokes one. The safety property is that the current owner's backup matures **before** every
   retained one — which is exactly why §2.4 calls re-issuing a fresh `tx1` at `H_now + initlock`
   unsound. **Only an on-chain spend of `F` resets the clock**, i.e. CR-A or CR-D. This also
   contradicts open question 1's suggestion that D22 might make the re-anchor optional: it does not.
   The option D22 *does* open here — retiring the flat-backup chain for CTES-R coins entirely and
   resting the census on the now-built attestation — is a **census-model change, not a re-anchor**,
   collides with `PARENT_V2_BASELINE >= 1` at four enforcement sites (`tesr.rs:2791`, `:4357`,
   `:4755`, and the constant at `:5316`) and with CR-A's own requirement that
   `create_backup_transactions` keep running, and would need its own soundness pass. Not costed here.
3. **Per-output blinding requires an rgb-lib fork change** (`rust_only.rs:19-20`, `:480-484`,
   `:571-576`), i.e. a deliberate bump of the pinned rev at `clients/libs/rust-rgb/Cargo.toml:30`.
   Not SE work, but a dependency-pin decision with the same "who owns this" character, and it must be
   an explicit choice rather than a side effect of a build.

---

## 6. Open questions for the owner

1. ~~**Who funds a CPFP fee child for a keyless watchtower?**~~ **ANSWERED 2026-08-11 — D31.**
   The owner chose **(A) owner-funded**, with **(B) the funded tower as a documented deployment
   option**; (C) third-party service and (D) raising `committed_fee_rate` were not taken. W1(d) is
   therefore no longer undesigned, and **the 5-week tail on this estimate closes** — W1's remaining
   parts (a) package route, (b) P2A spender, (c) v3 fee child are all engineering.
   The product claim is now stated rather than assumed: the protocol does **not** promise
   spike-time rescue, a keyless tower **cannot** fee-bump, and during a spike the defence is the
   owner being online. Written normatively into `PROTOCOL.md` §5.13 and `TRUST-MODEL.md` B4.
2. ~~**Confirm CR-D as the Phase-2 baseline.**~~ **ANSWERED 2026-08-11 — D29**, and built. G2 no
   longer selects the product (§2.2/§2.3): CR-D builds without it, in 2 transactions and zero CSV
   wait, and it has since landed (`build_colored_detrigger` / `cosign_colored_detrigger` /
   `colored_reanchor`). The only question left is whether to spend the 2–3 days on G2 at all, to
   find out whether CR-A's one-transaction form is *additionally* available. Recommended: yes, and
   now genuinely optional.
3. ~~**Does `clients/libs/web` have an out-of-repo consumer?**~~ **ANSWERED 2026-08-11 — D33: no.**
   Leave it fail-closed. No in-repo consumer was found either (the `web/` React app's
   `package.json` carries only react/react-dom/cra-template). P4's unbounded tail is closed and the
   item is costed at zero.
4. ~~**D21 — RGB over Lightning: build it or exclude it by name?**~~ **ANSWERED 2026-08-11 — D21:
   BUILD IT**, and P3 has since shipped (§3). The 0-or-2-week coin-flip sitting in the middle of the
   estimate is resolved.
5. **Does the coloured leaf get renewal, or is exit-only within one epoch normative?** A received
   coloured piece can never be split, renewed, combined or re-anchored (§1.7). The spine does not fix
   this. Either coloured leaf renewal is built (a separate ~2-week unit) or §10 states the
   limitation.
6. **Are the two silently-removed capabilities** — coloured multi-carrier combine and coloured leaf
   consolidation (§1.7) — **accepted losses?** They are not in D1's table and D3's `V_min` work will
   hit them. **Inventoried by D32**, which records both, names `V_min` as what they feed, and
   requires them in `DECISIONS.md` before §10 is drafted — so P5's gate is met. The *acceptance*
   is still an owner call; what is closed is that a reader can no longer discover the losses by
   accident.
7. **Is a coloured spine's zero-bit concealment an accepted loss?** A depth-`d` payee can derive the
   sender's change-leg allocation at every level (§1.7). Today's K=1 leak is accepted at
   `tokens.rs:208-218` for the one-level case only; the spine makes it transitive over the carrier's
   whole payment history. The "or does P2 move in front of S4" half is settled by events: **P2
   shipped** (§3) and the fork now derives a distinct blinding per output. It does not close this
   question — every per-output blinding is still a pure function of material the payee holds, so
   derivation, not attack, remains the leak — but the ordering question is gone.

*(The original question 7 — "strike `DECISIONS.md:104`" — is withdrawn: that row was already struck,
`~~rgb-lib is an uncommitted filesystem path~~ | CLOSED 2026-08-11 (6b2e662)`. The consequence stated
in the original §0 still holds: the per-output blinding item (§3, P2) is a deliberate rev bump of
`clients/libs/rust-rgb/Cargo.toml:30`, not a build-reproducibility fix.)*
