# Coloured spine and coloured on-chain re-anchor — scope, design and re-cost

**Read-only investigation, `feat/spark`, 2026-08-11. No code written.** Every claim cites source.
Unknowns are marked **UNVERIFIED** and are not smoothed over.

> ### ⚠ PROVENANCE — how much scrutiny this document actually had
>
> **The adversarial review did not run.** This was produced as three parallel investigations → a
> design → **two critiques (a skeptical-implementer lens and a soundness lens) → this write-up**. Both
> critique agents died on API errors mid-response. So §§1–4 carry the investigation's evidence and the
> designer's reasoning, but **nothing attacked them**. On this project the adversarial pass has
> repeatedly been the step that found the real defect — the D13 "simple port" turned out to be a theft
> path only because a probe went looking. Treat the mechanisms here as *proposed*, not *vetted*, and
> re-run the critique before any of it is built.
>
> **It also worked under a scope rule that has since been revoked.** The agents were briefed "never
> propose changes to `enclave/` or `lockbox/`", which was true when the run started and was lifted by
> the owner while it was in flight (**D22**, `DECISIONS.md`). The spine conclusion is unaffected — it
> needs no SE change either way — but the **re-anchor** analysis may have designed around a constraint
> that no longer binds. Specifically, CR-A/CR-B/CR-C were selected under "the SE cannot be changed";
> a fourth option involving SE co-operation was never considered and should be, in the 2–3 day spike.
>
> **Independently corroborated:** §1.5's structural blocker — that the spine's exit path depends on a
> fee-bump remedy with no implementation — was confirmed the same day by a live measurement on Core
> 30.2.0 (`notes/WP1-TRUC-P2A-SPIKE.md`): TRUC admits **two** in-flight tiers and refuses the third
> (`TRUC-violation … would have too many ancestors`), package-CPFP rescue **does** work, and **no
> `submitpackage` caller exists anywhere in this tree**. Two independent lines of evidence agree.

Purpose: D1 (`docs/utexo/DECISIONS.md:88-114`) chose CTES-R as the normative RGB lane and flips
`SdkConfig::colored_ladder` to `true` (`clients/libs/rust-sdk/src/config.rs:283`, `:312`, both
currently `false`). D2 chose "land all remaining CATS-B units, then freeze". The coloured spine sits
behind both, and behind it sits the coloured on-chain re-anchor, which does not exist. Because these
were unscoped, `DECISIONS.md:268-272` withdrew the 12–16 week roadmap estimate. This document scopes
them and produces the number that un-withdraws it.

---

## 0. Verdict

`colored_ladder = true` is reachable, and the coloured **spine** is a bounded engineering unit rather
than a research problem — far more of it already exists in the tree than D1's table implies, and it
requires no `enclave/` or `lockbox/` change. The coloured **re-anchor** is not: it is not buildable
in the shape D1 assumes ("colour the refresh transaction"), because `refresh` routes through
`withdraw`, an RGB-unaware builder that refuses carriers one level up
(`clients/libs/rust-sdk/src/refresh.rs:209-212`, `clients/libs/rust-sdk/src/wallet.rs:2611-2625`),
and because a coloured re-anchor's transition is a **rival over the coin's confirmed funding outpoint
`F` against the ladder's own live trigger** — the exact collapse class `CTESR-GATE.md:113-172`
measured. Its design is selected by a 2–3 day experiment nobody has run, and the three candidate
designs differ by a factor of four in cost and by a whole product tier in outcome. The shortest
honest path is therefore: **(i)** run two Phase-0 spikes — the P2A/TRUC fee-bump run (which gates the
spine's exit path, D5 and D6 simultaneously) and the RGB rival-colouring run over `F`; **(ii)** build
the spine, whose long pole is the receiver's N-deep seal/witness walk, not the builders; **(iii)**
branch the re-anchor on the spike. **14–20 engineer-weeks single-threaded, 9–13 calendar weeks with
two tracks**, which re-costs the roadmap at **16–21 weeks to a full protocol v1 draft, +2–3 to
publication**. The single largest risk is not either item: it is that the spine's exit path contends
for a fee-bump remedy that **has no implementation anywhere in the repo** (§1.5).

**One row of D1's prerequisite table is stale and should be struck before re-costing.**
`DECISIONS.md:104` calls `clients/libs/rust-rgb/Cargo.toml:15` an uncommitted filesystem path,
"urgent independent of every other item". It is now
`rgb-lib = { git = "https://github.com/gofman8/rgb-lib", rev = "38e344e0…" }` at
`clients/libs/rust-rgb/Cargo.toml:30`, with the manifest comment describing the old defect in the
past tense and directing local fork work to a git-ignored `.cargo/config.toml` patch. The revision
resolves in the local cargo checkout. **Zero cost remains.** This also demotes the per-output
blinding item (§3, P2) from "first make the build reproducible" to a deliberate rev bump.

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

### 1.5 The structural blocker: the spine's exit path contends with a remedy that does not exist

This is the most important finding in this document and it is a **sequencing** conclusion.

* Every tier's fee is frozen at `committed_fee_rate = 2.0` for both presets (`lib/src/tesr.rs:210`,
  `:215`), baked in at signing. Above 2 sat/vB a tier does not relay standalone
  (`lib/src/tesr.rs:85-88`), leaving the P2A anchor as the only remedy.
