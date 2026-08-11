# Utexo TES-R — renewal & invalidation design (the shipped protocol)

> ## ⚠️ Direction of travel: ONE COIN TYPE
>
> The status block below is about the *protocol*; this is about the *coin shapes*. Today there are
> two — *laddered* (TES-R) and *un-laddered* (RGB carriers and un-broadcast split sub-coins). **That
> is a transitional state, not the target architecture.** The decided direction is a single coin
> type; the un-laddered shape is being removed, not kept.
>
> The mechanism is **CTES-R** — colour every tier so an RGB carrier can be laddered, retiring
> terminal-freeze. Its gate passed against the live stack ([CTESR-GATE.md](CTESR-GATE.md)) and its
> foundation has landed (the `payload_vout` migration, the coloured tier builder, per-tier seal
> blinding). **The colouring itself is not yet wired.** Until it is, everything below about the
> un-laddered shape remains accurate as-built.
>
> In particular, the pre-TES-R **baseline** column used in the comparison tables is a historical
> measuring stick and stays. The un-laddered **shape** is a different thing, and it is going away.
>
> Reaching one coin type also requires porting `verify_bundle` to wasm/JS and Kotlin: the nodejs and
> web clients currently *refuse* any laddered coin. Background:
> [COLORED-FORWARDING.md](COLORED-FORWARDING.md).

**Status: SHIPPED — this is the protocol.** TES-R is not a proposal or a lane any more: `claim()`
establishes a trigger/extension/state ladder for **every fresh confirmed ROOT coin, unconditionally**.
The `deposit_protocol_version` field and the `UTEXO_PROTOCOL_DEFAULT` escape hatch that could opt a
deposit back into the flat pre-TES-R shape are **deleted**, and zero tests pin the old lane. This
document supersedes the calendar-refresh model described in `learn/invalidation-deep-dive.md` §1b/§6.

**One protocol, two coin SHAPES — both current.** "Un-laddered" is a *shape*, not a legacy lane:
- **Laddered**: every plain deposit — trigger T → extension X_m → state S, relative CSV, un-broadcast.
- **Un-laddered**: an **RGB carrier** is deliberately never laddered (a plain tier spend would destroy
  the allocation — the terminal-freeze rule of §5.10; pinned by sdk52), and a **split sub-coin whose
  funding is un-broadcast** cannot root a trigger (B0 — a trigger over an unconfirmed prevout is
  unbroadcastable). These keep the signed-once backup and transfer by backup-chain handover. That path
  is **load-bearing for RGB tokens**, not dead code — and it keeps the calendar machinery that
  §5.2 / §5.10 / §9-Phase-3 below expected to disappear (see the **[Shipped]** corrections there).

**How to read the rest.** §1 and §5–§7 are normative and describe running code. §2, §3, §8 and §9
compare against the **pre-migration absolute-ladder baseline** (the pre-TES-R design); that baseline
is retained as the *rationale for this design* and is no longer a shipped option. Sections where
implementation amended the design are tagged **[Shipped]** in place: §5.4 (in-ladder split +
first-class children), §5.10.4
and §5.2 (carrier deadlines survive on the un-laddered shape), §5.12 (the latch is a HODL-invoice
latch), §9 (migration complete).

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

## 2. Why the absolute ladder cannot scale (the block-space wall)

*(Historical rationale for the whole design. The absolute-ladder deposit path no longer exists — no
deposit is ever anchored to a decrementing absolute nLockTime ladder. "The pre-TES-R design" below
means that pre-migration baseline.)*

The pre-TES-R design anchored every coin's decrementing nLockTime ladder to an **absolute** deposit
height. Staying alive required an on-chain re-anchor (refresh, 112 vB) per coin per ~initlock
(~1,008 blocks ≈ 7 days), regardless of activity:

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

*(The **Baseline** column is the pre-TES-R design of §2 — every coin anchored to a decrementing
absolute nLockTime ladder from its deposit height, with a calendar deadline and idle rent. It is kept
because every number in this design is measured against it, and it is not a shipped alternative — the
shipped protocol is the last column.)*

| | **Baseline (pre-TES-R)** | **Spark** | **Ark** | **SuperScalar** | **TES-R (shipped)** |
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
  the baseline's one absolute guarantee (self-deposited never-transferred coin safe against SE alone),
  which REQ3 obliges us to preserve. Both reviewers converged on this; one additionally showed the
  factory buys almost nothing (input-bound amortization: ~101.5 vB/user real vs ~111 solo, a one-time
  ~10–50 vB saving) while its **Primitive 2
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

### 5.1 What the redesign did not touch (carried over unchanged)

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

**[Shipped]** these are `TesrParams::mainnet()` in `lib/src/tesr.rs` verbatim (d0 1440 / δ 36 /
d_floor 144, e0 720 / δE 36 / e_floor 144, m_max 15, committed fee 2 sat/vB); a `TesrParams::regtest()`
preset (24/6/6, 12/3/3, m_max 2) exists only so a full lifecycle fits in a test's mining budget.
`sdk44` pins the schedule arithmetic (`state_csv`/`ext_csv`/`needs_renewal`/`needs_rollover`).

δ = 36 (≈6 h) rather than the TES draft's 24 (≈4 h): both economics reviews flagged the head start as
the single parameter everything stands on, and mainnet has sustained >4 h full-block spikes. The
budget sensitivity is stated honestly: δ = 24 → 1,350 hops/level; **36 → 576**; 72 → 162; 144 → 45.
Because rollover (§5.6) is off-chain, even conservative δ only trades exit weight, never chain rent.

**The core property**: all three tiers are un-broadcast; BIP-112 CSV ticks only after the parent
confirms; T has no timelock. So **nothing anywhere matures until someone broadcasts T on-chain**. An
idle coin — and an entire idle split/combine DAG — never ages. No calendar deadlines, no refresh rent,
no root-deadline materializations. The whole audit-[17]/B6 class (deposit-anchored deadline
arithmetic) is deleted **for laddered coins**.

