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
| What invalidates old state | Consensus (absolute locktime ordering) + enclave refusal | **1-of-n operator honest key deletion** (trust) | Round expiry (consensus) | Decrementing nSequence DW (~64 updates then exhaustion) | **Consensus** (lower-CSV wins the trigger output; old epochs' parents unconfirmable) + the receiver's exact-equality census over the enclave's attested `sig_count` as defense-in-depth (§5.11) |
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
  forced-settlement (**834 vB** worst + days — the ~828 this line used to carry is the TES review-2
  exit-cost figure recorded in §8.3, 3×124 + 3×152, built on the superseded 124-vB tier; §5.9)
  to one 125-vB co-op de-trigger (the verdict stands; its
  "griefing is economically losing" *pricing* did not survive re-derivation — §5.8);
  the offline-theft FATAL is
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

**Constants (shipped defaults). [Shipped correction — the PUBLICATION half of TES-fixable #7 is not
built, and something stronger does its job.]** `/info/config` serves only `initlock`, `interval`,
`batchtimeout` and `version` (`server/src/endpoints/utils.rs`), and the NIP-100 nostr record carries the
flat `timelock` plus fee/status metadata (`server/src/main.rs`) — **neither publishes the tier schedule.**
What actually defends a receiver against a per-victim parameter split is receiver-side, and does not
depend on the SE publishing anything: a schedule that travels on an artifact is data, never a yardstick.
`cap_schedule` measures every conveyed `TesrParams` against the receiver's OWN network preset field by
field and refuses by name on the first disagreement ("`d0` = 24 where `bitcoin` says 1440"), and
`TesrParams::flat_ladder_params` is the single authority for the flat pair — the coordinator refuses to
boot against an env that disagrees with it (ci-guard `deny_flat_ladder_config_drift`).

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
idle coin — and an entire idle split/combine DAG — never ages **on the CSV side**: no refresh rent, no
root-deadline materializations.

**[D36 correction, 2026-08-14 — this paragraph used to end "No calendar deadlines … deleted for
laddered coins", and that is FALSE of every RECEIVED laddered coin.]** The ladder is not the coin's
only pre-signed material: the FLAT BACKUP CHAIN is retained, and it carries an absolute locktime which
each whole-coin hop shortens by `interval`. A coin that has been received `k` times therefore sits on
`min(L_k)` — a real, finite, approaching calendar height held by its PRIOR OWNERS — whatever its
ladder is doing. `SDK_E2E=86` measures exactly this: the tip advances toward that height and each hop
spends `interval` of it. What TES-R deletes is the CSV-side ageing; what survives is `min(L_k)`, and
`TRUST-MODEL.md` B4(ii) and `SPEC.md` §1 have said so since D36. The audit-[17]/B6 class is therefore
NARROWED, not deleted.

**[Shipped correction]** it is *not* deleted wholesale: the un-laddered shape (RGB carriers, §5.10;
sub-coins over un-broadcast funding, B0) still rests on the signed-once **absolute-locktime** backup,
so it still has a root deadline and still needs materialization before it. `auto_exit_due` and
`exit_deadline_block` therefore **survive, scoped to that shape** (`SdkConfig::auto_exit`, default on),
and are exercised live by sdk34 (carrier watchtower materializes before the
deadline) and sdk32 (the residual clawback window on an un-laddered carrier). A laddered coin has no
deadline for them to act on.

### 5.3 Fees [Amendment: TES-fixable #2 + Grove reviewer's pinning finding, merged]

Every pre-signed tx (T, X, S, SP, CB) is **nVersion=3 (TRUC)** and carries:
1. a **small committed fee** drawn from the coin (target ~2 sat/vB at signing; Σout = Σin −
   fee_committed) so the base case relays and confirms standalone — restoring the self-funding
   property the signed-once backup chain always had, and removing the chicken-and-egg the review
   found in the all-zero-fee draft; and
2. a **240-sat P2A anchor** (OP_1 0x4e73, anyone-can-spend) so the owner — or an operator's funded
   tower on their behalf, but **not** a keyless one (§5.13) — attaches a live-rate fee child during
   spikes. **[Shipped]** that child is **153 vB** (11 base + 41 P2A-in + 58 funding-in + 43 change),
   `estimate_child_vsize`, and the estimate was measured against a real signed child on regtest
   (153 estimated, 153 actual) rather than modelled.

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

**[Shipped correction — steps 1 and 3 are DESIGN, not running code; step 2 is what ships.]** There is
no `/renew/init` route anywhere in the tree, and no `total_sigs` column, field or counter machine:
`mercuryrustlib::tesr::renew` is exactly two `cosign_tier` calls over the ordinary
`/sign/first` + `/sign/second` pair, and `m` and `k` are fields of the client's own bundle, advanced
locally. So the SE-side renewal gate — the challenge-nonce binding and the "refuse unless the state
counter is genuinely within 2δ of the floor" refusal — **is specified here and unenforced**. What that
costs is narrower than an epoch-burn lever, and the narrower statement is the true one: `POST /sign/first`
already returns **401** unless the request carries a schnorr signature by the coin's own auth key
(`validate_signature`, `server/src/endpoints/sign.rs`), every co-sign still needs the owner's MuSig2
partial, and `m`/`k` are client-local bundle fields the SE never reads — so no coordinator-side epoch-burn
lever follows from the missing route. What the missing rail actually costs is **replay**: `/sign/first`
takes the replayable `signed_statechain_id`, not the single-use endpoint-bound
`validate_signature_nonce` of audit [15] that `withdraw/complete`,
`deposit/get_derived_token` and `statechain/spend_budget` already use, so a captured request can be
re-served. The concurrent-session
half of that shape is already closed — `acquire_signfirst_lock` fails **closed** with a retryable 503
rather than proceeding unserialized. The counts the SE does
publish are two, and both are load-bearing elsewhere: the lockbox's **lifetime `sig_count`** (attested
`utexo/sig_count/v2`, served through `GET /info/statechain/<id>` as `num_sigs`), which is the right-hand
side of the receiver's census (§5.11); and the per-node `sig_budget`/terminality receipt at
`GET /statechain/spend_budget/<id>`, which the enclave itself re-checks before consuming a secnonce.

**[Correction to the correction — steps 1 and 3 were DECIDED OUT, not deferred.]** An earlier pass closed
the paragraph above with "the design in steps 1 and 3 stands — it is the code that is behind it (O-1)".
That verdict is wrong in the direction that promises work nobody intends to do, and two in-repo decision
records say so: **DECISIONS.md D22** lists per-level SE counters as "the 'build it' option D4 rejected
partly on scope … re-openable if wanted, though D4's proof means it is no longer *needed*", and
**SPEC-ROADMAP §6** scopes "the {level, m, k} counter machine and `POST /renew/init`" out
**unconditionally**, on D4's round-2 proof that a hidden co-sign at **any** level raises the same TOTAL —
which is exactly what the shipped exact-equality census catches (§5.11). Steps 1 and 3 are therefore kept
above as the **design of record** for a renewal RAIL that was considered and dropped, not as a machine the
census depends on; step 2 is the whole of what renewal is. The one piece of step 1 still worth building on
its own merits is the audit-[15] nonce on `/sign/first` — a replay rail, not a counter machine.

**Why old state dies at the consensus level, not by enclave promise**: X_{m+1} strictly undercuts
every older extension in the CSV race for T.out[0]; every pre-renewal state hangs on an extension
that can now never confirm. This is Decker-Wattenhofer replace-by-lower-timelock at one dedicated
tier — renewal replaces horizontally, tree depth stays constant across all epochs. Honesty note
(accepted from review): "consensus-dead" is *race-conditional* — the newest extension must win a
≥36-block-edge race after a public trigger. That is strictly stronger than the baseline's post-window
position and categorically stronger than Spark's key-deletion promise, but it is a race, not an
axiom.

**[Shipped correction — there is no "enclave single-active-state refusal", and this document used to
call it the second independent layer.]** The enclave does not track which state is current and cannot
refuse a rival one; it *must* co-sign rivals, because that is what a renewal IS (a lower-CSV extension
over the same outpoint). What the enclave enforces is two different things, both real and neither of them
a rival defence: **one signature per secnonce** — the sealed secnonce is loaded and consumed in the same
row-locked transaction, and a second partial signature over a different challenge finds it NULL and is
refused (`lockbox/src/server.cpp`) — which is a MuSig2 nonce-reuse *key-leak* defence; and a per-coin
**`sig_budget` vs lifetime `sig_count`** check, run before the secnonce is consumed, which is what makes
terminalization (and the refusal of renewal on terminal nodes, §5.10 rule 2) enforceable rather than
promised. The genuine second layer over the consensus race is **the receiver's exact-equality census**
over that attested `sig_count` (§5.11): an undisclosed rival state cannot be hidden, because it shows up
as a count the disclosed tiers cannot account for.

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
extension + state tiers (fresh 576-hop budget). Cost: zero on-chain; +2 pre-signed txs (250 vB) of
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
| Ancestor's past owner vs my sub-coin | Root chain + stale S_j at some level | Confirm T, X; broadcast stale S_j before my SP | Tower broadcasts SP at Δ_{k+1} (undercuts by ≥36); per-tier repeat | **Honest**, per-tier ≥36-block edge. **[D36] A CALENDAR DEADLINE DOES EXIST**: the coin still sits on `min(L_k)`, the retained flat-backup height held by its prior owners | Per-tier reaction; ≥144 blocks notice before ANY hostile tx is final — but only until `min(L_k)` |
| Hacked SE alone | e_n (1 of 2-of-2) | Nothing signs; can only refuse (freeze ≠ seize) | Unilateral tree pre-signed, needs nobody | **Honest, unconditionally** | None |
| Hacked SE + past owner w/ retained pre-rotation share (**B1**) | Full key of F | Fresh no-timelock spend, private relay | Race with pre-signed material; blindness blocks target selection; public counter receipts attribute | **Adversary favored — byte-identical to the pre-TES-R B1**, the statechain trust unit, unchanged in kind and mitigations | Enclave deletion + attestation ops; settled coins immune |
| Hacked SE + thief, coin received PRE-hack, untouched | e_n only | Cannot produce owner's partial | 2-of-2 arithmetic | **Honest, unconditionally** (REQ-3 core case) | None |
| Trigger griefer (any past owner, anonymous) | T | Broadcast T to force cost | **Co-op de-trigger: 125 vB, coin fully restored (§5.8)** | **Honest** on the coin (nothing is stolen), but **not "economically losing" for the attacker**: at the committed rate he pays 0 and the coin pays ~980 sats in fees + anchors (§5.8) | SE alive; else falls to row 1 |
| Malicious/buggy tower | Bundle (all pre-signed, pays owner only) | Broadcast early / not act | — | **Honest** (early = settles to owner; inaction = no tower) | — |
| Mempool pinner | — | Pin packages | TRUC 1P1C + sibling eviction + committed base fee + P2A | **Honest** | Standard fee-bumping |

**[Shipped] where each row is exercised**: rows 1–3 (stale state / old epoch / ancestor race) —
**sdk51** (a watchtower defends against a hostile trigger end-to-end) and **sdk40 PART 2** (the stale
state dies at consensus); the honest defence's pre-signed material — **sdk50** (unilateral exit
through the full tier chain) and **sdk45** (the keyless bundle drives it with no key material);
adversarial rejection of forged/hidden structure — **sdk58** (11 cases), **sdk54**, **sdk55**. Rows 4–6
(hacked SE) are arithmetic properties of 2-of-2, not test-observable beyond the co-sign gates
(**sdk56/sdk57**, retry-idempotence and owner-share binding).