* **There is no anchor spender in the repo.** `SPEC-ROADMAP.md:161`, `:164` records the grep:
  `P2A_SCRIPT_BYTES` / `p2a_script()` / `P2A_VALUE` appear only at definition, attach and
  vbyte-accounting sites; no builder constructs a `TxIn` on a P2A outpoint. The only CPFP builder
  fixes `input_vout = 0` and `version: 2` (`lib/src/wallet/cpfp_tx.rs:66`, `:111`) — and a v2
  transaction may not spend an unconfirmed v3 output at all. No `submitpackage` caller exists; all 18
  client broadcasts are `electrum_client::transaction_broadcast_raw`.
* **On a spine the anchor slot is contended.** TRUC allows an unconfirmed parent exactly one
  unconfirmed child, and `SP_{i+1}` at CSV 0 **occupies it**. The fee-bump remedy and the shape that
  most needs it are mutually exclusive (`SPEC-ROADMAP.md:164`, point 3). `PROTOCOL.md:245-247`'s
  claim that *"each tier confirms before the next is valid, so no long vulnerable chains"* is **false
  for exactly the spine**; `DECISIONS.md:148-155` already records that §6 must be rewritten.

Consequence for costing: a coloured spine exit is executable at ≤ 2 sat/vB and **not executable above
it**, today, for reasons that are not the spine's fault but that the spine makes worse. Building the
spine before WP1's P2A/TRUC spike reports is building an exit path whose escape hatch may be
unreachable. Hence G1 in §3.

The coloured economics are already thin, which is why this matters rather than being an edge case. A
coloured leaf walking its own two rungs loses 1 152 sat of a 3 066-sat piece (37.6%), leaving a
**1 914-sat** owner output; generously accounted (leaf's own two rungs only, ~152 vB v3 CPFP child
per `SUBECONOMIC-FINALITY.md:55-58`), the external-sats break-even for exiting it is ≈ **3.3
sat/vB**. **UNVERIFIED**: that 152 vB is a cited figure, not a measurement — no anchor spender exists,
so no measured number exists.

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

* **Depth cap.** Mainnet `max_exit_txs = 23` (`lib/src/transfer/receiver.rs:806-812`) ⟹ **d ≤ 19**.
  On the deployed profile (`network = "testnet"` silently falls to the regtest schedule) the cap is
  139 ⟹ ~135 levels, whose TRUC stall alone (~68 blocks) exceeds the entire regtest state schedule
  (`DECISIONS.md:73-77`). **D7 must land before the spine's depth cap means anything.**
* **Latency.** `tesr_exit_wait_blocks` (`config.rs:196-207`) has already been re-derived for the
  spine: `per_level = ext_csv(0) + SPINE_CSV = 720 + 0`, giving `720d + 2160`. It charges **nothing**
  for the one block per level the parent needs to confirm, which its own doc-comment names
  (`tesr.rs:5341-5343`). Under TRUC the practical floor is ~1 block per 2 consecutive zero-CSV tiers.
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
`tokens.rs:117-123`). The spine changes what the change **is**: a tip, not a child. So it lifts the
cap **on the sender side, completely** — a root coloured carrier becomes a wallet balance, batch 1
and batch 1000 built by the same function.