**[Shipped correction]** it is *not* deleted wholesale: the un-laddered shape (RGB carriers, §5.10;
sub-coins over un-broadcast funding, B0) still rests on the signed-once **absolute-locktime** backup,
so it still has a root deadline and still needs materialization before it. `auto_exit_due` and
`exit_deadline_block` therefore **survive, scoped to that shape** (`SdkConfig::auto_exit`, default on,
margin 288 blocks), and are exercised live by sdk34 (carrier watchtower materializes before the
deadline) and sdk32 (the residual clawback window on an un-laddered carrier). A laddered coin has no
deadline for them to act on.

### 5.3 Fees [Amendment: TES-fixable #2 + Grove reviewer's pinning finding, merged]

Every pre-signed tx (T, X, S, SP, CB) is **nVersion=3 (TRUC)** and carries:
1. a **small committed fee** drawn from the coin (target ~2 sat/vB at signing; Σout = Σin −
   fee_committed) so the base case relays and confirms standalone — restoring the self-funding
   property the signed-once backup chain always had, and removing the chicken-and-egg the review
   found in the all-zero-fee draft; and
2. a **240-sat P2A anchor** (OP_1 0x4e73, anyone-can-spend) so any party — owner, keyless tower,
   operator — attaches a live-rate fee child (~152 vB: 41 P2A-in + 57.5 funding-in + 43 change + 10.5)
   during spikes.

TRUC's 1P1C topology + sibling eviction gives pinning resistance (each tier confirms before the next
is valid, so no long vulnerable chains); v3+P2A is relay *policy*, not consensus — the
miner-direct-submission fallback is documented, the same bet Lightning anchors make [Amendment:
economics-fixable "state the policy dependence"]. The committed fee is small and bounded-stale (it is
a floor, not the whole fee), so the backup-fee model's 2–13 sat/vB stranded-reserve pathology does not
return in force.

### 5.4 Splits, combines, sub-coins

