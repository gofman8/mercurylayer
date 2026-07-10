# Utexo V2 — renewal & invalidation redesign

**Status**: chosen architecture, post-adversarial-review. This document supersedes the V1 calendar-refresh
model described in `learn/invalidation-deep-dive.md` §1b/§6 for all new deposits once shipped.

**Chosen architecture**: **TES-R** — *Trigger / Extension / State with self-split Rollover*.
It is a composition: the `csv-trigger` (TES) candidate is the chassis; two load-bearing elements are
imported from the `factory-amortized` (Grove) candidate's non-factory half (the cooperative de-trigger
and the off-chain self-split rollover); every FIXABLE adversarial finding against TES is folded in as a
design amendment; both of TES's FATAL findings are addressed structurally (§5.8, §6). The Grove factory
primitive and the evolved absolute ladder are rejected (§8).

---

## 1. The 4 requirements (+ the delegation refinement)

1. **Off-chain forever, unlimited off-chain state transitions.** Refinement (product owner): periodic
   on-chain is acceptable **iff** (a) footprint scales with *activity*, not time — idle coins cost
   ~nothing — or is heavily amortized/batched, **and** (b) users never act personally: an
   operator/tower does it, priced into onboarding/transaction fees.
2. **No/low operator liquidity.** No Ark-style round liquidity, no SSP denomination pools, no
   SuperScalar leaf stock. Native exact-amount split/combine (value conserved from the user's own
   coin) must survive.
3. **Non-custody under operator hack.** A hacked operator may steal *future* deposits and *future*
   state transitions, but pre-hack state left untouched must be absolutely safe. Preserve or improve
   today's guarantees (2-of-2, share rotation s+e=const, enclave deletion).
4. **Blind operator for RGB.** The SE may know everything about sats; it must never learn RGB
   contents in-system (blind co-signing preserved). Exit-time RGB disclosure is acceptable.

**Hard constraints honored**: no soft forks (no APO/CTV/CSFS; TRUC/v3 + P2A are relay policy and ARE
used); single SE + enclave; blind co-signing, exact-amount split/combine DAG, RGB on un-broadcast
carriers, Lightning latch all kept; towers delegatable + keyless, mandatory-forever acceptable;
unilateral exit always exists for the current owner; **missed liveness must never mean
confiscation-by-design** — nothing in TES-R ever pays the operator by timeout.

---

## 2. Why V1's absolute ladder cannot scale (the block-space wall)

V1 anchors every coin's decrementing nLockTime ladder to an **absolute** deposit height. Staying alive
requires an on-chain re-anchor (refresh, 112 vB) per coin per ~initlock (~1,008 blocks ≈ 7 days),
regardless of activity:

- **Idle rent**: 112 vB × 52,560/1,008 ≈ **5,840 vB per coin-year** — independent of whether the coin
  ever moves.
- **1M coins**: 1M × 112 / 1,008 ≈ **111,111 vB per block ≈ 11.1%** of all Bitcoin block space.
- **10M coins**: ≈ **111%** — physically impossible.
- **Prefunding the rent** (the "hide it in onboarding" route): 5 idle years at an average 10 sat/vB
  ≈ 292,000 sats ≈ $292/coin @ $100k/BTC — exceeding the entire value of most retail coins. The
  refinement's clause (b) cannot rescue clause (a): delegation changes who *pays*, not how much chain
  space is *burned*.

The root cause is architectural: **absolute timelocks age while un-broadcast**. The clock runs on the
calendar, so the defense must be renewed on the calendar. Both adversarial economics reviews confirmed
this arithmetic independently; the evolved-absolute candidate's own analysis concedes the rent is
structurally O(coins × time / initlock) and merely divides it by ~11.5× (see §8.2). The only cure is to
change the timelock type: **relative (CSV) locks on un-broadcast transactions do not tick until a
parent confirms** (BIP-112). Then the clock starts on *attack*, not on deposit — and idle coins never
age at all.

---

## 3. Comparison landscape