**[D36 correction, 2026-08-14.]** This paragraph used to read *"Row 3's 'no calendar deadline exists'
holds for a **laddered** sub-coin; the un-laddered shape keeps its root deadline"* — which REAFFIRMED
the error above rather than catching it, and split the world along the wrong axis. The distinction is
not laddered/un-laddered: **every coin that has been RECEIVED keeps `min(L_k)`**, because the flat
backup chain is retained alongside the ladder and each hop shortens its absolute locktime by
`interval`. Laddering removes the CSV-side ageing and nothing else. `SDK_E2E=86` is the measurement;
`TRUST-MODEL.md` B4(ii) is the trust-model statement; `SPEC.md` §1 carries it in the specification.

**Why this mattered enough to correct rather than annotate:** an implementer reading the row as
written would omit `deadline_safety_due` entirely — it is the ONLY scheduled defence of `min(L_k)` on
a laddered coin — on the strength of a sentence saying the deadline does not exist.

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
funding output F′** and rebuild T′/X′_0/S′_0 — one tier-shaped v3 tx, confirmed unopposed inside the
≥144-block window during which no adversary tx is valid. **[Shipped correction — the size.]**
`build_detrigger` emits a *tier*, anchor and all (`build_tier_tx`, payload out + 240-sat P2A), so it is
**125 vB** (`TIER_VBYTES`, measured), not the 111 vB of a bare 1-in-1-out co-op spend; the coloured
variant is a coloured self-transition at **168 vB** (`COLORED_TIER_VBYTES` — an `opret`, not a tapret,
and exactly one P2TR output wider than the plain tier). Consequences:

- **Trigger griefing is SURVIVABLE — the victim's value always comes out**: it answers with one
  125-vB de-trigger, and the grief is fee-attributable on-chain even though T itself is anonymous.
  > **[D57] "collapses from FATAL to priced nuisance", "keeps off-chain-ness" and "the coin's ladder
  > resets fresh" are RETRACTED as unproven claims.** They rest on the de-trigger spending into a
  > fresh funding output `F′` and rebuilding `T′/X′_0/S′_0` — and `cosign_detrigger`, the co-signer
  > that would do it, has **zero callers repo-wide**. `sdk40` PART 2 proves only the consensus half
  > (the stale `X′` dies); it calls `cosign_tier` and sends to an external address, so what it
  > demonstrates is an ON-CHAIN exit, which is the outcome "collapses from FATAL" says is avoided.
  > Restoring the sentence needs the wiring plus an E2E that lands `F′` and a rebuilt ladder. See
  > §5.8.
  > **[Corrected] "Damage:cost ratio ≈ 0.4 — griefing is economically losing" was a vB ratio
  > (111 ÷ 276) and it does not survive being priced in sats.** Both transactions pay out of the
  > **coin**: a tier's fee is committed at signing (`tier_out_value` = prev − committed_fee − P2A), and
  > that is as true of the hostile T as of the de-trigger. At or below the committed 2 sat/vB the
  > attacker broadcasts a transaction he already holds and pays **nothing at all**, while the coin loses
  > 2 × (250-sat committed fee + 240-sat anchor) ≈ **980 sats**. The attacker begins paying only above
  > the committed rate, where T no longer relays on its own and needs a CPFP child he must fund — but at
  > that same rate the victim's de-trigger is itself a stuck tier needing an **owner**-funded child
  > (§5.13). So griefing is not "economically losing"; it is **cheap-to-free, and bounded**: the damage
  > is fee-sized sats out of the coin, never the coin, and the ladder is restored.
  > **[D35] This whole calculus is stated for a lane where voiding the tree yields the attacker
  > NOTHING**, and that premise is what keeps the damage fee-sized. It holds only while every spend of
  > `F` a prior owner still holds is PLAIN. A retained *coloured* backup would turn the same ~112-vB
  > spend from destruction into capture of the whole allocation, at which point every number above
  > describes the wrong quantity entirely. §5.10 rule 6 is what keeps the premise true.
- **Mass-grief saturation** (residual, quantified): 1M simultaneous triggers force ~125M vB of co-op
  responses within a ~144-block grace ≈ **87%** of a day's block space (144M vB) — strained but
  survivable. The triggers themselves are another ~125M vB of block space, but per the correction above
  the attacker does not *pay* for that at the committed rate: those fees come out of the victims' coins.
  Beyond ~1M/day the response degrades to prioritized fee auction on the highest-value coins. If the SE
  is *simultaneously* dead, de-trigger is unavailable and rows 1–3 apply per coin. This correlated scenario is the design's worst case and is stated as residual risk
  R-1 (§6).
- The de-trigger requires SE liveness — it is a UX/cost shield, never a safety dependency (the
  unilateral tree always exists).

**[Shipped — [D57] SCOPED to the half that is actually proven.]** Built by
`mercurylib::tesr::build_detrigger`. **sdk40 PART 2 proves the CONSENSUS half**: a griefer broadcasts
T′, the owner's fresh no-timelock spend of `T′.out[0]` confirms immediately, and X′ can never confirm
afterwards even past E blocks.

> It does **not** prove the restoration half, and this paragraph used to claim it did — "co-signed by
> `cosign_detrigger`, and proven end-to-end". sdk40 PART 2 calls `cosign_tier`, not
> `cosign_detrigger`, and its destination is `bitcoin_core::getnewaddress()` — an **external wallet
> address**, not a fresh funding output `F′`. So the claims above it that *the de-trigger spends "into
> a fresh funding output F′ and rebuilds T′/X′_0/S′_0"*, that the victim *"keeps off-chain-ness, the
> coin's ladder resets fresh"*, and — resting on those — that *"trigger griefing collapses from FATAL
> to priced nuisance"* are **UNTESTED**. `cosign_detrigger` has zero callers repo-wide; only the
> coloured twin is wired.
>
> What is proven is that the grief is **survivable**: the value comes out. What is not proven is that
> it is **cheap** — the coin going on-chain is the outcome the FATAL-downgrade says it avoids. Until
> **[D68] `cosign_detrigger` IS NOW WIRED** — `UtexoWallet::detrigger_to_owner`, driven end to end by
> `SDK_E2E=89`: a griefer confirms `T`; the owner answers with a de-trigger spending `T.out[0]` at no
> relative timelock; it confirms; the value lands at an address the OWNER named; and the pre-signed
> extension is then submitted to the node and REFUSED — `bad-txns-inputs-missingorspent`. The old
> ladder is dead, measured against bitcoind rather than inferred.
>
> So the specification may now say: **the owner chooses when the coin lands, in two transactions with
> zero CSV wait, and every retained tier dies with it.** It still may NOT say "the ladder resets
> fresh" — there is no `F′` and no rebuilt `T′/X′_0/S′_0` on this lane, so getting back off-chain is
> a fresh deposit. That restoration half remains unbuilt.

The colored (168-vB `opret`) variant and the mass-grief prioritization policy (R-1 / O-6) are not
test-covered either.

### 5.9 Exit costs