A split SP is a state-tier tx: spends X_m.out[0] at nSequence Δ_{k+1} (one decrement below the
splitter's own retained state), N child resting outputs with **exact amounts, Σout = Σin −
fee_committed**, + P2A. Parent terminalized at the SE before co-signing, exactly as today. Each child
resting output hosts its own extension + state tiers — **no trigger needed**: SP is itself
un-broadcast, so nothing ticks until it confirms. A combine CB carries per-input nSequence = that
input's Δ (BIP-112 is per-input); Σ-inputs terminal-ancestor verification (R7) unchanged. Child coins
get fresh slots via the free derived-token path (REQ-35 analog).

**[Shipped] — the in-ladder split, and why it is the only split.** A split is a **state tier** (`SP`
spending `X_m.out[0]`), i.e. a *descendant* of T, never a rival for F. That is what closes the theft
vector that twice reverted the default: a past owner's retained no-timelock trigger has nothing to
race, because the split does not compete for the funding outpoint. Attack-proven by **sdk58** (11
adversarial cases, all REJECT) and **sdk59** (end-to-end split payment), with the bundle/backup-chain
adversarial cases in **sdk54/sdk55**.

**[Shipped] — admission floor (defect D1, fixed).** A child is not a bare output: `establish_child`
hangs the child's OWN extension + state tiers off `SP.out[j]`, each burning committed fee + P2A, and
the final state output must still clear dust. The guard originally used the old backup-fee floor
(442 sat at 2 sat/vB), so a piece in the 442..1306 window **terminalized the parent and then failed**,
stranding it to unilateral-exit-only. The floor is now `mercurylib::tesr::min_child_value(fee_rate,
dust)` (1306 sat at 2 sat/vB), checked **before** the parent is terminalized.

**[Shipped] — received children are FIRST-CLASS** (`CHILDREN.md`). The child claim completes
the standard SE key handover: `A_child` is invariant across the rotation (`sender_share + SE_old ==
receiver_share + SE_new`), which is exactly what keeps the pre-signed child exit chain valid, while the
sender is permanently locked out (auth rotated). A child is then payable onward off-chain — **whole**
via `child_retransfer`, or **split** via `child_in_ladder_pay` (a depth-2 `ancestors` chain). Each hop
costs exactly **one co-signature** and discloses exactly **one superseded state**, which the receiver's
census counts and proves out-raced. Evidence: **sdk60** (alice→bob→carol whole re-transfer, the funding
outpoint unspent throughout) and **sdk17** (partial second hop). The **child is deliberately not
terminalized** — the census closes any pre-conveyance rival and the handover closes every later one;
the one exception is the Lightning-latched piece (§5.12), which stays terminalized.

**[Shipped] — B0, root-only laddering.** `claim()` ladders a coin only when its funding output is a
**confirmed on-chain root**. A sub-coin whose funding is an un-broadcast split output cannot root a
trigger (the trigger would have no prevout to spend, and a v3 tier cannot relay over an unconfirmed v2
parent), so such a coin travels the un-laddered shape instead — checked fail-closed against electrum,
never inferred.

### 5.5 Renewal (pure off-chain; the common case)

When Δ_{k+1} would fall below D_floor, the SDK runs renewal inside `transfer()` — replacing the
pre-TES-R on-chain auto-refresh with **zero on-chain bytes**:

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
≥36-block-edge race after a public trigger. That is strictly stronger than the baseline's post-window
position and categorically stronger than Spark's key-deletion promise, but it is a race, not an
axiom. Enclave single-active-state refusal remains the second, independent layer.

**[Shipped] evidence**: **sdk51** — someone else spends F (a prior owner racing a stale state, or a
griefer); the owner runs only `defend_ladders()` and the strictly-lowest-CSV current state matures
first, so the funds land at the owner's key. **sdk40 PART 1** — BIP-68 is enforced by real consensus
(X is rejected before E confirmations of T, S before D of X) and nothing ages while un-broadcast.
**sdk40 PART 2** — a stale ladder is killed outright at the consensus level (X′ can never confirm once
its prevout is gone). **sdk43** — unbounded off-chain renew → rollover → renew, zero on-chain bytes,
then a unilateral exit through the whole deep chain. **sdk42** — the full laddered lifecycle.

### 5.6 Rollover at epoch exhaustion (the Grove import — mandatory on-chain touch DELETED)

The TES draft forced an on-chain compaction after epoch exhaustion. TES-R instead performs an
**off-chain self-split rollover**: at m = 15 the owner (SDK, automatic, inside a transfer) co-signs a
1-in-1-out self-split SP_roll consuming the current state slot, whose child resting output hosts fresh
extension + state tiers (fresh 576-hop budget). Cost: zero on-chain; +2 pre-signed txs (~248 vB) of
*contingent* exit weight and +1 depth level. The parent is terminalized — frozen forever (see §5.10).

**Compaction becomes an optional depth-capping policy, not a liveness requirement.** Default SDK
policy: when depth > 3, the next transfer triggers a **solo compaction** = exactly the pre-TES-R
on-chain re-anchor (`refresh`: 112 vB, non-interactive, existing code path), reborn at depth 0,
priced into that transfer's fee.
**[Amendment, TES-fixable #2 (economics review)]** the interactive MuSig2 batch coordinator is
**dropped at launch**: batching saves only 112 → ~103 vB/coin (corrected: 57.5 in + 43 out + amortized
overhead — the draft's "58 vB with output netting" was wrong, no netting exists) while interactive
dropout math is brutal (0.95^50 ≈ 7.7% clean-batch probability). Solo compaction inside `transfer()`
costs +0.005 percentage points of block space and deletes the coordinator service, its rebuild
protocol, and its privacy leak. If batching ever ships, **[Amendment, TES-fixable #3]** the fresh T'
is pre-signed against the known template output *before* broadcast, closing the recurring B5 window.

Net budget between on-chain touches (default policy): 576 × 4 depth levels ≈ **2,300 transfers per
112 vB** — and a user who tolerates depth may set the cap higher and touch the chain *never*.

**[Shipped]** rollover runs unattended inside `transfer()`: **sdk43** drives renew → rollover → renew
past epoch exhaustion and shows the funding outpoint untouched. The solo-compaction path is the
re-anchor of **sdk30** (both fee models). Defect **D2** was here: `refresh_sponsored` sized the
operator's rebate into the dead 442..1306 window of D1, so a *sponsored* refresh failed **after** the
user had already paid the on-chain fee; the rebate is now `max(fee + DUST_LIMIT, min_child_value)` and
the operator absorbs the difference.

### 5.7 Race analysis

| Adversary | Holds | Attack | Honest defence (keyless-tower executable) | Winner | Watching assumption |
|---|---|---|---|---|---|
| Previous owner, same epoch | T, X_m, stale S_j | Broadcast T; at +E_m broadcast X_m; wait Δ_j | Tower sees F spent (loud, on-chain); **preferred: co-op de-trigger (§5.8)**; else broadcast S_k at +Δ_k | **Honest**, ≥36-block (~6 h) CSV edge per tier + fee race | ≥1 tower reacting within a window that opens with ≥E_m + Δ_k ≥ 288 blocks (~2 days) of total notice |
| Previous owner, old epoch m′ | T, X_{m′}, old states | Broadcast T; wait E_{m′} | Co-op de-trigger, or broadcast X_m at +E_m — every epoch-m′ state's parent becomes unconfirmable | **Honest**, edge 36·(m−m′) ≥ 36 blocks | Tower awake within the edge, after ≥E_m ≥ 144 blocks of prior notice |
| Ancestor's past owner vs my sub-coin | Root chain + stale S_j at some level | Confirm T, X; broadcast stale S_j before my SP | Tower broadcasts SP at Δ_{k+1} (undercuts by ≥36); per-tier repeat | **Honest**, per-tier ≥36-block edge; **no calendar deadline exists** (the pre-TES-R [17]/B6 class deleted) | Per-tier reaction; ≥144 blocks notice before ANY hostile tx is final |
| Hacked SE alone | e_n (1 of 2-of-2) | Nothing signs; can only refuse (freeze ≠ seize) | Unilateral tree pre-signed, needs nobody | **Honest, unconditionally** | None |
| Hacked SE + past owner w/ retained pre-rotation share (**B1**) | Full key of F | Fresh no-timelock spend, private relay | Race with pre-signed material; blindness blocks target selection; public counter receipts attribute | **Adversary favored — byte-identical to the pre-TES-R B1**, the statechain trust unit, unchanged in kind and mitigations | Enclave deletion + attestation ops; settled coins immune |
| Hacked SE + thief, coin received PRE-hack, untouched | e_n only | Cannot produce owner's partial | 2-of-2 arithmetic | **Honest, unconditionally** (REQ-3 core case) | None |
| Trigger griefer (any past owner, anonymous) | T | Broadcast T to force cost | **Co-op de-trigger: 111 vB, coin fully restored (§5.8)** | **Honest**; attacker pays ~276 vB to cost the victim ~111 vB | SE alive; else falls to row 1 |
| Malicious/buggy tower | Bundle (all pre-signed, pays owner only) | Broadcast early / not act | — | **Honest** (early = settles to owner; inaction = no tower) | — |
| Mempool pinner | — | Pin packages | TRUC 1P1C + sibling eviction + committed base fee + P2A | **Honest** | Standard fee-bumping |

**[Shipped] where each row is exercised**: rows 1–3 (stale state / old epoch / ancestor race) —
**sdk51** (a watchtower defends against a hostile trigger end-to-end) and **sdk40 PART 2** (the stale
state dies at consensus); the honest defence's pre-signed material — **sdk50** (unilateral exit
through the full tier chain) and **sdk45** (the keyless bundle drives it with no key material);
adversarial rejection of forged/hidden structure — **sdk58** (11 cases), **sdk54**, **sdk55**. Rows 4–6
(hacked SE) are arithmetic properties of 2-of-2, not test-observable beyond the co-sign gates
(**sdk56/sdk57**, retry-idempotence and owner-share binding). Row 3's "no calendar deadline exists"
holds for a **laddered** sub-coin; the un-laddered shape keeps its root deadline (§5.10.4).

**The traded property, stated plainly**: the pre-TES-R design's unconditional ~7-day no-watch window
is exchanged for *perpetual but alarm-driven* watching. No theft tx can become valid until ≥144 blocks
(~1 day) after a **publicly visible on-chain trigger** spends F — strictly more defender notice than
that design's silent calendar maturities and post-window minutes-scale mempool races. Missed liveness
is never confiscation-by-design: nothing expires, nothing sweeps to the operator; loss requires a real
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

**[Shipped]** built by `mercurylib::tesr::build_detrigger` / co-signed by `cosign_detrigger`, and
proven end-to-end by **sdk40 PART 2**: a griefer broadcasts T′, the owner's fresh no-timelock spend of
T′.out[0] confirms immediately, and X′ can never confirm afterwards even past E blocks. The colored
(~155 vB tapret) variant and the mass-grief prioritization policy (R-1 / O-6) are not test-covered.

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

**[Shipped]** the unilateral path is driven end-to-end by **sdk50** (and by the keyless tower in
**sdk45**). Defect **D4** was here: a *child* routed to a unilateral exit was booked `WITHDRAWING`
even though a unilateral exit produces no withdrawal transaction, so status polling errored forever —
the child now reports the unilateral shape it actually has.

### 5.10 RGB integration [Amendment — resolves the colored-extension contradiction found by BOTH reviews]

The TES draft's "per-hop re-signed colored defensive extension" is **deleted** — it contradicted the
signed-once rule and forked into either 54-fold budget collapse or Spark-grade enclave-trust
(accepted finding). The rule set:

1. **RGB transitions anchor ONLY in signed-once transactions**: colored splits/combines (SP/CB) and
   colored self-transitions (de-trigger re-anchors, rollover self-splits). Plain T/X/S never host
   anchors and are sats-only (token-destroying if used on a carrier; structurally omitted from
   carrier watch bundles, exactly as the token-destroying plain backup always has been).
2. **Terminal-freeze invariant**: a colored tx only ever spends outputs of **terminalized** structure.
   Terminalization (spend_budget → terminal, public receipt) happens before the colored co-sign, as
   today — and the SE **refuses renewal on terminal nodes**, so no ancestor of any RGB anchor is ever
   re-signed. Consequently **no superseded colored witnesses exist anywhere in TES-R**: the
   rgb-lib "alternative unconfirmed closings of one seal" problem from the draft's open list shrinks
   to today's already-supported un-broadcast-carrier validation (consignments carry the un-broadcast
   T/X/SP chain as witness txs — more branch, same model).
3. **Seals sit on resting outputs** of un-broadcast colored txs; token transfers remain colored splits
   minting fresh receiver pieces — token DAG depth grows per token hop exactly as it did before the
   migration (no regression; carrier economics are unchanged from the pre-TES-R design — **including
   its calendar deadlines**, per the correction in 4 below; the draft wrote "minus all calendar
   deadlines" and that was wrong).
4. **Carrier defense** = materialize the colored branch only (REQ-33 semantics), per-tier CSV head
   starts as §5.7 row 3. **[Shipped correction — the draft over-claimed here]**: because rule 1 forbids
   laddering a carrier at all, a carrier never gains the CSV tiers that would delete its calendar. It
   keeps the signed-once **absolute-locktime** backup, so a received token carrier **does** still have
   a root-deadline materialization duty, and REQ-33's `auto_exit_due` machinery is retained for exactly
   this shape (default on; sdk34 materializes before the deadline, sdk32 documents the residual
   clawback window if nobody does). Only *laddered* coins have no deadline. This is the price of
   terminal-freeze and is deliberate, not a migration leftover.
5. **SE blindness fully preserved**: a colored sighash is byte-indistinguishable from a plain one;
   renewal/rollover/de-trigger sign sats-structure or owner-built colored self-transitions the SE
   never parses; consignments stay P2P (ECIES via relay). No batch coordinator exists at launch, so
   the coordinator-sees-carriers concern from review is moot.

**[Shipped] evidence**: **sdk52** pins rule 1, the invariant everything else here rests on — in one
wallet the plain coin carries a ladder and the token carrier carries **none**, and an off-chain RGB
transfer still settles. **sdk32** pins the terminal-freeze semantics over time (carrier terminal at the
SE, un-laddered, no fresh co-signed sweep possible) and the residual clawback window of point 4. The
RGB suites (`rgb*`, `ta*`, `tb*`) run over the shipped protocol unchanged.

### 5.11 Receiver verification at claim (R′ set)

Derived from public data; any deviation = reject: (R3′) F on-chain, unspent, pays A. (R4′) T spends F,
no timelock; tier outputs pay A + the public H_tag tweaks; current extension has nSequence exactly
E0 − m·δE and the new state exactly D0 − (k+1)·δ against the SE's **publicly-served counters**, with
headroom ≥ receiver policy. (R5′) SE signature count == exact expected tree size (counters endpoint) —
any hidden extra co-signed state/extension shows as a count mismatch, same linchpin as R5 in the
pre-TES-R receiver set. (R6′/R7′) per-level branch validation + Σ-inputs terminal ancestors, now
including the terminal-freeze check for colored ancestry. (R8) RGB consignment client-validated,
un-broadcast witnesses allowed.
All txs v3, committed-fee + single P2A, Σout = Σin − fee. **[Amendment, TES-fixable #6]**:
claim-time validation DoS pricing.

> **[D7/D25] The "hard depth cap of 8" that used to be specified here NEVER EXISTED IN CODE, and has
> been deleted rather than implemented.** The real cap is *derived*, not a literal:
> `max_split_depth(base, per_level, epoch_blocks) = 1 + (epoch − base_wait)/level_wait`
> (`lib/src/transfer/receiver.rs:768-779`), enforced by `enforce_split_depth_cap_shaped`
> (`clients/libs/rust/src/tesr.rs:5210`). It therefore MOVES WITH THE NETWORK PROFILE, which is the
> whole point — and is why the profile itself is now explicit per network rather than falling through
> to a toy schedule. Publishing a fixed 8 would have contradicted the code on every profile at once.

**[Shipped]** the R′ set is enforced at claim: **sdk46** (R5′ against the *real* SE counter — the SE
increments by exactly the tier count, `verify_bundle` accepts the true count and rejects a hidden extra
signature), **sdk47** (R′ across a transfer), **sdk54** (adversarial `verify_bundle`), **sdk55**
(backup-chain adversarial), **sdk58** (11 adversarial in-ladder-split cases). The census generalizes to
N hops for first-class children (`se_num_sigs == flat_backups + Σ conveyed_tiers`): each hop discloses
exactly one superseded state, so an undisclosed rival shows up as a count mismatch — **sdk60**,
**sdk17**.

### 5.12 Lightning latch **[Shipped — it is a HODL-invoice latch]**

Transfer shape is unchanged, so the latch composes over the ladder; per §5.5 the renewal co-signs
bundled into a latched transfer are gated by the same preimage — atomic with the state co-sign. No
half-renewed state is observable to the SSP (was open problem #9; now normative).

The shipped latch is the **HODL-invoice** construction (`LIGHTNING.md`, which supersedes the
adaptor-signature sketch that preceded it — PTLCs do not exist in the routing network). Lightning
works **both directions on the ladder**: **sdk63** (pay), **sdk64** (receive), **sdk67** (receive out
of an in-ladder split), **sdk65** (non-exact pay), with the failure/rollback paths in **sdk68** (exact
reclaim after a pay failure) and **sdk66** (non-exact rollback); **sdk53** pins the latch guard.

**This is the one case that stays terminalized.** The LN-latched piece sits unclaimed past the
pending-transfer lock's window (the SSP settles on its own schedule), so the temporary lock cannot be
what protects it — the piece is terminalized instead, a permanent lockout. Every other in-ladder child
is *not* terminalized (§5.4): it is protected by the census plus the key handover.

Defect **D3** was here: the one-call Lightning PAY API minted its input via `ensure_exact_coin` and so
refused **every laddered coin** — i.e. every coin the protocol produces. It now pays from the ladder
directly.

### 5.13 Watchtowers (the keyless TES-R watch bundle)

{trigger, newest extension, newest state or SP chain, per-tier CSV schedule, fee-child templates} —
all pre-signed, pays only the owner, zero key material (REQ-34 preserved). `watch_pass` becomes a
small state machine: monitor F (one outpoint subscription) → on hostile trigger, request co-op
de-trigger via the owner's SDK if reachable, else broadcast X at +E_m, state/SP at +Δ, attaching P2A
children. **[D31] WHAT A KEYLESS TOWER CANNOT DO — normative.** A keyless tower can watch `F`, and can
broadcast the pre-signed tiers **at their committed fee**. It **cannot fee-bump them.** A CPFP child
spending the P2A anchor requires an input the tower does not hold and a signature it cannot make, so
if the mempool's floor rises above a tier's committed rate, a keyless tower has no action available:
`min relay fee not met, 200 < 423` is a refusal it cannot answer. This is a **stated limit of the
protocol, not an implementation gap** — an implementer must not read "delegable, keyless watching"
as implying spike-time rescue.

Nor does the anyone-can-spend anchor supply one. Rescuing a 240-sat anchor was measured to need a
**180 330-sat child** (WP1) — about **900×** the anchor's value — so while anyone *may* bump it,
nobody without a stake in the coin has a reason to. "Anyone-can-spend" is a permission, not an
incentive.

**[D31] WHO BUMPS, THEN: the owner.** The party that funds a CPFP package is the **coin owner**, from
their own wallet — the only party holding both a spendable UTXO and a motive. The consequence is
explicit: **the guarantee during a fee spike is "the owner is online"**, which is precisely the
condition a watchtower otherwise removes. The protocol does not promise otherwise.

**[D31] OPTIONAL: the funded-tower variant.** An operator MAY run a tower with a small hot fee wallet
and bump on the owner's behalf. Its exposure is bounded and worth stating exactly: such a tower still
holds **no coin keys**, so compromising it costs the operator's fee float and **cannot touch a user's
coin** — a materially smaller claim than "keyless", and materially larger than nothing. It carries an
operational duty in exchange: the float must be refilled, and **a tower that runs dry fails at exactly
the moment it is needed**. A suggested rail is an onboarding-prepaid fee bond sized to ~2 spike bumps
(~2 × 152 vB × 50 sat/vB ≈ 15 000 sats, operator-carried and priced into the onboarding fee). This
variant is offered so the choice is informed; it is **not** assumed by any other part of this
specification.

**[Amendment, TES-fixable #4]** Independently of who funds it, `watch_pass` must be package-aware
(`submitpackage`, not per-tx `transaction_broadcast_raw`) and the bundle carries fee-child templates.
Multiple towers compose idempotently.

**[Shipped]** **sdk45** is the evidence for the two properties this section rests on: the watch bundle
carries **no key material**, and a **second independent tower is idempotent** (both explicitly
asserted there). **sdk51** runs the state machine against a real hostile trigger (and is a no-op while
the coin is idle — an un-broadcast coin never ages, so there is nothing to defend). The carrier-side
counterpart — materializing a colored branch before its deadline — is **sdk34** (§5.10.4). What is
**partly implemented as of 2026-08-11 [D31/#123]**: the package-aware broadcast now EXISTS as a
path — `mercurylib::wallet::p2a_fee_child::build_p2a_fee_child` builds and prices the v3 owner-funded
child, and `mercuryrustlib::core_rpc::submit_package` submits the 1P1C package to a Bitcoin Core node
(electrum has no `submitpackage`, so this is a second, opt-in backend used for this one call).
**Verified live, end to end, through repo code**: a v3 tier paying 0.048 sat/vB was REFUSED alone by a
node whose floor is 0.1 sat/vB (`min relay fee not met, 6 < 13`) and then ACCEPTED as a package
(`package_msg = "success"`, parent in the mempool with a descendant count of 2) —
`clients/libs/rust/tests/live_p2a_package_rescue.rs`. That is the WP1 acceptance criterion, which
required the rescue to run through this repo's own code rather than a hand-run `bitcoin-cli`.
**The CALLERS are now wired too (2026-08-11).** `exit_pass_with_bump` and `watch_pass_with_bump`
escalate a tier refused at its committed fee into a 1P1C package, and `unilateral_exit` /
`defend_ladders` use them whenever `SdkConfig::fee_bump` supplies an owner fee source. The
capability is an explicit argument, never ambient config, so the plain `exit_pass` / `watch_pass`
keep their exact keyless meaning — and a keyless pass now reports a fee-stuck tier as a **stated
limit** ("no fee-bump capability was supplied … a keyless tower cannot bump (D31) … it will not
clear by retrying") instead of as one more retryable failure indistinguishable from an immature CSV.
The anchor is located by matching the P2A script, never by a guessed vout, because a coloured tier
carries an extra `opret` and a hardcoded index would spend the payload output instead.

What remains genuinely unbuilt is the tower's FUNDING RAIL (the prepaid fee bond of the optional
funded-tower variant); the keyless default still walks the tiers with per-tx
`transaction_broadcast_raw` and attaches no P2A fee child — the committed fee of §5.3 carries each
tier at ordinary rates, but the amendment above is a specification, not shipped code, and no test
exercises spike-time fee bumping (§9). Note that under **D31** this gap is narrower than it looks for
the DEFAULT tower: a keyless tower would not bump even if the code existed, so what is missing is the
**owner's** package path (`#123`), which additionally needs a Bitcoin Core RPC endpoint the SDK does
not have — electrum exposes no `submitpackage`.

---

## 6. Requirement-by-requirement verification

**REQ 1 — off-chain forever / activity-scaled footprint: MET (per the refinement; near-categorical).**
Idle **laddered** coins: 0 vB forever — no rent, no deadlines, no forced materializations (the
baseline's entire 5,840 vB/coin-yr rent class deleted). **[Shipped caveat]** an idle **un-laddered**
coin (RGB carrier, B0 sub-coin) also pays 0 vB rent, but it keeps its absolute-locktime root
deadline, so a *received* carrier still owes one materialization before that deadline (§5.10.4) —
activity-scaled, but not
deadline-free. Off-chain transitions: 576 hops per depth level, rollover is off-chain (sdk43), so hop
count is **unlimited without any mandatory chain touch**; the default depth-3
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
  confiscation; bounded by the attacker paying ~2.5× the defender per coin. (No 7-day exit stampede of
  the pre-TES-R kind exists, though — untriggered coins idle safely under a dead SE forever.)
- **R-2 No unconditional no-watch window**: a received coin must be watched (delegated, keyless,
  prepaid) from the moment of receipt, forever. Alarm-driven with ≥1 day notice, but a downgrade in
  kind from the pre-TES-R first-window guarantee, accepted deliberately. **[Shipped]** on the
  un-laddered shape the duty is *calendar*-driven instead (materialize before the root deadline,
  §5.10.4) — sdk32 is the standing
  record of what happens to a received carrier whose owner never acts.
- **R-3 B1 unchanged**: the statechain trust unit (enclave share-deletion) remains the floor, as in
  the pre-TES-R design and every statechain.
- **R-4 δ=36 head starts vs sustained congestion**: a fee spike outlasting the head start converts
  the CSV edge into a pure fee race; committed fees + P2A + funded towers are the answer, and δ is a
  dial (open problem O-2 quantifies against mainnet history before mainnet ship). **[Shipped gap]**
  only the committed-fee half exists today — the tower broadcasts tier-by-tier and attaches no fee
  child (§5.13), so this risk is currently carried, not mitigated.
- **R-5 Deep-DAG unilateral latency**: depth-3 worst ≈ 60 days (coop exit instant); geometric
  schedules cap it ≈ 30 days at reduced budgets (O-4).
- **R-6 Enclave counter machine {level, m, k} is safety-relevant** for receiver verification; needs
  the INV-23/24-grade formal spec before ship (O-1).

---

## 7. Footprint economics

Primitives: P2TR in 57.5 / out 43 / overhead 10.5 / P2A out 13 vB; tier tx 124 vB; fee child 152 vB;
solo compaction 112 vB. All corrected figures from the economics review adopted (the draft's 58-vB
batched claim and 0.04 vB/transfer are **rejected**; no output netting exists).

**7.1 Per coin** (the Baseline column is the pre-TES-R design of §2–§3)
| | Baseline (pre-TES-R) | TES-R (shipped) |
|---|---|---|
| Idle coin | 5,840 vB/yr | **0 vB/yr** |
| Active, 10 tx/day, default depth-3 policy | 5,840 vB/yr (rent dominates) | 3,650 hops/yr ÷ ~2,300/compaction ≈ 1.6 × 112 ≈ **~180 vB/yr** (≈0.05 vB/transfer) |
| Active, no-compaction policy | n/a | **0 vB/yr** on-chain; +~248 vB contingent exit weight per 576 hops (~0.43 vB/hop, paid only if exiting unilaterally) |

**7.2 System, 1M-vB blocks, 52,560 blocks/yr, 10% of coins active at 10 tx/day (aggressive):**
- **1M coins**: idle 900k → 0; active 100k × ~180 ≈ 18M vB/yr ≈ **342 vB/block ≈ 0.034%** of block
  space (baseline: 111,111 vB/block ≈ 11.1% — a **~320× reduction**). Plus churn: 20%/yr
  deposit+exit at ~111 + ~372–828 vB adds ~0.2–0.4% — present in every design including Spark.
- **10M coins**: ≈ **0.34%** (+ churn) — vs the baseline's physically impossible 111%.
  **100M coins ≈ 3.4%** — the "hundreds of millions TVL" target is reachable if value concentrates
  realistically.
- Fully idle custody TVL is exactly free at any scale.

**7.3 The fee model (per the delegation refinement)**
- Onboarding fee: deposit tx share (~58–111 vB) + tower fee bond (~15k sats, actuarially sized) +
  tower infra (~$0.001/coin-yr: 10M outpoint subscriptions ≈ one indexed node ≈ $3–6k/yr, ~15 GB
  bundles). No idle-rent prefund line exists — the item that made the pre-TES-R design unpriceable
  ($292/coin for 5 idle years) is gone.
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
This deletes the baseline's one absolute guarantee (self-deposited never-transferred coin safe vs SE
alone) and breaks REQ3's "preserve or improve" — unfixably, because the amortization *is* the shared
multisig, which cannot include current holders and whose signer independence Bitcoin cannot prove.
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
REQ2 forces us to keep. Additional: 90-day unilateral/SE-death wait (13× worse than the baseline's
~7 days, correlated with bank-run moments); actuarial fee-model insolvency in the tail (13-month
frozen fees, no forward market); (k+1)× enclave share blast radius on an unattested SGX. Not even
useful as a bridge: TES-R migration is a single re-anchor per coin (§9), self-funding within one week
of the avoided baseline rent.
Salvaged ideas: P2A-on-everything, tower-executed maintenance (made off-chain), conveyed-locktimes
(moot for laddered coins — they have no calendar deadlines to convey. **[Shipped correction]** audit
[17] therefore does *not* close by deletion: it survives on the un-laddered shape, absorbed by the
`auto_exit_margin_blocks` default of 288 blocks (≥ k_max·interval + 144), not solved.)

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

## 9. Migration path off the pre-TES-R design — **COMPLETE**

*(Historical plan, kept intact; outcomes annotated. The migration is done: there is one protocol, the
`deposit_protocol_version` / `UTEXO_PROTOCOL_DEFAULT` escape hatch is deleted, and no test pins the old
lane. The per-test migration record lives in git history and `history/MIGRATION.md`.)*

**Phase 0 — ship, no behavior change** — *DONE*: server: generalized counters {level, m, k, total_sigs},
`POST /renew/init` (challenge-nonce rail), config + nostr record; SDK: TES-R builders in
`transaction.rs` (CSV tiers, v3, committed fee + P2A, public H_tag tweaks), the TES-R watch bundle +
package-aware `watch_pass` with fee wallet, R′ receiver checks behind a version tag in the transfer
message (receivers handle both ladder types during migration). Enclave: **cryptographically
unchanged** (blind MuSig2 + one-shot nonces suffice); only the counter state machine needs the
INV-23/24-grade spec (O-1) — no SGX rebuild required for launch, decoupling from mainnet-audit [13].
*Outcome*: shipped, with two deltas. (a) The transfer-message version tag that let receivers handle
both ladder types is gone with the escape hatch — receivers now dispatch on the coin's **shape**
(laddered vs un-laddered), not on a protocol number. (b) **`watch_pass` is NOT package-aware**: it
broadcasts tier by tier with `transaction_broadcast_raw` and attaches no P2A fee child, so
TES-fixable #4 (§5.13) is *specified but unimplemented*. It is adequate at ordinary fee rates
(committed fees make each tier self-relaying) and is the known gap under a sustained spike (R-4).

**Phase 1 — new deposits are TES-R** — *DONE, and now unconditional*: `claim()` co-signs T for every
fresh **confirmed root** coin (same B5 onboarding-window semantics as the old tx1 moment). The two
deliberate exceptions are shapes, not opt-outs: RGB carriers (§5.10) and sub-coins over un-broadcast
funding (B0, §5.4).

**Phase 2 — converting the pre-TES-R coins** — *DONE (rail retained)*: one final on-chain re-anchor per
coin (the 112-vB refresh, `refresh_sponsored` rail reusable), reborn as TES-R at depth 0. 1M coins ×
112 vB = 112M vB one-time ≈ one week of the avoided baseline rent — **the migration pays for itself
within ~7 days**; schedule over weeks. Un-broadcast sub-coin DAGs migrated at their next transfer
(auto-refresh hook) or before their root deadline (`auto_exit_due` covered stragglers). The re-anchor
and sponsored-rebate paths survive as ordinary maintenance (§5.6; defect D2 was found here).

**Phase 3 — delete the pre-TES-R calendar machinery** — *PARTLY DONE, deliberately*:
refresh-token-slot economics and the B6/[17] margin arithmetic are gone from the laddered path, but
`exit_deadline_block` and the
`auto_exit_due` pass are **kept** — they are what defends the un-laddered shape, which is permanent
(§5.2 / §5.10.4). The plan's "removed once the conversion completes" clause was written on the
assumption every coin would end up laddered; that assumption is false for RGB carriers. RGB: no
protocol change (the anchoring rules of §5.10 are a constraint tightening, enforced at co-sign time);
rgb-lib needs only the existing
un-broadcast-witness validation, now exercised over deeper chains — dedicated adversarial suite still
open (O-3).

**Test coverage (live, replaces the Phase-0 test plan)** — every item below names a test that exists
today:

| Property | Live evidence |
|---|---|
| Laddered deposit + full lifecycle | sdk48, sdk42, sdk44 (schedule params) |
| Transfer over the ladder (Model A) | sdk41, sdk49, sdk40 |
| Renewal → rollover → renewal, unbounded, 0 vB | **sdk43** |
| CSV enforced by real consensus; stale ladder dies once its prevout is spent | **sdk40** PART 1 / PART 2 |
| Cooperative de-trigger defeats a hostile trigger | **sdk40 PART 2** |
| Unilateral exit through the tier chain (public SDK surface) | **sdk50** |
| Watchtower defends a triggered coin; keyless bundle, idempotent second tower | **sdk51**, **sdk45** |
| Receiver verification R′ / census | sdk46, sdk47, **sdk54**, sdk55 |
| In-ladder split (adversarial + end-to-end) | **sdk58** (11 cases), sdk59 |
| First-class children (whole / partial onward hop) | **sdk60**, **sdk17** |
| RGB carrier never laddered; terminal-freeze over time | **sdk52**, sdk32; carrier materialization sdk34 |
| Lightning both directions on the ladder (HODL latch) | **sdk63**, **sdk64**, **sdk67**; sdk65 non-exact; sdk66/sdk68 failure paths; sdk53 latch guard |
| Concurrency / DAG invariants | chaos22 |

Not covered by any live test, and honestly open: **package-relay tower defense** and P2A fee-child
attachment under a real fee spike (§5.3 / §5.13 — not merely untested, *unimplemented*: no
`submitpackage` caller exists), the **colored** de-trigger variant (§5.8), and mass-grief
prioritization (R-1 / O-6). Those remain design-level claims. The earlier plan's "mixed-protocol
estates" item is void: no mixed estate can exist.

---

## 10. Open problems & future work

- **O-1 (blocking, STILL OPEN)**: formal spec + audit of the enclave counter machine {level, m, k} —
  receivers verify tree shape against it; a missed interleaving is a double-spend. INV-23/24-grade
  treatment before mainnet. *Partially de-risked in practice*: the census is exercised adversarially by
  sdk54/sdk55/sdk58 and across hops by sdk46/47/60/17, and retry-idempotence of the counter by sdk56 —
  but no formal spec exists, so this stays blocking.
- **O-2 (blocking dial)**: δ/δE vs sustained mainnet congestion — quantify head-start survival
  against 2023–24 spike history; δ=36 is the shipped default (`TesrParams::mainnet()`, arithmetic
  pinned by sdk44), with the budget table of §5.2 as the trade space. The congestion study itself is
  not done.
- **O-3**: rgb-lib adversarial suite for deep un-broadcast witness chains (terminal-freeze removes
  superseded-witness ambiguity, but depth and DoS bounds need tests). *Status*: the never-laddered
  carrier rule and terminal-freeze semantics are pinned (sdk52, sdk32) and the `rgb*`/`ta*`/`tb*`
  suites run over the shipped protocol; the depth/DoS adversarial suite is still missing.
- **O-4**: deep-DAG unilateral latency — geometric per-level E0/D0 schedules (worst wait <30 days at
  any depth) vs per-level hop budgets; ship as a dial with a chosen default.
- **O-5**: tower fee-bond actuarial sizing + refund/receipt rail (operator-signed per-bump rebates,
  reusing the refresh_sponsored rail shape).
- **O-6**: mass-grief saturation modeling (R-1) — prioritization policy for de-trigger waves,
  per-coin-value triage, and whether a standing "grief bond" priced into transfers should scale with
  coin count.
- **O-7**: relay-policy dependence — v3/P2A/1P1C are policy, not consensus; maintain the
  miner-direct-submission fallback and re-verify at each Core release.
- **O-8**: batched compaction (post-launch, **not shipped**): non-interactive constructions only (the
  interactive MuSig2 coordinator stays dead); pre-signed T′ against the template before broadcast is
  mandatory if revived.
- **O-9 [new, from the migration]**: `child_in_ladder_pay` splits a child through a depth-2 `ancestors`
  chain (§5.4). The N-hop census generalizes, but the interaction of *depth* with the hard depth cap of
  §5.11 and with the min_child_value floor (D1) at each level is only exercised to depth 2 (sdk17,
  sdk60). Deeper child chains need their own adversarial pass before they are relied on.
- **Covenant future work**: **CTV** would let one on-chain output commit to the whole T/X/S tree (and
  make factories safe — no signer cohort to collude, deleting B1-F — reviving Grove-style shared-UTXO
  amortization for onboarding, the natural Phase-4). **APO/ANYPREVOUT** would make state txs
  rebindable (true Eltoo): the trigger tier disappears, trigger-griefing with it, and renewal becomes
  a pure local re-sign. **CSFS** would allow delegated de-trigger without SE liveness. None are
  required: TES-R ships on today's Bitcoin.

---

*Implementation ledger (added when the migration completed): four defects the build exposed are fixed
and recorded where they bite — **D1** in-ladder-split admission floor (§5.4), **D2** sponsored-refresh
rebate sized into D1's dead window (§5.6), **D3** the one-call Lightning PAY API refusing every
laddered coin (§5.12), **D4** a unilaterally-exiting child booked `WITHDRAWING` (§5.9). Four of the
draft's claims were over-stated and are corrected in place rather than deleted: the calendar machinery
(and audit [17]) survives on the un-laddered shape (§5.2, §5.10.3/4, §8.2, §9 Phase 3), the latch is a
HODL-invoice latch and its piece stays terminalized (§5.12), children are first-class rather than
exit-only (§5.4), and TES-fixable #4 (package-aware tower broadcast) is **specified but unimplemented**
(§5.13, §9 Phase 0) — the one place this document describes something that does not yet exist in code.*

*Adversarial findings ledger: TES review-1 FATAL#1 defanged (§5.8), FATAL#2 accepted-as-tolerated
(§5.7); TES review-1 fixables 1–7 folded; TES review-2 corrections (compaction 103 vB, exit 828 vB,
δ-sensitivity, policy dependence, m≤floor-2, migration self-funding) folded; RGB fork resolved by
deletion + terminal-freeze (§5.10); Grove review-2 salvage (trigger-ladder standalone, co-op
de-trigger, tower bond, onboarding-fee framing) adopted; Grove factory + C-90 rejected with reasons
(§8).*