| | **V1 (Mercury Utexo)** | **Spark** | **Ark** | **SuperScalar** | **V2 (TES-R)** |
|---|---|---|---|---|---|
| Idle on-chain footprint | 5,840 vB/coin-yr (112 vB per ~7d) | **0** | Refresh per round or lose funds | Per-factory ladder txs (~monthly, amortized /N) | **0** |
| Renewal mechanism | On-chain re-anchor per coin | Off-chain re-sign with operator group (`renew_leaf`) | On-chain round through ASP | New factory before dying period | **Off-chain**: DW lower-CSV extension re-sign; self-split rollover at epoch exhaustion |
| What invalidates old state | Consensus (absolute locktime ordering) + enclave refusal | **1-of-n operator honest key deletion** (trust) | Round expiry (consensus) | Decrementing nSequence DW (~64 updates then exhaustion) | **Consensus** (lower-CSV wins the trigger output; old epochs' parents unconfirmable) + enclave refusal as defense-in-depth |
| Missed-liveness outcome | Raceable after window; never confiscated | Safe (operator renews); trust-dependent | **CONFISCATION** — funds sweep to server after expiry | **CONFISCATION** — "the LSP can simply claim the entire UTXO" after the dying period (delvingbitcoin 1143) | Raceable **only after** a public on-chain trigger + ≥144-block CSV notice; **never confiscated** — no output ever pays the operator by timeout |
| Operator liquidity | None | SSP denomination-swap pools | Round liquidity fronted per refresh | LSP leaf liquidity ("1/(N+1) of its funds") | **None** (fee-sized outlays only; §7.4) |
| Exact amounts | Native split/combine | Fixed denominations + SSP swaps | Fixed by round | Fixed by leaf | **Native split/combine preserved** |
| Operators | 1 (blind SE + enclave) | t-of-n group | 1 ASP | 1 LSP | 1 (blind SE + enclave), unchanged |
| Unilateral exit wait | ≤ ~7 d | Relative timelock wait (2000→ blocks, decrementing) | Before expiry only | O(log N) txs, force-exits sibling subtrees | E+D sequential CSV: ≤15 d flat, decreasing with activity; deeper for sub-coins (§5.9) |
| Watching duty | Deadline-bounded (~7 d windows) | Operator-dependent | Deadline-critical (miss = lose) | Once per factory lifetime (miss = lose) | Perpetual but **event-driven** (alarm = public trigger tx; ≥1 day notice), keyless, delegable |

Honest placement: **Spark is the footprint benchmark** (zero periodic on-chain), bought with 1-of-n
key-deletion trust for invalidation and fixed denominations. **Ark and SuperScalar both have
expiry-confiscation** — disqualified outright by our constraints. TES-R reaches Spark-class footprint
(idle exactly 0; ~0.06 vB per transfer amortized) with **consensus-level invalidation** and **no
denominations**, at the cost of perpetual (but alarm-driven, keyless, cheap) watching and longer
unilateral-exit latency.

---

## 4. Architecture selection — how the adversarial findings were weighed

Three candidates, six adversarial reviews. Verdict logic:

- **Grove (factory-amortized)** carries a FATAL REQ3 break that is *unfixable within the
  architecture*: the shared factory root is an n-of-n of operator-chooseable signers that never
  rotates; a Sybil-originated factory (or hacked-SE + original-cohort collusion, "B1-F") fresh-spends
  the root and confiscates all 64 coins — including received-and-untouched ones — via private relay,
  with no race the victims can enter and no receiver-side check that can price it. It also deletes
  V1's one absolute guarantee (self-deposited never-transferred coin safe against SE alone). Both
  reviewers converged on this; one additionally showed the factory buys almost nothing (input-bound
  amortization: ~101.5 vB/user real vs ~111 solo, a one-time ~10–50 vB saving) while its **Primitive 2
  — the CSV trigger ladder — delivers 100% of the headline win standalone**. We accept that salvage
  verdict: the trigger-ladder ideas (co-op de-trigger, self-split renewal, tower fee bond) are folded
  into TES-R; the factory is rejected (revisited only under covenants, §10).
- **Evolved absolute ladder (C-90)** is honest but self-refuting: its own math shows the rent wall is
  structural (10M coins ≈ 9.6% of block space; 100M ≈ 96%), the idle sub-coin DAG problem is
  *unsolved* (pre-signed chains re-anchor flat coins only; an idle received sub-coin still hits the
  root deadline), and the 90-day unilateral wait is the same number as the footprint win. Both
  reviewers scored it a bridge, not a destination — and TES-R's migration is a single re-anchor per
  coin (§9), so no bridge is needed. Rejected entirely; its useful ideas (P2A on everything,
  tower-executed maintenance) are subsumed.
- **TES (csv-trigger)** drew one review with two claimed FATALs (trigger griefing; trigger + offline
  victim = theft) and one with none ("no fatal under the stated requirements"). We judge both claimed
  FATALs as **serious-but-answerable**: the griefing FATAL is defanged by the cooperative de-trigger
  (§5.8) that the TES draft missed and the Grove review supplied — grief cost collapses from
  forced-settlement (~828 vB + days) to one 111-vB co-op re-anchor; the offline-theft FATAL is
  precisely the race class the hard constraints *explicitly tolerate* ("a race you can lose while
  unwatched is tolerable iff towers are cheap+delegatable"), and TES gives strictly more notice
  (≥144 blocks of public on-chain alarm) than any competitor's silent maturity. Every FIXABLE finding
  from both reviews is folded in below and marked **[Amendment]**. The RGB colored-extension
  contradiction (SERIOUS in both reviews) is resolved by *deleting* the re-signed colored extension
  and enforcing a terminal-freeze rule (§5.10) — accepting the finding rather than arguing it.

---

## 5. The chosen architecture: TES-R

### 5.1 What is kept from V1, unchanged

Single blind-MuSig2 SE (signs only 32-byte sighashes); key-share rotation s_n + e_n = const with
enclave deletion of old shares; per-node spend budgets / terminality with public receipts
(`GET /statechain/spend_budget` generalized); receiver-verifies-everything at claim; keyless
delegatable watch bundles (REQ-34); RGB allocations anchored to (possibly un-broadcast) carrier txs
with P2P consignments; Lightning latch; exact-amount split/combine DAG with Σout value conservation.
The enclave's cryptographic core is **unchanged** — blind MuSig2 is sequence-agnostic.

### 5.2 The three tiers (per coin)

The funding UTXO **F** (P2TR key-path, aggregate A = KeyAgg(P_user, P_SE)) is the only thing resting
on-chain. Above it, a pre-signed, un-broadcast tree:

```
F  (on-chain, value v)
└─ T   TRIGGER    [v3, NO timelock, signed ONCE at deposit detection, never re-signed]
   │               out[0]: v−fee_T → P2TR(A + H_tag("TES/trigger",A)·G)   out[1]: 240-sat P2A
   ├─ X_0 … X_m  EXTENSIONS  (mutually exclusive spends of T.out[0])
   │               X_m: [v3, input nSequence = CSV E_m = E0 − m·δE]
   │               signed at deposit (m=0) and at each off-chain renewal (m≥1)
   │               out[0] → P2TR(A + H_tag("TES/ext",A,level,m)·G) + P2A
   └─ (on X_m.out[0])  S_0 … S_k  STATES / SP splits / CB combines  (mutually exclusive)
                   S_k: [v3, input nSequence = CSV Δ_k = D0 − k·δ], pays owner k
```

**Constants (shipped defaults, all served via `/info/config` and — [Amendment, TES-fixable #7] —
published in the SE's signed nostr record so a receiver can detect a per-victim parameter split):**

| Parameter | Value | Meaning |
|---|---|---|
| D0 / δ / D_floor | 1,440 / **36** / 144 blocks | state tier: 36 hops per epoch, ~6 h head start per hop |
| E0 / δE / E_floor | 720 / **36** / 144 blocks | extension tier: 17 epochs; **forced rollover at m = 15** [Amendment: never operate at the floor with a minimal edge] |
| Hop budget per depth level | 36 × 16 = **576 transfers** | between depth-increments; see §5.6 |
| Worst flat unilateral wait | E0 + D0 = 2,160 blocks ≈ **15 days**, decreasing 36 blocks/hop | |

δ = 36 (≈6 h) rather than the TES draft's 24 (≈4 h): both economics reviews flagged the head start as
the single parameter everything stands on, and mainnet has sustained >4 h full-block spikes. The
budget sensitivity is stated honestly: δ = 24 → 1,350 hops/level; **36 → 576**; 72 → 162; 144 → 45.
Because rollover (§5.6) is off-chain, even conservative δ only trades exit weight, never chain rent.

**The core property**: all three tiers are un-broadcast; BIP-112 CSV ticks only after the parent
confirms; T has no timelock. So **nothing anywhere matures until someone broadcasts T on-chain**. An
idle coin — and an entire idle split/combine DAG — never ages. No calendar deadlines, no refresh rent,
no root-deadline materializations. The whole audit-[17]/B6 class (deposit-anchored deadline
arithmetic) is deleted wholesale, along with `auto_exit_due`'s calendar machinery.

### 5.3 Fees [Amendment: TES-fixable #2 + Grove reviewer's pinning finding, merged]

Every pre-signed tx (T, X, S, SP, CB) is **nVersion=3 (TRUC)** and carries:
1. a **small committed fee** drawn from the coin (target ~2 sat/vB at signing; Σout = Σin −
   fee_committed) so the base case relays and confirms standalone — restoring V1's self-funding
   property and removing the chicken-and-egg the review found in the all-zero-fee draft; and
2. a **240-sat P2A anchor** (OP_1 0x4e73, anyone-can-spend) so any party — owner, keyless tower,
   operator — attaches a live-rate fee child (~152 vB: 41 P2A-in + 57.5 funding-in + 43 change + 10.5)
   during spikes.

TRUC's 1P1C topology + sibling eviction gives pinning resistance (each tier confirms before the next
is valid, so no long vulnerable chains); v3+P2A is relay *policy*, not consensus — the
miner-direct-submission fallback is documented, the same bet Lightning anchors make [Amendment:
economics-fixable "state the policy dependence"]. The committed fee is small and bounded-stale (it is
a floor, not the whole fee), so V1's 2–13 sat/vB stranded-reserve pathology does not return in force.

### 5.4 Splits, combines, sub-coins

A split SP is a state-tier tx: spends X_m.out[0] at nSequence Δ_{k+1} (one decrement below the
splitter's own retained state), N child resting outputs with **exact amounts, Σout = Σin −
fee_committed**, + P2A. Parent terminalized at the SE before co-signing, exactly as today. Each child
resting output hosts its own extension + state tiers — **no trigger needed**: SP is itself
un-broadcast, so nothing ticks until it confirms. A combine CB carries per-input nSequence = that
input's Δ (BIP-112 is per-input); Σ-inputs terminal-ancestor verification (R7) unchanged. Child coins
get fresh slots via the free derived-token path (REQ-35 analog).

### 5.5 Renewal (pure off-chain; the common case)

When Δ_{k+1} would fall below D_floor, the SDK runs renewal inside `transfer()` — replacing V1's
on-chain auto-refresh with **zero on-chain bytes**:

1. `POST /renew/init {statechain_id, challenge_response}` — **[Amendment, TES-fixable #1]** bound to
   the single-use endpoint-bound challenge nonce (the audit-[15] rail already in the codebase), and
   the SE **refuses unless the state counter is genuinely within 2δ of the floor** — killing both the
   replay grief and the arbitrary-epoch-burn lever the review found.
2. Two blind MuSig2 co-signs: X_{m+1} spending T.out[0] at CSV E0 − (m+1)·δE, then the transfer's own
   state S'_0 on X_{m+1}.out[0] at CSV D0. Key rotation covers all tiers (tweaks are public).
3. SE atomically advances the public counters {level, m, k, total_sigs} (the spend_budget endpoint
   generalized). **[Amendment, TES-fixable #5]** when a Lightning-latched transfer triggers renewal,
   both co-signs are gated by the *same* preimage/latch — no half-renewed extension is observable to
   the SSP before payment settles.

**Why old state dies at the consensus level, not by enclave promise**: X_{m+1} strictly undercuts
every older extension in the CSV race for T.out[0]; every pre-renewal state hangs on an extension
that can now never confirm. This is Decker-Wattenhofer replace-by-lower-timelock at one dedicated
tier — renewal replaces horizontally, tree depth stays constant across all epochs. Honesty note
(accepted from review): "consensus-dead" is *race-conditional* — the newest extension must win a
≥36-block-edge race after a public trigger. That is strictly stronger than V1's post-window position
and categorically stronger than Spark's key-deletion promise, but it is a race, not an axiom. Enclave
single-active-state refusal remains the second, independent layer.

### 5.6 Rollover at epoch exhaustion (the Grove import — mandatory on-chain touch DELETED)

The TES draft forced an on-chain compaction after epoch exhaustion. TES-R instead performs an
**off-chain self-split rollover**: at m = 15 the owner (SDK, automatic, inside a transfer) co-signs a
1-in-1-out self-split SP_roll consuming the current state slot, whose child resting output hosts fresh
extension + state tiers (fresh 576-hop budget). Cost: zero on-chain; +2 pre-signed txs (~248 vB) of
*contingent* exit weight and +1 depth level. The parent is terminalized — frozen forever (see §5.10).

**Compaction becomes an optional depth-capping policy, not a liveness requirement.** Default SDK
policy: when depth > 3, the next transfer triggers a **solo compaction** = exactly V1's refresh
(112 vB, non-interactive, existing code path), reborn at depth 0, priced into that transfer's fee.
**[Amendment, TES-fixable #2 (economics review)]** the interactive MuSig2 batch coordinator is
**dropped at launch**: batching saves only 112 → ~103 vB/coin (corrected: 57.5 in + 43 out + amortized
overhead — the draft's "58 vB with output netting" was wrong, no netting exists) while interactive
dropout math is brutal (0.95^50 ≈ 7.7% clean-batch probability). Solo compaction inside `transfer()`
costs +0.005 percentage points of block space and deletes the coordinator service, its rebuild
protocol, and its privacy leak. If batching ever ships, **[Amendment, TES-fixable #3]** the fresh T'
is pre-signed against the known template output *before* broadcast, closing the recurring B5 window.

Net budget between on-chain touches (default policy): 576 × 4 depth levels ≈ **2,300 transfers per
112 vB** — and a user who tolerates depth may set the cap higher and touch the chain *never*.

### 5.7 Race analysis

| Adversary | Holds | Attack | Honest defence (keyless-tower executable) | Winner | Watching assumption |
|---|---|---|---|---|---|
| Previous owner, same epoch | T, X_m, stale S_j | Broadcast T; at +E_m broadcast X_m; wait Δ_j | Tower sees F spent (loud, on-chain); **preferred: co-op de-trigger (§5.8)**; else broadcast S_k at +Δ_k | **Honest**, ≥36-block (~6 h) CSV edge per tier + fee race | ≥1 tower reacting within a window that opens with ≥E_m + Δ_k ≥ 288 blocks (~2 days) of total notice |
| Previous owner, old epoch m′ | T, X_{m′}, old states | Broadcast T; wait E_{m′} | Co-op de-trigger, or broadcast X_m at +E_m — every epoch-m′ state's parent becomes unconfirmable | **Honest**, edge 36·(m−m′) ≥ 36 blocks | Tower awake within the edge, after ≥E_m ≥ 144 blocks of prior notice |
| Ancestor's past owner vs my sub-coin | Root chain + stale S_j at some level | Confirm T, X; broadcast stale S_j before my SP | Tower broadcasts SP at Δ_{k+1} (undercuts by ≥36); per-tier repeat | **Honest**, per-tier ≥36-block edge; **no calendar deadline exists** (V1's [17]/B6 class deleted) | Per-tier reaction; ≥144 blocks notice before ANY hostile tx is final |
| Hacked SE alone | e_n (1 of 2-of-2) | Nothing signs; can only refuse (freeze ≠ seize) | Unilateral tree pre-signed, needs nobody | **Honest, unconditionally** | None |
| Hacked SE + past owner w/ retained pre-rotation share (**B1**) | Full key of F | Fresh no-timelock spend, private relay | Race with pre-signed material; blindness blocks target selection; public counter receipts attribute | **Adversary favored — byte-identical to V1's B1**, the statechain trust unit, unchanged in kind and mitigations | Enclave deletion + attestation ops; settled coins immune |
| Hacked SE + thief, coin received PRE-hack, untouched | e_n only | Cannot produce owner's partial | 2-of-2 arithmetic | **Honest, unconditionally** (REQ-3 core case) | None |
| Trigger griefer (any past owner, anonymous) | T | Broadcast T to force cost | **Co-op de-trigger: 111 vB, coin fully restored (§5.8)** | **Honest**; attacker pays ~276 vB to cost the victim ~111 vB | SE alive; else falls to row 1 |
| Malicious/buggy tower | Bundle (all pre-signed, pays owner only) | Broadcast early / not act | — | **Honest** (early = settles to owner; inaction = no tower) | — |
| Mempool pinner | — | Pin packages | TRUC 1P1C + sibling eviction + committed base fee + P2A | **Honest** | Standard fee-bumping |

**The traded property, stated plainly**: V1's unconditional ~7-day no-watch window is exchanged for
*perpetual but alarm-driven* watching. No theft tx can become valid until ≥144 blocks (~1 day) after a
**publicly visible on-chain trigger** spends F — strictly more defender notice than V1's silent
calendar maturities and post-window minutes-scale mempool races. Missed liveness is never
confiscation-by-design: nothing expires, nothing sweeps to the operator; loss requires a real
adversary winning a telegraphed public race while every tower slept through ≥1 day of alarm. This is
exactly the race class the hard constraints tolerate. (The reviewer's "trigger + offline victim =
theft" FATAL is this row; we accept the downgrade-in-kind — no safe unwatched holding period — as the
price of deleting the rent, and answer it with cheap, prefunded, redundant towers: §7.4.)

### 5.8 Cooperative de-trigger [Amendment — imported from the Grove review; answers TES FATAL #1]

T's output pays the coin's own aggregate (A + public tweak). A fresh cooperative co-sign has **no
timelock**, and no pre-signed extension is valid before CSV E_m ≥ 144 blocks. Therefore, on any
hostile trigger: owner (or SDK automatically) + SE **key-path-spend T.out[0] immediately into a fresh
funding output F′** and rebuild T′/X′_0/S′_0 — one ~111-vB tx, confirmed unopposed inside the
≥144-block window during which no adversary tx is valid. For token carriers the de-trigger is a
colored self-transition (~155 vB, tapret). Consequences:

- **Trigger griefing collapses from FATAL to priced nuisance**: attacker pays ~276 vB (T + fee child)
  to force the victim a ~111-vB co-op re-anchor. The victim keeps off-chain-ness, the coin's ladder
  resets fresh, and the grief is fee-attributable on-chain even though T itself is anonymous.
  Damage:cost ratio ≈ 0.4 — griefing is economically losing.
- **Mass-grief saturation** (residual, quantified): 1M simultaneous triggers force ~111M vB of co-op
  responses within a ~144-block grace ≈ 77% of block space for a day — strained but survivable, and
  the attacker burns ~2.5× more. Beyond ~1M/day the response degrades to prioritized fee auction on
  the highest-value coins. If the SE is *simultaneously* dead, de-trigger is unavailable and rows 1–3
  apply per coin. This correlated scenario is the design's worst case and is stated as residual risk
  R-1 (§6).
- The de-trigger requires SE liveness — it is a UX/cost shield, never a safety dependency (the
  unilateral tree always exists).

### 5.9 Exit costs

- **Cooperative** (normal path): 1 fresh co-signed tx ≈ 111 vB, instant, batchable — unchanged.
- **Unilateral, flat coin**: 3 pre-signed txs (T+X+S ≈ 372 vB, self-relaying via committed fees) +
  0–3 P2A children (~152 vB each) in spikes → **372–828 vB**; wait = E_m + Δ_k sequential, worst
  2,160 blocks (~15 days) fresh, decreasing 36 blocks per hop/renewal. [Amendment: the draft's "1–3
  children" corrected — with committed fees the base case is 0 children; in a spike all 3.]
- **Unilateral, depth-d sub-coin**: 3 + 2d txs ≈ 124·(3+2d) vB; worst wait ≤ (d+1)·2,160 blocks —
  depth-3 ≈ 60 days. This is the honest cost of relative timelocks (Spark shares the class).
  Mitigations: cooperative exit covers the normal case; default depth cap 3; optional per-level
  geometrically shrinking E0/D0 schedules (floor 144) bound total worst wait < 2×2,160 ≈ 30 days at
  any depth, trading per-level hop budget — a shipped dial, default off (open problem O-4).
- **Token materialization**: confirm through the last colored tx (branch only, 2d+1 txs); no final
  state needed; allocation settles on the resting output.

### 5.10 RGB integration [Amendment — resolves the colored-extension contradiction found by BOTH reviews]

The TES draft's "per-hop re-signed colored defensive extension" is **deleted** — it contradicted the
signed-once rule and forked into either 54-fold budget collapse or Spark-grade enclave-trust
(accepted finding). The V2 rule set:

1. **RGB transitions anchor ONLY in signed-once transactions**: colored splits/combines (SP/CB) and
   colored self-transitions (de-trigger re-anchors, rollover self-splits). Plain T/X/S never host
   anchors and are sats-only (token-destroying if used on a carrier; structurally omitted from
   carrier watch bundles, exactly as V1).
2. **Terminal-freeze invariant**: a colored tx only ever spends outputs of **terminalized** structure.
   Terminalization (spend_budget → terminal, public receipt) happens before the colored co-sign, as
   today — and the SE **refuses renewal on terminal nodes**, so no ancestor of any RGB anchor is ever
   re-signed. Consequently **no superseded colored witnesses exist anywhere in TES-R**: the
   rgb-lib "alternative unconfirmed closings of one seal" problem from the draft's open list shrinks
   to today's already-supported un-broadcast-carrier validation (consignments carry the un-broadcast
   T/X/SP chain as witness txs — more branch, same model).
3. **Seals sit on resting outputs** of un-broadcast colored txs; token transfers remain colored splits
   minting fresh receiver pieces — token DAG depth grows per token hop exactly as in V1 (no
   regression; carrier economics = V1 carrier economics, minus all calendar deadlines).
4. **Carrier defense** = materialize the colored branch only (REQ-33 semantics minus the calendar),
   per-tier CSV head starts as §5.7 row 3. An idle received token carrier has **no materialization
   deadline, ever** — the V1 "received tokens must materialize by the root deadline" machinery is
   deleted.
5. **SE blindness fully preserved**: a colored sighash is byte-indistinguishable from a plain one;
   renewal/rollover/de-trigger sign sats-structure or owner-built colored self-transitions the SE
   never parses; consignments stay P2P (ECIES via relay). No batch coordinator exists at launch, so
   the coordinator-sees-carriers concern from review is moot.

### 5.11 Receiver verification at claim (R′ set)

Derived from public data; any deviation = reject: (R3′) F on-chain, unspent, pays A. (R4′) T spends F,
no timelock; tier outputs pay A + the public H_tag tweaks; current extension has nSequence exactly
E0 − m·δE and the new state exactly D0 − (k+1)·δ against the SE's **publicly-served counters**, with
headroom ≥ receiver policy. (R5′) SE signature count == exact expected tree size (counters endpoint) —
any hidden extra co-signed state/extension shows as a count mismatch, same linchpin as V1's R5. (R6′/
R7′) per-level branch validation + Σ-inputs terminal ancestors, now including the terminal-freeze
check for colored ancestry. (R8) RGB consignment client-validated, un-broadcast witnesses allowed.
All txs v3, committed-fee + single P2A, Σout = Σin − fee. **[Amendment, TES-fixable #6]**: hard depth
cap (reject depth > 8 regardless of policy) + claim-time validation DoS pricing.

### 5.12 Lightning latch

Transfer shape is unchanged, so the latch composes as in V1; per §5.5 the renewal co-signs bundled
into a latched transfer are gated by the same preimage — atomic with the state co-sign. No
half-renewed state is observable to the SSP (was open problem #9; now normative).

### 5.13 Watchtowers (WatchBundle v2)

{trigger, newest extension, newest state or SP chain, per-tier CSV schedule, fee-child templates} —
all pre-signed, pays only the owner, zero key material (REQ-34 preserved). `watch_pass` becomes a
small state machine: monitor F (one outpoint subscription) → on hostile trigger, request co-op
de-trigger via the owner's SDK if reachable, else broadcast X at +E_m, state/SP at +Δ, attaching P2A
children. **[Amendment, TES-fixable #4]** `watch_pass` must be package-aware (`submitpackage`, not
per-tx `transaction_broadcast_raw`) and towers hold a small funded fee wallet — keyless w.r.t. the
coin, funded w.r.t. anchors; the bundle carries fee-child templates. Funding rail: onboarding prepays
a tower fee bond sized to ~2 spike bumps (~2 × 152 vB × 50 sat/vB ≈ 15,000 sats, operator-carried and
priced into the onboarding fee per the delegation refinement). Multiple towers compose idempotently.

---

## 6. Requirement-by-requirement verification

**REQ 1 — off-chain forever / activity-scaled footprint: MET (per the refinement; near-categorical).**
Idle coins: 0 vB forever — no rent, no deadlines, no forced materializations (V1's entire
5,840 vB/coin-yr class deleted). Off-chain transitions: 576 hops per depth level, rollover is
off-chain, so hop count is **unlimited without any mandatory chain touch**; the default depth-3
compaction policy is an optional optimization costing 112 vB per ~2,300 transfers ≈ 0.05 vB/transfer,
executed by the SDK/operator inside `transfer()` and priced into that transfer's fee — the user never
acts personally. *Residual*: a hostile ex-owner can force a coin one 111-vB co-op re-anchor at ~2.5×
their own cost (R-2 below) — footprint scales with activity OR adversary spend, with the adversary
side bounded and economically losing.

**REQ 2 — no/low operator liquidity: MET.** No round liquidity (nothing expires), no denomination
pools (exact-amount Σ-conserving split/combine preserved verbatim), no leaf stock (all value is the
user's own coin). Operator money is fee-sized only: tower fee bonds (prepaid, priced into onboarding)
and P2A bump children. *Asterisk (accepted from review)*: contingent tower fee capital under a
correlated grief wave is real — ~$17/coin defended at 30 sat/vB; a 100k-coin wave ≈ $1.7M fronted.
Mitigated by committed base fees (base case needs no child), the de-trigger (defense is 111 vB co-op,
not 552 vB unilateral), and the prepaid bond rail; it is capital-at-cost, never custody or principal.

**REQ 3 — non-custody under operator hack: MET; unchanged in kind, improved in notice.** Pre-hack
coins left untouched are unconditionally safe: every spend path needs both 2-of-2 shares; the hacked
SE holds only post-rotation e_n; renewal requires the current owner's partial (the SE cannot mint a
lower-CSV extension alone); the unilateral tree is pre-signed at all times; SE refusal = freeze ≠
seize. The residual is **byte-identical B1** (SE + past owner with a retained pre-rotation share —
enclave-deletion failure), with the same mitigations (enclave, blindness blocks targeting, public
counter receipts) — and post-hack theft against watched coins now requires a public trigger +
≥144 blocks of on-chain notice instead of a minutes-scale mempool race. Honesty correction folded in:
old-epoch invalidation is race-conditional, not absolute (§5.5).

**REQ 4 — blind operator / P2P RGB: MET.** The SE signs only sighashes; colored = plain at the byte
level; consignments stay P2P; renewal/rollover/de-trigger never touch RGB payloads; anchors live
exclusively in signed-once colored txs over terminal-frozen ancestry (§5.10), so re-signing can never
invalidate an anchor — the review's contradiction is resolved by construction, and the batch
coordinator (the other REQ4 gap) does not ship. Exit-time RGB disclosure unchanged and acceptable.

**Top residual risks (stated plainly):**
- **R-1 Correlated SE-death + mass trigger grief**: with the SE dead, de-trigger is unavailable and
  every triggered coin fights per-tier races; defense capacity saturates around ~10⁵–10⁶
  simultaneous triggers/day, beyond which small coins are not worth defending at spike rates. Never
  confiscation; bounded by the attacker paying ~2.5× the defender per coin. (No V1-style 7-day exit
  stampede exists, though — untriggered coins idle safely under a dead SE forever.)
- **R-2 No unconditional no-watch window**: a received coin must be watched (delegated, keyless,
  prepaid) from the moment of receipt, forever. Alarm-driven with ≥1 day notice, but a downgrade in
  kind from V1's first-window, accepted deliberately.
- **R-3 B1 unchanged**: the statechain trust unit (enclave share-deletion) remains the floor, as in
  V1 and every statechain.
- **R-4 δ=36 head starts vs sustained congestion**: a fee spike outlasting the head start converts
  the CSV edge into a pure fee race; committed fees + P2A + funded towers are the answer, and δ is a
  dial (open problem O-2 quantifies against mainnet history before mainnet ship).
- **R-5 Deep-DAG unilateral latency**: depth-3 worst ≈ 60 days (coop exit instant); geometric
  schedules cap it ≈ 30 days at reduced budgets (O-4).
- **R-6 Enclave counter machine {level, m, k} is safety-relevant** for receiver verification; needs
  the INV-23/24-grade formal spec before ship (O-1).

---

## 7. Footprint economics

Primitives: P2TR in 57.5 / out 43 / overhead 10.5 / P2A out 13 vB; tier tx 124 vB; fee child 152 vB;
solo compaction 112 vB. All corrected figures from the economics review adopted (the draft's 58-vB
batched claim and 0.04 vB/transfer are **rejected**; no output netting exists).

**7.1 Per coin**
| | V1 | TES-R |
|---|---|---|
| Idle coin | 5,840 vB/yr | **0 vB/yr** |
| Active, 10 tx/day, default depth-3 policy | 5,840 vB/yr (rent dominates) | 3,650 hops/yr ÷ ~2,300/compaction ≈ 1.6 × 112 ≈ **~180 vB/yr** (≈0.05 vB/transfer) |
| Active, no-compaction policy | n/a | **0 vB/yr** on-chain; +~248 vB contingent exit weight per 576 hops (~0.43 vB/hop, paid only if exiting unilaterally) |

**7.2 System, 1M-vB blocks, 52,560 blocks/yr, 10% of coins active at 10 tx/day (aggressive):**
- **1M coins**: idle 900k → 0; active 100k × ~180 ≈ 18M vB/yr ≈ **342 vB/block ≈ 0.034%** of block
  space (V1: 111,111 vB/block ≈ 11.1% — a **~320× reduction**). Plus churn: 20%/yr deposit+exit at
  ~111 + ~372–828 vB adds ~0.2–0.4% — present in every design including Spark.
- **10M coins**: ≈ **0.34%** (+ churn) — vs V1's physically impossible 111%. **100M coins ≈ 3.4%** —
  the "hundreds of millions TVL" target is reachable if value concentrates realistically.
- Fully idle custody TVL is exactly free at any scale.

**7.3 The fee model (per the delegation refinement)**
- Onboarding fee: deposit tx share (~58–111 vB) + tower fee bond (~15k sats, actuarially sized) +
  tower infra (~$0.001/coin-yr: 10M outpoint subscriptions ≈ one indexed node ≈ $3–6k/yr, ~15 GB
  bundles). No idle-rent prefund line exists — the item that made V1 unpriceable ($292/coin for 5
  idle years) is gone.
- Per-transfer fee: carries the amortized compaction share (≈0.05 vB) + committed-fee top-ups +
  renewal (0 vB, 2 co-signs). Users never see or perform a maintenance action.
- Grief insurance: a small per-transfer tower-insurance component funds de-trigger re-anchors
  (~111 vB each, damage:attacker-cost ≈ 0.4).

**7.4 Tower economics**: monitoring is cheap and delegable (above); the priced item is contingent
defense capital (R-1), bounded per coin (≤ 3 × 152 vB × spike rate) and prefunded via the bond. A
tower's worst action remains harmless (everything it holds pays only the owner).

---

## 8. What we did NOT choose and why

### 8.1 Grove (shared-UTXO factories) — REJECTED (factory primitive); SALVAGED (trigger-ladder half)
**Fatal (accepted from both reviews)**: the factory root is an n-of-n of operator-chooseable,
never-rotating signers. A Sybil-originated factory — operator fills 64 slots with its own keys, funds
them itself, distributes leaves to real victims who pass every R-check — lets the operator (or
hacked-SE + original cohort, **B1-F**) fresh-spend F_root via private relay and confiscate all 64
coins in one confirmation, *including received-and-untouched coins*, destroying every RGB allocation
below. No timelock gates a fresh signature; the victims' pre-signed tree loses a race they cannot see.
This deletes V1's one absolute guarantee (self-deposited never-transferred coin safe vs SE alone) and
breaks REQ3's "preserve or improve" — unfixably, because the amortization *is* the shared multisig,
which cannot include current holders and whose signer independence Bitcoin cannot prove.
**Cost-benefit inversion (accepted)**: amortization is input-bound — ~101.5 vB/user real (with change
outputs) vs ~111 solo, a one-time ~10–50 vB saving — while the ceremony succeeds with p ≈ 0.98⁶⁴ ≈ 27%
per attempt and free-join griefing stalls onboarding. The factory's Primitive 2 (CSV trigger ladders,
co-op de-trigger, self-split renewal, tower fee bond) delivers the entire footprint win standalone —
it is inside TES-R. Factories may return under CTV (covenant-committed trees have no cohort to
collude, deleting B1-F) — §10.

### 8.2 Evolved absolute ladder (C-90) — REJECTED
**Fatal (self-admitted + confirmed by both reviews)**: rent is structurally O(coins × time/initlock):
C-90 = 505 vB/coin-yr idle → 0.96% of block space at 1M coins, **9.6% at 10M, 96% at 100M** — an
11.5× postponement of the complaint, not a cure. **Fatal #2**: the pre-signed refresh chains re-anchor
flat coins only; an idle *received sub-coin* is still bounded by its root deadline, and extending it
requires either materializing (footprint spike) or all-descendant re-signing (multi-party liveness) —
"unsolved in general" per its own open problems, i.e. REQ1 fails for exactly the split/combine feature
REQ2 forces us to keep. Additional: 90-day unilateral/SE-death wait (13× worse than V1, correlated
with bank-run moments); actuarial fee-model insolvency in the tail (13-month frozen fees, no forward
market); (k+1)× enclave share blast radius on an unattested SGX. Not even useful as a bridge: TES-R
migration is a single re-anchor per coin (§9), self-funding within one week of avoided V1 rent.
Salvaged ideas: P2A-on-everything, tower-executed maintenance (made off-chain), conveyed-locktimes
(moot — V2 has no calendar deadlines to convey; audit [17] closes by deletion).

### 8.3 TES as drafted — AMENDED, not adopted verbatim
Accepted and folded: renewal-auth replay fix; committed-fee restoration; package-aware funded towers;
latch-gated renewal; depth cap + DoS pricing; nostr-published constants; batch coordinator dropped;
compaction arithmetic corrected (103 not 58 vB); exit-cost disclosure corrected (828 vB / 3 children
worst); forced rollover before the floor; RGB colored-extension deleted in favor of terminal-freeze;
mandatory compaction replaced by off-chain rollover; co-op de-trigger added. Rejected findings: the
"trigger-grief = FATAL" classification (defanged to priced nuisance by the de-trigger); the
"offline = theft" classification (it is the constraint-tolerated race class, with more notice than
any alternative).

---

## 9. Migration path from V1

**Phase 0 — ship, no behavior change**: server: generalized counters {level, m, k, total_sigs},
`POST /renew/init` (challenge-nonce rail), config + nostr record; SDK: TES-R builders in
`transaction.rs` (CSV tiers, v3, committed fee + P2A, public H_tag tweaks), WatchBundle v2 +
package-aware `watch_pass` with fee wallet, R′ receiver checks behind a version tag in the transfer
message (receivers handle both ladder types during migration). Enclave: **cryptographically
unchanged** (blind MuSig2 + one-shot nonces suffice); only the counter state machine needs the
INV-23/24-grade spec (O-1) — no SGX rebuild required for launch, decoupling from mainnet-audit [13].

**Phase 1 — new deposits are TES-R**: T co-signed at deposit detection (same B5 onboarding window
semantics as today's tx1 moment).

**Phase 2 — V1 coin conversion**: one final on-chain re-anchor per coin (exactly today's 112-vB
refresh, `refresh_sponsored` rail reusable), reborn as TES-R at depth 0. 1M coins × 112 vB = 112M vB
one-time ≈ one week of V1's avoided rent — **the migration pays for itself within ~7 days**; schedule
over weeks. Un-broadcast V1 sub-coin DAGs migrate at their next transfer (auto-refresh hook) or
before their V1 root deadline (existing auto_exit_due covers stragglers until converted).

**Phase 3 — delete V1 calendar machinery**: `exit_deadline_block`, `auto_exit_due` scheduling,
refresh-token-slot economics, B6/[17] margin arithmetic — all removed once no V1 coins remain. RGB:
no protocol change (anchoring rules of §5.10 are a constraint tightening, enforced at co-sign time);
rgb-lib needs only the existing un-broadcast-witness validation, now exercised over deeper chains —
dedicated adversarial suite required (O-3).

**Test plan**: extend SDK_E2E to — renewal races (renew vs transfer vs split interleavings, INV-23/24
analog), co-op de-trigger under hostile trigger, per-tier tower defense with package relay, rollover +
depth-cap compaction, colored branch materialization with terminal-freeze, latch-gated renewal
atomicity, mixed V1/V2 estates.

---

## 10. Open problems & future work

- **O-1 (blocking)**: formal spec + audit of the enclave counter machine {level, m, k} — receivers
  verify tree shape against it; a missed interleaving is a double-spend. INV-23/24-grade treatment
  before mainnet.
- **O-2 (blocking dial)**: δ/δE vs sustained mainnet congestion — quantify head-start survival
  against 2023–24 spike history; δ=36 is the provisional default, with the budget table of §5.2 as
  the trade space.
- **O-3**: rgb-lib adversarial suite for deep un-broadcast witness chains (terminal-freeze removes
  superseded-witness ambiguity, but depth and DoS bounds need tests).
- **O-4**: deep-DAG unilateral latency — geometric per-level E0/D0 schedules (worst wait <30 days at
  any depth) vs per-level hop budgets; ship as a dial with a chosen default.
- **O-5**: tower fee-bond actuarial sizing + refund/receipt rail (operator-signed per-bump rebates,
  reusing the refresh_sponsored rail shape).
- **O-6**: mass-grief saturation modeling (R-1) — prioritization policy for de-trigger waves,
  per-coin-value triage, and whether a standing "grief bond" priced into transfers should scale with
  coin count.
- **O-7**: relay-policy dependence — v3/P2A/1P1C are policy, not consensus; maintain the
  miner-direct-submission fallback and re-verify at each Core release.
- **O-8**: batched compaction (post-launch): non-interactive constructions only (the interactive
  MuSig2 coordinator stays dead); pre-signed T′ against the template before broadcast is mandatory if
  revived.
- **Covenant future work**: **CTV** would let one on-chain output commit to the whole T/X/S tree (and
  make factories safe — no signer cohort to collude, deleting B1-F — reviving Grove-style shared-UTXO
  amortization for onboarding, the natural Phase-4). **APO/ANYPREVOUT** would make state txs
  rebindable (true Eltoo): the trigger tier disappears, trigger-griefing with it, and renewal becomes
  a pure local re-sign. **CSFS** would allow delegated de-trigger without SE liveness. None are
  required: TES-R ships on today's Bitcoin.

---

*Adversarial findings ledger: TES review-1 FATAL#1 defanged (§5.8), FATAL#2 accepted-as-tolerated
(§5.7); TES review-1 fixables 1–7 folded; TES review-2 corrections (compaction 103 vB, exit 828 vB,
δ-sensitivity, policy dependence, m≤floor-2, migration self-funding) folded; RGB fork resolved by
deletion + terminal-freeze (§5.10); Grove review-2 salvage (trigger-ladder standalone, co-op
de-trigger, tower bond, onboarding-fee framing) adopted; Grove factory + C-90 rejected with reasons
(§8).*