- **Cooperative** (normal path): 1 fresh co-signed tx ≈ 111 vB, instant, batchable — unchanged.
- **Unilateral, flat coin**: 3 pre-signed txs (T+X+S = **375 vB**, self-relaying via committed fees) +
  0–3 P2A children (153 vB each) in spikes → **375–834 vB**; wait = E_m + Δ_k sequential, worst
  2,160 blocks (~15 days) fresh, decreasing 36 blocks per hop/renewal. [Amendment: the draft's "1–3
  children" corrected — with committed fees the base case is 0 children; in a spike all 3.]
  **[Corrected — the 372/828 this line used to carry were built on a 124-vB tier.]** The signed tier is
  **125 vB**, measured through the production finaliser: the 124-vB model assumed a 64-byte
  `SIGHASH_DEFAULT` witness, while TES-R hashes with `TapSighashType::All` and therefore carries the
  explicit `0x01` sighash byte. At 2 sat/vB that model committed 248 sat to a transaction that relays at
  125 vB — 1.984 sat/vB — i.e. it silently broke the one property the committed fee exists for. Every
  figure derived from it moved with it.
- **Unilateral, depth-d sub-coin**: 3 + 2d txs = **293·d + 375 vB** (`tesr_exit_vbytes`: T, X and the
  final state at 125 each, and per level an `SP` — the only rung with two payload outputs, 125 + 43 —
  plus one 125-vB extension); worst wait ≤ (d+1)·2,160 blocks —
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
6. **[D35] Every SE co-signed spend of a coloured coin's `F` is one of exactly two things** — there is
   no third category, and "an opret nobody verified" is a refusal rather than a shape:
   (i) **PLAIN**, and therefore an acknowledged allocation-destroying spend that **no exit path may
   recommend** (this is what the retained flat backups of a coloured coin are, and why they are
   omitted from carrier watch bundles); or
   (ii) **COLOURED and receiver-assigned**, verified as such by the receiver's own
   `verify_consignment_assignment` against an outpoint read from the RECEIVER'S own coin record.
   The consequence is a lane rule, because the permitted backup shape is a function of the DECLARED
   lane and not of the union of two lanes' shapes: **on a coloured bundle every conveyed flat backup
   MUST be plain** — any OP_RETURN on one is refused by name, in the same predicate that runs over
   every flat backup, as part of the R′ set (§5.11) alongside the tier colour-shape check. The
   permissive `op_return_outputs <= 1` STAYS for the **un-laddered carrier lane**, where the coloured
   backup is the legitimate exit material; that is precisely the point, since one validator was
   serving two lanes with opposite requirements. Without this, a prior owner's retained coloured
   backup is an *undetectable allocation-theft primitive* — it spends `F` and re-assigns the
   allocation to themselves, invisible to every other receiver check — and it also falsifies §5.8's
   griefing-is-losing premise.
   **[Shipped]** `verify_flat_backup_lane` (`clients/libs/rust/src/tesr.rs`), run on BOTH acceptance
   paths (claim and the SSP pre-pay census); guarded by
   `ci-guards/tests/deny_colored_backup_on_a_colored_ladder.rs`; the legitimate coloured-ladder-plus-
   envelope shape it admits is exercised live by **sdk78** (c.2).

**[Shipped] evidence**: **sdk52** pins rule 1, the invariant everything else here rests on — in one
wallet the plain coin carries a ladder and the token carrier carries **none**, and an off-chain RGB
transfer still settles. **sdk32** pins the terminal-freeze semantics over time (carrier terminal at the
SE, un-laddered, no fresh co-signed sweep possible) and the residual clawback window of point 4. The
RGB suites (`rgb*`, `ta*`, `tb*`) run over the shipped protocol unchanged.

### 5.11 Receiver verification at claim (R′ set)

Derived from public data; any deviation = reject: (R3′) F on-chain, unspent, pays A. (R4′) T spends F,
no timelock; tier outputs pay A + the public H_tag tweaks; every later tier's **signed** nSequence is a
BIP-68 *block* relative timelock lying inside the band its kind allows — `[e_floor, e0]` for an
extension, `[d_floor, d0]` for a state, exactly `SPINE_CSV = 0` for a split tip — and the tier's
**declared** `csv` field is bound to that same signed number (`bind_declared_csv`), so a schedule that
contradicts the signatures is refused rather than believed; superseded tiers are held to the identical
band and binding. Headroom ≥ receiver policy.

> **[Corrected — this line used to assert a per-position exactness that nothing checks.]** It required
> "the current extension at exactly `E0 − m·δE` and the new state at exactly `D0 − (k+1)·δ`".
> `verify_bundle_ex` (and `verify_child_bundle`, and the superseded loop) check **bands**, never
> positions, and no receiver can form the exact term: nothing serves `m` or `k` (§5.5) and they are
> fields of the *sender's own* bundle. The band is not the weaker yardstick it looks like, because its
> endpoints are not the sender's either — `cap_schedule` runs BEFORE the census on both receive paths
> and refuses a conveyed `TesrParams` field by field against the receiver's OWN network preset. What the
> exact position would have added — that the disclosed tiers sit at the epoch indices the coin's history
> implies, so no undisclosed co-sign hides between them — is carried instead by R5′ below (a hidden
> co-sign at any level raises the same TOTAL) and by the D14 margin, which is measured off the LIVE
> rival's structural position rather than off a declared epoch index. The exactness is therefore
> **withdrawn as a requirement**, not left as a rule the code is behind: per D4 / SPEC-ROADMAP §6 the
> `{m, k}` machine it would need was scoped out unconditionally (§5.5).

**[Shipped correction — the yardstick is NOT the SE's.]** This document used to say those bands are checked
"against the SE's publicly-served counters"; no endpoint serves m or k (§5.5). The authority is the
receiver's OWN network preset, and a conveyed schedule that disagrees with it is refused field by field
(`cap_schedule`) rather than honoured — which is the stronger property, because it holds even against a
coordinator that lies. Rival separation is enforced with the same discipline: **[D14]** the margin a
superseded tier must clear is read off the LIVE rival's structural position (`RivalKind::margin` — δ for
a state, δE for an extension; 36 blocks each on mainnet), never off which conveyed list the superseded
tier arrived in. (R5′) SE signature count == exact expected tree size — `verify_bundle_bound` enforces
the exact equality **`se_num_sigs == flat_backups + tiers + superseded`**, where `se_num_sigs` is the
lockbox's attested lifetime `sig_count`; any hidden extra co-signed state/extension shows as a count
mismatch, same linchpin as R5 in the pre-TES-R receiver set. The census only proves anything if that
count is the ENCLAVE's, so it is not taken on the coordinator's word: `/info/statechain` must carry an
`utexo/sig_count/v2` attestation over `(statechain_id, num_sigs, sig_budget, nonce)`, verified against
the **PINNED enclave attestation identity** rather than against any key served in the same response,
and an unattested or unverifiable count is refused outright.

> **[D69]** This clause read "the chain-anchored `enclave_public_key` (the one already bound to the
> tx0 output)". That anchor does not exist for the coins that need it most: a depth-≥2 in-ladder-split
> ancestor's funding output is deliberately **un-broadcast**, so there was nothing on chain to bind to
> and the verifying key arrived alongside the signature (TRUST-MODEL B11). The enclave now signs every
> attestation with one long-term identity (`utexo/attestation-identity/v1`, served at
> `GET /attestation_identity`) which the client pins: **compiled-in pin → configured value → REFUSE**,
> never a fallback to the served key. The check is now independent of whether the coin is on chain, so
> it holds at every split depth.
(R6′/R7′) per-level branch validation + Σ-inputs terminal ancestors, now
including the terminal-freeze check for colored ancestry. (R8) RGB consignment client-validated,
un-broadcast witnesses allowed.
All txs v3, committed-fee + single P2A, Σout = Σin − fee. **[Amendment, TES-fixable #6]**:
claim-time validation DoS pricing.

> **[D7/D25] The "hard depth cap of 8" that used to be specified here NEVER EXISTED IN CODE, and has
> been deleted rather than implemented.** The real cap is *derived*, not a literal:
> `max_split_depth(base, per_level, epoch_blocks) = 1 + (epoch − base_wait)/level_wait`
> (`max_split_depth`, `lib/src/transfer/receiver.rs`), enforced by
> `enforce_split_depth_cap_shaped` (`clients/libs/rust/src/tesr.rs`). It therefore MOVES WITH THE NETWORK PROFILE, which is the
> whole point — and is why the profile itself is now explicit per network rather than falling through
> to a toy schedule. Publishing a fixed 8 would have contradicted the code on every profile at once.

**[Shipped]** the R′ set is enforced at claim: **sdk46** (R5′ against the *real* SE counter — the SE
increments by exactly the tier count, `verify_bundle` accepts the true count and rejects a hidden extra
signature), **sdk47** (R′ across a transfer), **sdk54** (adversarial `verify_bundle`), **sdk55**
(backup-chain adversarial), **sdk58** (11 adversarial in-ladder-split cases). The census generalizes to
N hops for first-class children, per segment and with the same three terms
(`child_num_sigs == CHILD_V2_BASELINE + tiers + superseded`, and `CHILD_V2_BASELINE = 0` because a
derived child slot never ran `create_tx1` and so has no flat backup): each hop discloses exactly one
superseded state, so an undisclosed rival shows up as a count mismatch — **sdk60**, **sdk17**.

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

{trigger, newest extension, newest state or SP chain, per-tier CSV schedule} — all pre-signed, pays
only the owner, zero key material (REQ-34 preserved). `watch_pass` becomes a small state machine:
monitor F (one outpoint subscription) → on hostile trigger, broadcast X at +E_m and state/SP at +Δ.

> **[D57] THREE CORRECTIONS, 2026-08-14 — this paragraph described a tower that was never built.**
>
> * **"fee-child templates"** was listed here and again under TES-fixable #4. No such field exists:
>   `TesrBundle` has none, and `WatchEntry` carries only `branch_txs`, `backup_tx`, `backup_locktime`,
>   `deadline_block`, `trigger`. It **cannot** exist — a fee child needs a funding input the keyless
>   tower does not hold, which is [D31]'s entire point. Removed rather than annotated: a bundle field
>   that cannot exist is not a spec detail, it is a wrong one.
> * **"attaching P2A children"** contradicted this section's own closing paragraph, which says the
>   keyless default "attaches no P2A fee child" and that under D31 *that is the correct behaviour, not
>   a missing feature*. Removed.
> * **"request co-op de-trigger via the owner's SDK if reachable"** is not a code path.
>   `watch_pass` → `watch_pass_seen` branches only `Idle` / trigger-matched / `Void`, and
>   `cosign_detrigger` has **zero callers repo-wide** — only the COLOURED twin
>   (`cosign_colored_detrigger`) is wired, and only from `colored_reanchor`. Removed; if it is ever
>   wanted it is a feature to build, not a behaviour to describe. **[D31] WHAT A KEYLESS TOWER CANNOT DO — normative.** A keyless tower can watch `F`, and can
broadcast the pre-signed tiers **at their committed fee**. It **cannot fee-bump them.** A CPFP child
spending the P2A anchor requires an input the tower does not hold and a signature it cannot make, so
if the mempool's floor rises above a tier's committed rate, a keyless tower has no action available:
the tier is refused outright at `sendrawtransaction` and that refusal is one it cannot answer. This is a
**stated limit of the protocol, not an implementation gap** — an implementer must not read "delegable,
keyless watching" as implying spike-time rescue.

> **[Corrected] the refusal this document used to quote — `min relay fee not met, 200 < 423` — is not a
> TES-R tier's.** It is WP1's, produced on an isolated node with a lab-forced 3 sat/vB floor against a
> *synthetic* 141-vB transaction carrying a 200-sat fee (1.42 sat/vB). A real tier is 125 vB
> (`TIER_VBYTES`) committing 250 sats at 2 sat/vB, so under that same 3 sat/vB floor it reads
> **`min relay fee not met, 250 < 375`**. The shape of the claim is unchanged and the limit is real —
> only the numbers were borrowed from a different transaction. (The one live quote below,
> `min relay fee not met, 6 < 13`, stays — but for a narrower reason than "it is the real one". Its
> numbers are lab numbers too: that run is a **regtest** node at a **0.1 sat/vB** floor against a parent
> deliberately built under it, and `RESIDUAL-CLASSIFICATION` classes it as the same kind of number as
> 200 < 423. What is different, and is the whole reason it is quoted, is the *path*: the refusal and the
> package that answered it went through this repo's own `build_p2a_fee_child` + `submit_package`, which
> is the WP1 acceptance criterion. **Neither figure is a protocol bound**; both are lab conditions.)

Nor does the anyone-can-spend anchor supply a rescuer. **[Corrected] the "~900×" that used to stand here
was three errors stacked**, and it is worth replacing rather than softening because the conclusion is
sound and the arithmetic was not. WP1's child paid 180,330 sats, but (a) 180,330 ÷ 240 is 751×, not 900×;
(b) the 900 came from 180,330 ÷ **200** — the ratio to the *parent's fee*, a different quantity; and
(c) 180,330 was the funding wallet's chosen overpay, not a requirement: at that run's own 3 sat/vB floor
the 318-vB package needed 954 sats total and the parent had already committed 200, so the child's
required fee was **754 sats**. The honest reason no stranger bumps is **structural, not a ratio**: the
child's change must clear `CHILD_CHANGE_DUST = 330` while the anchor is worth `P2A_VALUE = 240`, so an
anchor-only child can never produce a legal change output at any fee rate — and the only builder in the
tree hardcodes two inputs (anchor + the owner's funding UTXO), so it cannot be constructed with repo code
at all. "Anyone-can-spend" is a permission, and there is no shape in which it is an incentive.

**[D31] WHO BUMPS, THEN: the owner.** The party that funds a CPFP package is the **coin owner**, from
their own wallet — the only party holding both a spendable UTXO and a motive. The consequence is
explicit: **the guarantee during a fee spike is "the owner is online"**, which is precisely the
condition a watchtower otherwise removes. The protocol does not promise otherwise.

**[D31] OPTIONAL: the funded-tower variant.** An operator MAY run a tower with a small hot fee wallet
and bump on the owner's behalf. Its exposure is bounded and worth stating exactly: such a tower still
holds **no coin keys**, so compromising it costs the operator's fee float and **cannot touch a user's
coin** — a materially smaller claim than "keyless", and materially larger than nothing. It carries an
operational duty in exchange: the float must be refilled, and **a tower that runs dry fails at exactly
the moment it is needed**. **[FUNDING RAIL — and the unit it must be sized in is NOT sats.]** A bond of "~2 spike bumps
(~2 × 153 vB × 50 sat/vB ≈ 15 300 sats)" is the obvious sizing and it is not the binding one.

A fee child is v3 and spends two things: the stuck tier's P2A anchor, and a funding UTXO. Under TRUC
(BIP-431) a v3 transaction may have at most **one** unconfirmed ancestor — and the tier is already
it. So a second rescue funded from the *unconfirmed change of the first* has two unconfirmed ancestor
chains and is refused at any price. **Measured, not argued**
(`clients/libs/rust/tests/live_tower_float.rs`): the chained attempt returns
`TRUC-violation, tx <txid> would have too many ancestors`, while the same tier funded from a second
CONFIRMED output is accepted.

**Therefore a tower's simultaneous-rescue capacity is the number of CONFIRMED fee UTXOs it holds,
each large enough for one bump — not its balance.** A float of 1 000 000 sats in ONE output rescues
exactly one tier per confirmation window however many coins it watches; a tower sized only in sats
reads as solvent and is not, which is the D31 failure wearing a reassuring number. The rail therefore
reports in both units (`tower_float::Solvency`), names which one failed, and gives the matching
remedy — short on sats means add money, short on capacity means SPLIT what you already hold, and the
two are not interchangeable. `tower_float::plan_float` distinguishes a float that needs a top-up from
one that needs only re-shaping, because the latter is fixable for free and an operator told to "top
up" will spend money and remain uncovered.

This variant is offered so the choice is informed; it is **not** assumed by any other part of this
specification.

**[Amendment, TES-fixable #4 — [D57] RESCOPED. As written this was a normative `must` the code
deliberately violates, and a specification cannot ship both it and the D31 paragraph below.]**
A **funded** tower's `watch_pass` MUST be package-aware (`submitpackage`, not per-tx
`transaction_broadcast_raw`). A **keyless** tower MUST NOT be: it has no funding input to spend, so a
package it could build would be a 1-parent-0-child package, i.e. the same broadcast with more moving
parts. The bundle carries **no** fee-child templates on either variant — see [D57] above. Multiple
towers compose idempotently on both.

Under the authority order the CODE is right here and the `must` was wrong: `transaction_broadcast_raw`
on the keyless default is the behaviour [D31] specifies, and the package path exists and is wired for
the funded variant.

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

**What remains genuinely unbuilt, stated against the paragraph above rather than around it.** The
owner's package path is NOT on this list any more: it is built, wired into both broadcast loops, and
verified live. Two things are left. (1) The **prepaid fee BOND** of the optional funded-tower variant —
its actuarial sizing and the operator-signed per-bump refund/rebate rail (O-5). `tower_float::Solvency`
and `plan_float` already answer *how much, in how many outputs*; nobody has priced the bond or built the
rebate. (2) **No E2E suite test exercises spike-time bumping.** The two tests that do
(`live_p2a_package_rescue.rs`, `live_tower_float.rs`) are node-gated: they need a Bitcoin Core RPC
endpoint and skip — loudly, printing why — without one, so a green suite run is not evidence the rescue
works. Separately, and by design rather than as a gap: the keyless default still walks the tiers with
per-tx `transaction_broadcast_raw` and attaches no P2A fee child. Under **D31** that is the correct
behaviour, not a missing feature — a keyless tower has no funding input to spend and would not bump even
if the code were reachable from it; what it now does instead is *say so* (see the stated-limit refusal
above) rather than retry forever.

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
acts personally. *Residual*: a hostile ex-owner can force a coin one 125-vB co-op de-trigger — and per
the correction in §5.8 that costs the attacker **nothing** at the committed rate, so footprint scales
with activity OR adversary spite. What is bounded is the damage, not the attacker: ~980 sats of fees and
anchors out of the coin per grief, and the coin fully restored.

**REQ 2 — no/low operator liquidity: MET.** No round liquidity (nothing expires), no denomination
pools (exact-amount Σ-conserving split/combine preserved verbatim), no leaf stock (all value is the
user's own coin). Operator money is fee-sized only: tower fee bonds (prepaid, priced into onboarding)
and P2A bump children. *Asterisk (accepted from review)*: contingent tower fee capital under a
correlated grief wave is real — ~$17/coin defended at 30 sat/vB; a 100k-coin wave ≈ $1.7M fronted.
Mitigated by committed base fees (base case needs no child), the de-trigger (defense is one 125-vB
co-op tier, not a **375-vB** pre-signed unilateral walk — *the 552 vB this line carried from the
original design doc is untraceable: it appears nowhere else in `docs/`, matches neither 3 tiers at
either tier size nor any tier-plus-children sum in §5.9, and the pass that replaced it could not source
it; 375 = 3 × `TIER_VBYTES` 125, which is the quantity the sentence is contrasting*), and the prepaid
bond rail (still unbuilt, O-5); it is capital-at-cost, never custody or principal.

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
  confiscation; bounded in *damage* (fee-sized sats per coin, §5.8) — **not** by the attacker's own
  spend, which is zero at the committed rate. (No 7-day exit stampede of
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
- **R-6 The enclave counter machine {level, m, k} does not exist — and is RETIRED BY PROOF, not owed.**
  What is attested today is a single lifetime `sig_count` per coin plus its budget; every structural
  expectation (which epoch, which level, how many tiers should exist) is reconstructed by the receiver
  from the bundle and its own preset. That is sound for the census — a total is exactly what an
  exact-equality count needs, because a hidden co-sign at **any** level raises the same total (D4's
  round-2 proof; SPEC-ROADMAP §6 scopes the machine out unconditionally, DECISIONS.md D22 records it as
  "no longer *needed*"). *An earlier revision of this bullet escalated it to "the safety-relevant state
  machine is still unspecified and unbuilt"; that was an unevidenced upgrade and is withdrawn.* Two
  things the proof does **not** cover, and they are the real residual: it is conditional on the two
  coordinator-trust premises D8 attaches — P3, that `se_num_sigs` is the true count, now earned by the
  **pinned-identity** `utexo/sig_count/v2` attestation ([D69]; §5.11), and **P1, the sid ↔ aggregate-key binding,
  still coordinator-supplied and unattested** (`ladder_binding_precheck_cause`); and per **D40.2** the
  key material and the counter it attests are held by the same party, which no counter machine would
  have fixed. See O-1, restated on those terms.

---

## 7. Footprint economics

Primitives: P2TR in 57.5 / out 43 / overhead 10.5 / P2A out 13 vB; **tier tx 125 vB** (measured through
the production finaliser; the 124 this table used to carry was the `SIGHASH_DEFAULT` witness model —
see §5.9), coloured tier **168 vB** (= 125 + one 43-vB `opret` output, exactly); **fee child 153 vB**
(measured); solo compaction 112 vB. All corrected figures from the economics review adopted (the draft's
58-vB batched claim and 0.04 vB/transfer are **rejected**; no output netting exists).

**7.1 Per coin** (the Baseline column is the pre-TES-R design of §2–§3)
| | Baseline (pre-TES-R) | TES-R (shipped) |
|---|---|---|
| Idle coin | 5,840 vB/yr | **0 vB/yr** |
| Active, 10 tx/day, default depth-3 policy | 5,840 vB/yr (rent dominates) | 3,650 hops/yr ÷ ~2,300/compaction ≈ 1.6 × 112 ≈ **~180 vB/yr** (≈0.05 vB/transfer) |
| Active, no-compaction policy | n/a | **0 vB/yr** on-chain; +250 vB contingent exit weight per 576 hops (~0.43 vB/hop, paid only if exiting unilaterally) |

**7.2 System, 1M-vB blocks, 52,560 blocks/yr, 10% of coins active at 10 tx/day (aggressive):**
- **1M coins**: idle 900k → 0; active 100k × ~180 ≈ 18M vB/yr ≈ **342 vB/block ≈ 0.034%** of block
  space (baseline: 111,111 vB/block ≈ 11.1% — a **~320× reduction**). Plus churn: 20%/yr
  deposit+exit at ~111 + 375–834 vB adds ~0.2–0.4% — present in every design including Spark.
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
  (125 vB each; and it must be priced as insurance against a **free** attack, not against one that
  loses money — §5.8).

**7.4 Tower economics**: monitoring is cheap and delegable (above); the priced item is contingent
defense capital (R-1), bounded per coin (≤ 3 × 153 vB × spike rate) and prefunded via the bond. A
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
(**[D36] NOT moot** — this used to read "moot for laddered coins, they have no calendar deadlines to
convey", and a laddered coin that has been received keeps `min(L_k)` from its retained flat backup
chain. Audit [17] therefore does *not* close by deletion, and not only on the un-laddered shape: it
survives on every received coin, absorbed by the `auto_exit_margin_blocks` default and by
`deadline_safety_due`, not solved. **[Corrected — the 288-block literal this document used
to quote is superseded.]** 288 = `k_max·interval + 144` spent a **single** confirmation window on a walk
that lands `3 + 2d` transactions one after another, each of which must confirm before the next tier's
relative lock even starts counting. The shipped default is DERIVED, not chosen —
`auto_exit_margin_blocks_for(k_max, interval, d) = k_max·interval + (3 + 2d)·144` — which at
`k_max = 14`, `d = 1` is **2,120 blocks on mainnet** (interval 100) and **860 on regtest** (interval 10).
The walk's own `Σ csv` is deliberately NOT in it: `auto_exit_due` takes that head start per coin, off the
coin's own chain, and subtracts it before comparing.)

### 8.3 TES as drafted — AMENDED, not adopted verbatim
Accepted and folded: renewal-auth replay fix; committed-fee restoration; package-aware funded towers;
latch-gated renewal; depth cap + DoS pricing; nostr-published constants (folded as design; the
publication half is still unbuilt and the receiver-side yardstick supersedes it — §5.2); batch
coordinator dropped;
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

**Phase 0 — ship, no behavior change** — *PARTLY DONE (SDK yes, server no — see the outcome)*: server:
generalized counters {level, m, k, total_sigs},
`POST /renew/init` (challenge-nonce rail), config + nostr record; SDK: TES-R builders in
`transaction.rs` (CSV tiers, v3, committed fee + P2A, public H_tag tweaks), the TES-R watch bundle +
package-aware `watch_pass` with fee wallet, R′ receiver checks behind a version tag in the transfer
message (receivers handle both ladder types during migration). Enclave: **cryptographically
unchanged** (blind MuSig2 + one-shot nonces suffice); only the counter state machine needs the
INV-23/24-grade spec (O-1) — no SGX rebuild required for launch, decoupling from mainnet-audit [13].
*Outcome*: the SDK half shipped; the SERVER half of this phase did not, and the *DONE* above overstated
it. Three deltas. (a) The transfer-message version tag that let receivers handle both ladder types is
gone with the escape hatch — receivers now dispatch on the coin's **shape** (laddered vs un-laddered),
not on a protocol number. (b) **The server line never landed.** There are zero occurrences of
`total_sigs` in the tree, no `POST /renew/init` route, no {level, m, k} counter machine, and no column or
migration for any of them; `/info/config` and the nostr record publish the flat parameters, not the tier
schedule (§5.2, §5.5). What ships in their place is the lockbox's attested lifetime `sig_count` plus the
`spend_budget` terminality receipt — and that is not a shortfall to be made good later: **the phase-0
server line was subsequently cancelled, not deferred.** D4's round-2 proof retired the counter machine
and `POST /renew/init` (SPEC-ROADMAP §6, DECISIONS.md D22), and the schedule-publication half was
superseded by the receiver-side yardstick of §5.2. The residual carried under the O-1 label is the trust
premise D40.2 names, not this build item.
(c) **`watch_pass` package-awareness is no longer the gap it was.** The 1P1C path exists
(`build_p2a_fee_child` + `core_rpc::submit_package`), both broadcast loops escalate through it when a fee
source is supplied (`watch_pass_with_bump` / `exit_pass_with_bump`), and it is verified live. The plain
keyless `watch_pass` still broadcasts tier by tier with `transaction_broadcast_raw` and attaches no
child — deliberately, per **D31** — and reports a fee-stuck tier as a stated limit. What is still open
under a sustained spike (R-4) is therefore narrower than "unimplemented": it is that the default tower is
keyless, and a keyless tower cannot bump at any level of implementation.

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

Not covered by any test in this suite, and honestly open: **package-relay tower defense** and P2A
fee-child attachment under a real fee spike. That one is no longer *unimplemented* —
`mercuryrustlib::core_rpc::submit_package` is the caller, `build_p2a_fee_child` builds and prices the
child, and both broadcast loops escalate through them (§5.13) — but the only tests that exercise it
(`live_p2a_package_rescue.rs`, `live_tower_float.rs`) require a Bitcoin Core RPC endpoint and skip
without one, so nothing here proves it. Also open: the **colored** de-trigger variant (§5.8), the
funded-tower fee BOND (O-5), and mass-grief prioritization (R-1 / O-6). Those remain design-level
claims. The earlier plan's "mixed-protocol estates" item is void: no mixed estate can exist.

---

## 10. Open problems & future work

- **O-1 — RESTATED. The build item is closed; the trust premise underneath it is what stays open.**
  ~~*(as it stood: "blocking, STILL OPEN — formal spec + audit of the enclave counter machine
  {level, m, k} … a missed interleaving is a double-spend, so this needs INV-23/24-grade treatment
  before mainnet"; and, briefly, "unbuilt, not merely unspecified … so this stays blocking")*~~ — the
  motivating worry, a missed interleaving, is precisely what the total-count census answers. There is
  no such machine in the tree (§5.5): the enclave keeps one lifetime `sig_count` and a `sig_budget`, and
  every structural expectation is reconstructed receiver-side. **D4's round-2 proof holds** — exact
  equality on the TOTAL detects a hidden co-sign at any level, and decoy-vs-hidden is separated by
  per-item validation, slot uniqueness over the union of live and disclosed tiers, and root anchoring,
  not by per-level counters — so SPEC-ROADMAP §6 scopes the machine and `POST /renew/init` out
  **unconditionally** and DECISIONS.md D22 records the same. Specifying and auditing a machine nobody
  will build was never the blocking item; it is deleted as one here rather than carried as a debt.
  *De-risked in practice*: the census is exercised adversarially by sdk54/sdk55/sdk58, across hops by
  sdk46/47/60/17, and for retry-idempotence of the count by sdk56.
  **What remains open under this label, per D40.2 (which folds O-1, CO-1 and CO-3 into ONE defect —
  publish one row, cite it three times):** the enclave key material and the counter it attests are held
  by the party the receiver is being protected from. The proof must therefore be published *with* its
  premises — P3 (`se_num_sigs` is the true count) is now **earned** by the **pinned-identity**
  `utexo/sig_count/v2` attestation ([D69] — pinned, not chain-anchored: a deep in-ladder-split ancestor
  has no chain anchor by design), while **P1 (the sid ↔ aggregate binding) is still coordinator-supplied
  and unattested**. The only construction that closes it is a second, independently administered SE write
  domain under a separate legal entity; an external anchor over `(sid, n, h_n)` is rejected with reasons
  (the attack is *under*-reporting, and the receiver's rule would be a floor written by the adversary).
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
  chain (§5.4). The N-hop census generalizes, but the interaction of *depth* with the **derived** depth
  cap of §5.11 (`max_split_depth` — there is no literal to compare against, and the number moves with the
  network profile) and with the min_child_value floor (D1) at each level is only exercised to depth 2 (sdk17,
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
exit-only (§5.4), and TES-fixable #4 (package-aware tower broadcast) **is now built and wired**
(§5.13, §9 Phase 0) — the owner-funded 1P1C path exists, escalates from both broadcast loops, and is
verified live against a node; only the funded-tower fee BOND (O-5) is still specification.

The places where this document still describes something that does not exist in code are named where
they bite, not hidden here: the renewal RAIL of §5.5 steps 1 and 3 (no `/renew/init`, no `total_sigs`,
no {level, m, k} machine — O-1), and the publication of the tier schedule by the SE (§5.2). **Neither is
"design that stands while the code catches up"** — the verdict an earlier correction pass attached here,
withdrawn: the renewal rail was **decided out** (D4's proof, SPEC-ROADMAP §6, DECISIONS.md D22), and the
publication half was **superseded** by a receiver-side yardstick that is strictly stronger because it
holds against a lying coordinator (`cap_schedule` / `TesrParams::flat_ladder_params`, the D27 pattern).
Both are kept above as design of record — one dropped, one replaced — and the only rail still worth
building from either is the audit-[15] single-use nonce on `/sign/first`.*

*Adversarial findings ledger: TES review-1 FATAL#1 defanged (§5.8), FATAL#2 accepted-as-tolerated
(§5.7); TES review-1 fixables 1–7 folded; TES review-2 corrections (compaction 103 vB, exit 828 vB,
δ-sensitivity, policy dependence, m≤floor-2, migration self-funding) folded; RGB fork resolved by
deletion + terminal-freeze (§5.10); Grove review-2 salvage (trigger-ladder standalone, co-op
de-trigger, tower bond, onboarding-fee framing) adopted; Grove factory + C-90 rejected with reasons
(§8).*