It does **not** lift it on the receiver side. After the spine lands, the honest end state of a
coloured coin is:

* **Sender / root carrier** — unlimited partial payments, bounded only by the exit-chain length cap
  (d ≤ 19 mainnet) and by the absolute epoch deadline (§2.4).
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

---

## 2. The coloured on-chain re-anchor

### 2.1 Not buildable as assumed

D1's table row assumes "colour the refresh transaction". That is wrong at three levels.

**It is not a flag on the existing driver.** `reanchor`
(`clients/libs/rust-sdk/src/refresh.rs:131-234`) mints a fresh deposit aggregate for exactly
`amount − ceil(112 · rate)` using a *free* derived-slot voucher (`:184-198`) and then calls
`self.withdraw(&fresh_addr, …)` (`:209-212`). `withdraw` refuses a named carrier one level up
(`clients/libs/rust-sdk/src/wallet.rs:2611-2625`) and `withdraw::execute → new_transaction` is a
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

### 2.2 The blocker that makes this a spike, not a builder task

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

**Verdict: the coloured re-anchor is a 2–3 day experiment whose outcome selects among three designs.
Costing it as a builder task before that experiment runs would be a guess presented as a plan —
precisely why `DECISIONS.md:268` withdrew the estimate.**

### 2.3 The three designs

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
**Cost: 5–7 weeks. Best outcome; highest variance.**

**CR-B — exit-and-redeposit, if the spike fails.** Walk the coloured ladder on chain (`T`, `X_m`,
`S_k`: 3 coloured tiers, 504 vB — this is sdk75, already proven, and `colored_exit_move` already
books the move, `tesr.rs:974-985`), then sweep `S_k`'s payload output into a fresh deposit aggregate
with a coloured builder. Requires **no new RGB rival behaviour whatsoever** — the ladder is genuinely
spent, not shadowed. Costs 4 transactions / ~659 vB against CR-A's 1 / ~155, and — the real price — a
full CSV wait of `e_m + d_k` = 2 160 blocks ≈ 15 days on the mainnet schedule before the coin is
usable again. The sweep still needs a coloured builder for a key that is **not in rgb-lib's BDK
descriptor** (every tier pays a Mercury seed-derived key), so it is not free.
**Cost: 3–3.5 weeks. Materially worse product; certain to work.**

**CR-C — build nothing; make the bound explicit and enforced.** Accept that a coloured coin's life is
bounded by the absolute flat-backup horizon `L0 = H_deposit + initlock` (1 000 blocks ≈ 6.9 days on
the deployed profile, `server/Settings.toml:2-3`) and ends in a forced on-chain exit. Build only the
two things that convert a silent loss into a clean refusal: the sender-side pre-flight (§3, R1) and a
coloured near-deadline force-exit. Write the limitation into §10/§12 rather than around it.
**Cost: 1.5–2 weeks. Honest, cheap, and it makes D1's "a real ladder" claim materially thinner.**

### 2.4 What renewal is *not*

`build_colored_renewal` (`tesr.rs:2085-2176`) is not a substitute and must not be presented as one.
It replaces `X_m` over the trigger's payload output off-chain at zero vB — it never touches `F` and
never touches the flat-backup chain. Three budgets, and renewal refills one:

| budget | size | refilled by renewal? |
|---|---|---|
| state rungs | `d0=1440, δ=36, d_floor=144` ⟹ 36 hops/epoch | yes |
| extension rungs | `e0=720, δE=36, m_max=15` ⟹ 15 renewals, then `needs_rollover` — and **coloured rollover does not exist** (`tesr.rs:7033`, `:1351-1362`, `:9172`) | partially, then terminal |
| **absolute clock** `L_k = H_deposit + initlock − k·interval` (`epoch_deadline_from_flat_backups`, `tesr.rs:5607`) | 1 000 blocks ≈ 6.9 days deployed | **no — renewal cannot reach it at all** |

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

One more closed door: `cosign_detrigger` refuses coloured outright (`tesr.rs:7367-7377`) and is
unwired anyway. **A coloured coin today has no cooperative primitive of any kind.**

---

## 3. Dependency-ordered work plan

Gates are observable — a test that goes green, a measured number, a written verdict — never
"reviewed".

### Phase 0 — unknown reducers (parallel, 1–2 wk; folds into WP1)

| # | Item | Gate |
|---|---|---|
| **G1** | **P2A/TRUC live run.** Broadcast three consecutive zero-CSV v3 tiers on regtest with `minrelayfee` above 2 sat/vB; record Core's rejection string; submit one 1P1C P2A package; choose a package-capable backend | A written verdict: either a tier is reliably bumpable through its anchor, or it is not. Cites a run, not a reading of policy. **Gates the spine, D5 and D6 simultaneously** |
| **G2** | **RGB rival-colouring spike over `F`.** Colour a second transition spending `F` while the ladder's trigger consumes that Opout as Tentative, **in a second process**; mine it; check `select_valid_witness`; validate the new ladder's leaf consignment at a receiver | Pass ⟹ CR-A. Fail ⟹ CR-B or CR-C. 2–3 days |
| **G3** | **D7 profile reconciliation** (already WP4). Explicit `TesrParams` for testnet/signet; one normative `lockheight_init` | A unit test asserts every network string with no silent default. Until this lands, "d ≤ 19" is not the deployed number and neither the depth cap nor the epoch horizon means anything |
| **G4** | **Measure the coloured re-anchor vsize** through the production finaliser, as `the_coloured_fee_matches_a_measured_signed_tier` does | A pinned constant, not `112 + 43` |

### Phase 1 — the spine (sequenced after G1 and G3)

| # | Item | Dep | Gate | Band |
|---|---|---|---|---|
| **S1** | `TierRole::Spine = 0x0C` appended; cap seal derivation | — | Round-trip test; tag census test still green | 0.5 d |
| **S2** | Coloured cap builder + `ColoredTip` population + journal tip variant (a `SplitJournalChild` shape holding a one-tier coloured leg + `PendingTier` for the cap) | S1 | The refusal at `tesr.rs:3965` is deleted and a unit test builds a coloured cap whose consignment contains its own witness | 2–3 d |
| **S3** | `change_leg_role(SplitLane::Colored) → SpineTip` — **same commit as S2** | S2 | A test asserts floor 906 and the one-tier builder together; a test asserts the two can never disagree | 0.5 d |
| **S4** | `build_colored_spine_batch` / `cosign_colored_spine_batch` | S2 | `refuse_uncolored_over_colored_tip` at `:4646` replaced by a lane fork; a coloured tip batched twice in-crate; census `= 2` per slot asserted | 3–5 d |
| **S5** | **Receiver: N-deep `colored_child_txids` / `colored_child_seals`; resolve `consignments.len() == tiers.len()` for intermediate segments; enforce `m = 0` on spine segments** | S4 | A receiver adopts and books a piece at spine depth 2; a hand-crafted bundle with a mislabelled segment gets a **named** refusal and `assert_not_an_unrelated_refusal` passes | 6–9 d, **could be 3 wk** |
| **S6** | Crash-recovery replay for a coloured tip leg (`SplitJournalRecord::spine_tip`, `resume_in_ladder_split`) | S4 | A fault-injected crash between co-sign and conveyance resumes; a test asserts no phantom rival is ever co-signed over `SP.out[K]` | 3–5 d, low confidence |
| **S7** | Booking / health / watchtower / `auto_exit` head-start over an N-level chain | S5 | `WatchEntry` expresses "the sender confirmed `SP_i`", or the gap is closed by name (`PARTIAL-PAYMENT-ECONOMICS.md` §4.7 calls the current shape the repo's silent-degradation shape) | 3 d+ |
| **S8** | Cost + **latency** model re-derivation: `4 + d` for spine levels, 211 vB coloured, floors, and the per-level parent-confirmation block the model charges zero for | G1, WP5 | One test derives txs, vBytes **and wait** from measured transactions at depths 0..3 and fails if any drifts. Note this moves a receiver-side admission rule (`max_exit_txs`), so budget test churn | 2 d + churn |
| **S9** | Live-stack E2Es: a coloured carrier making two sequential payments; a receiver claiming at depth 2; a crash-recovery replay | S5–S7 | Green on the live stack, on the **G3** profile | 5 d+ |

### Phase 2 — the re-anchor (branch on G2)

| # | Item | Gate | Band |
|---|---|---|---|
| **R1** | **Sender-side horizon pre-flight** in `colored_in_ladder_pay` *and* `in_ladder_pay` — fetch tip + parent flat chain, run the same `validate_backup_chain_v2` the receiver will run (`tesr.rs:5783-5796`), refuse **before the first co-sign** | sdk32's reorder at `:288` is undone; the post-horizon send **refuses on the sender** and the SUCCESS-line KNOWN GAP at `:472` is deleted. **Do this regardless of branch — lane-independent, and it is days** | 3–5 d |
| **R2a** | CR-A: bridge surface (fork repair primitives exposed) + rev bump + coloured broadcast shape | `cargo build` from a clean clone at a tagged rev; a unit test archives a dead ladder without touching `update_witnesses` | 1–2 wk |
| **R2b** | CR-A: builder + driver + measured fee + routing of `refresh` / `auto_refresh_due` with a fail-closed `is_colored()` gate read from the coin's own `tesr-` row | sdk30's coloured sibling: allocation survives; a fresh coloured ladder establishes over `F'`; old tiers are unbroadcastable **and** retired; a post-re-anchor conveyance claims at the far end | 2–3 wk |
| **R2c** | *or* CR-B: coloured sweep-to-deposit after a full exit walk | Same E2E, minus the rival; asserts the 15-day CSV wait explicitly | 3–3.5 wk total |
| **R2d** | *or* CR-C: coloured near-deadline force-exit + the normative limitation | A test that a coloured carrier approaching `min(L_k)` is force-exited; §10 states the bounded life | 1.5–2 wk total |
| **R3** | Fix `build_colored_child_retransfer`'s message at `tesr.rs:6375`, which still offers a re-anchor a leaf **structurally cannot have** | The source-inspecting test at `:20063-20074` that banned that exact phrase from the plain sibling is extended to the coloured one | 0.5 d |

### Phase 3 — the rest of D1's table

| # | Item | Gate | Band |
|---|---|---|---|
| **P1** | WP6(i) receiver-side refusal: a conveyed transfer bearing both a plain ladder and an off-chain-commitment envelope is rejected (today sender-side only, `wallet.rs:866`; `transfer_receiver.rs:885`, `:967` persist `rgb_consignment` with no cross-check) | Adversarial test through `validate_encrypted_message` | 2–3 d |
| **P2** | Per-output blinding in the rgb-lib fork + rev bump; lift `refuse_colored_multi_payee`. `AssetColoringInfo.static_blinding` is a single `Option<u64>` (`rust_only.rs:19-20`) applied to every vout (`:480-484`) **and reused as the opret's MPC entropy** (`:571-576`) — one number does two jobs and they must be split without breaking anti-collision | Coloured K=3 batch; a payee cannot open a sibling seal; opret MPC entropy unchanged. **Gates coloured K>1, NOT the spine** — a coloured spine batch at K=1 is a 2-payload `SP`, byte-for-byte the shape coloured split ships today (`tokens.rs:214-215`, `:230`) | 1–1.5 wk |
| **P3** | LN: pass `Some(batch)` on the coloured child lane (plumbing exists — `convey_child_bundle(.., batch_id)`, `tesr.rs:6660-6705`; the plain lane already does it at `transfer.rs:1636-1646`) **plus the real work**: an SSP pre-pay RGB gate over a coloured child's off-chain witness chain, since `validate_pending_token` resolves only `branch_txs` + `BackupTx.rgb_consignment` (`ssp.rs:487-500`, `tokens.rs:5140-5153`), and an F7 journal for the coloured lane (`tokens.rs:3512-3516`) | An SSP refuses to pay against a coloured child whose allocation it cannot verify; a latched coloured send resumes after a crash | 0 or 1.5–2 wk — **blocked on D21** |
| **P4** | JS/web/Kotlin. **Smaller than D1's table says on JS, larger on Kotlin.** The *product* SDK (`clients/libs/nodejs-utexo`) is a JSON-lines client over the Rust daemon and is unaffected (`clients/apps/web-wallet/server.js:22`); the fail-closed guards bite only the legacy `clients/libs/nodejs` (consumer: `clients/apps/nodejs/index.js:140`) and `clients/libs/web` (**no in-repo consumer found**). Kotlin's `FFITransferMsg` has no field for laddered material at all and refuses rather than truncates (`lib/src/unifii_interface.rs:55-81`) | Owner decision first: does `clients/libs/web` have an out-of-repo consumer? | 0, 2–3 wk, or unbounded — see §4 |
| **P5** | Inventory the two capabilities the flip silently removes (coloured multi-carrier combine, coloured leaf consolidation) into D3's `V_min` work | They appear in DECISIONS.md before §10 is drafted | 1 d |

---

## 4. Effort estimate — the number that un-withdraws the roadmap

**Engineering to make `colored_ladder = true` defensible: 14–20 engineer-weeks for one engineer, or
9–13 calendar weeks with two working the independent tracks.**

| Block | Range | Confidence |
|---|---|---|
| Phase 0 gates (G1–G4) | 1–2 wk | High — scoped experiments, shares WP1's fixture |
| Phase 1 spine (S1–S9) | 6–9 wk | Medium |
| Phase 2 re-anchor: **CR-A** 5–7 / **CR-B** 3–3.5 / **CR-C** 1.5–2 (+ R1, R3) | 1.5–7 wk | **Unknown until G2** |
| Phase 3 P1 + P2 + P5 | 2–3 wk | Medium-high |
| Phase 3 P3 (LN) | 0 or 1.5–2 wk | Blocked on D21 |
| Phase 3 P4 (clients) | 0, 2–3 wk, or **unbounded** | See below |

Feeding back into the roadmap: the 12–16 weeks was a **specification** deliverable, and Phases 0–3
overlap its drafting rather than preceding it. The spine and re-anchor gate §5, §6, §10 and §11
specifically. **Re-cost: 16–21 weeks to a full protocol v1 draft, +2–3 to publication** — the
increase over 12–16 being roughly the spine's non-overlapping tail plus whichever re-anchor branch G2
selects. If G2 fails and CR-C is chosen, it lands at the low end; if CR-A is chosen and S5 blows up,
at the high end or past it.

### Which items can blow up, and why

**S5 — the receiver's N-deep seal/witness walk. This has the smell.** It touches a conveyed wire
format whose own doc-comment warns that a defaulted field is downgrade surface
(`tesr.rs:1263-1266`); every check on that path is an exact-equality census where a miscount means
*"no receiver can adopt any piece of the batch"*; and it must resolve a shipped invariant
(`consignments.len() == tiers.len()`, `:9183-9190`) that nobody wrote with an N-segment chain in
mind. Nominal 6–9 days; could be three weeks.

**S6 — crash-recovery replay. This also has the smell.** Crash-safety code where a wrong replay
co-signs a phantom rival over `SP.out[K]` — the exact failure `SplitLegRole`'s doc-comment exists to
prevent (`tesr.rs:3134-3146`) — and "only ever after a crash" is precisely where this repo's
silent-degradation defects live. `resume_in_ladder_split` was not read in full; **3–5 days is the
least grounded number in this document.**

**G1 — if the P2A/TRUC spike reports that a tier cannot be reliably bumped above 2 sat/vB, the
spine's value proposition changes and D5 reopens.** Who funds a bump, from which UTXO, under TRUC's
one-child and 1000-vB limits, and what stops a third party pinning an anyone-can-spend anchor, is
open design nobody has started (`SPEC-ROADMAP.md:161-175`).

**G2 — a fail here does not merely delay CR-A, it changes the product.** CR-B costs 4× the bytes and
15 days of latency per renewal; CR-C means a coloured coin's life is bounded by `initlock` and ends
in a forced exit. Both are shippable; neither matches what D1's table implies was being bought.

**P4 — the client port has an unbounded tail.** Porting `verify_bundle` is a crate **move**, not a
rewrite (~860 lines: `verify_bundle_ex` 445, `verify_colored_shape` 77, `verify_bundle_bound` 64,
`verify_conveyed_child` 271), and it is genuinely pure — but it lives in `mercuryrustlib`, which
pulls electrum/reqwest/sqlx/tokio/rgb-lib, while the wasm crate depends only on `mercurylib`
(`wasm/Cargo.toml:20`). Even a *completed* verifier port leaves a JS client able to verify the ladder
and unable to book the allocation, because `accept_colored_ladder` / `accept_colored_child_bundle`
(`tokens.rs:2061`, `:2195`) go through rgb-lib.

### Named unknowns

* **UNVERIFIED** — whether rgb-lib will colour a rival transition over `F` at all (G2). Everything in
  Phase 2 hangs on it.
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
* **UNVERIFIED** — the watchtower story for a coloured spine (`PARTIAL-PAYMENT-ECONOMICS.md` §4.7
  says `WatchEntry` cannot express "the sender confirmed `SP_i`"). `watchtower.rs` was not read; if
  that is open, S7 is bigger than 3 days.
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

Three places where an SE-shaped answer exists and is deliberately **not** designed here:

1. **The census RHS is unauthenticated, and flipping the flag makes every RGB coin depend on it.**
   `sig_count` travels as a bare JSON integer with no signature or freshness token; the enclave never
   reads or signs it; the client trusts it as the census RHS at `tesr.rs:9093`. A coordinator that
   under-reports by *k* hides *k* co-signed rival states and the exact-equality census still
   balances. `DECISIONS.md` D8 records this as theft-class and VERIFIED; D8-CLOSE (`:245-252`)
   sketches closure as a signature over two fields on one endpoint. **Owner decision.**
2. **The epoch problem — the thing the re-anchor exists to solve — has an SE-shaped alternative.**
   The binding constraint on a coloured coin is the absolute flat-backup chain rooted at
   `H_deposit + initlock`, which is simultaneously the census term and the epoch clock
   (`epoch_deadline_from_flat_backups`, `tesr.rs:5607`). If the census rested on SE-attested per-slot
   state instead, the absolute clock would stop being binding and CR-A/B/C would all become optional.
   D4 rejected SE-side counters partly on scope. **Flagged, not designed.**
3. **Per-output blinding requires an rgb-lib fork change** (`rust_only.rs:19-20`, `:480-484`,
   `:571-576`), i.e. a deliberate bump of the pinned rev at `clients/libs/rust-rgb/Cargo.toml:30`.
   Not SE work, but a dependency-pin decision with the same "who owns this" character, and it must be
   an explicit choice rather than a side effect of a build.

---

## 6. Open questions for the owner

1. **Which scope rule is current?** This document was produced under a brief stating `enclave/` and
   `lockbox/` are permanently out of scope. `DECISIONS.md:212-247` records **D22, dated 2026-08-11**:
   *"SE scope rule REVOKED — `lockbox/` and `enclave/` are in scope"*, re-opening D8-CLOSE, D10 and
   the SSP expiry gates. Nothing above designs an SE change. If D22 is current, §5 items 1 and 2
   become live options and item 2 in particular may make the whole re-anchor optional — which would
   change the estimate materially.
2. **G2's outcome selects the product, not just the schedule.** If the rival-colouring spike fails,
   which is chosen: CR-B (4× bytes, 15-day latency per renewal) or CR-C (coloured coins have a
   bounded ~6.9-day life on the deployed profile and end in a forced exit)? This should be decided
   *before* the spike runs, so the spike is a gate rather than a discussion.
3. **Does `clients/libs/web` have an out-of-repo consumer?** No in-repo consumer was found (the
   `web/` React app's `package.json` carries only react/react-dom/cra-template). If there is none,
   P4 shrinks to `clients/libs/nodejs` + `clients/apps/nodejs` and drops substantially in priority.
4. **D21 — RGB over Lightning: build it or exclude it by name?** P3 is 0 or 1.5–2 weeks entirely on
   this answer.
5. **Does the coloured leaf get renewal, or is exit-only within one epoch normative?** A received
   coloured piece can never be split, renewed, combined or re-anchored (§1.7). The spine does not fix
   this. Either coloured leaf renewal is built (a separate ~2-week unit) or §10 states the
   limitation.
6. **Are the two silently-removed capabilities** — coloured multi-carrier combine and coloured leaf
   consolidation (§1.7) — **accepted losses?** They are not in D1's table and D3's `V_min` work will
   hit them.
7. **Strike `DECISIONS.md:104`** (rgb-lib as an uncommitted path) before re-costing: it is fixed
   (`clients/libs/rust-rgb/Cargo.toml:30`).
