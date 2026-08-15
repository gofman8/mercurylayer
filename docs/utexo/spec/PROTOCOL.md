# Utexo TES-R — the protocol

**TES-R** — *Trigger / Extension / State with self-split Rollover* — is the protocol. `claim()`
establishes a trigger/extension/state ladder for **every fresh confirmed ROOT coin, unconditionally**.
There is no protocol-version field and no escape hatch that opts a deposit into a flat un-laddered
shape; no test pins any other lane.

**One protocol, two coin SHAPES.** "Un-laddered" is a *shape*, not a legacy lane:

- **Laddered**: every plain deposit — trigger T → extension X_m → state S, relative CSV, un-broadcast.
- **Un-laddered**: an **RGB carrier** is deliberately never laddered (a plain tier spend would destroy
  the allocation — the terminal-freeze rule of §5.10; pinned by `sdk52`), and a **split sub-coin whose
  funding is un-broadcast** cannot root a trigger (B0 — a trigger over an unconfirmed prevout is
  unbroadcastable). These keep the signed-once backup and transfer by backup-chain handover. That path
  is **load-bearing for RGB tokens**, not dead code, and it keeps the calendar machinery (root
  deadline, `auto_exit_due`) that laddering removes on the CSV side.

> ### Direction of travel: ONE COIN TYPE
>
> Two coin shapes is a transitional state, not the target architecture. The decided direction is a
> single coin type; the un-laddered shape is to be removed, not kept.
>
> The mechanism is **CTES-R** — colour every tier so an RGB carrier can be laddered, retiring
> terminal-freeze. Its foundation has landed (the `payload_vout` migration, the coloured tier builder,
> per-tier seal blinding). **The colouring itself is not yet wired.** Until it is, everything below
> about the un-laddered shape is accurate as-built.
>
> Reaching one coin type also requires porting `verify_bundle` to wasm/JS and to Kotlin: the nodejs
> and web clients currently **refuse** any laddered coin.

Companion documents: `SPEC.md` (normative specification), `TRUST-MODEL.md` (trust boundaries and
residual risks), `CHILDREN.md` (first-class received children), `LIGHTNING.md` (the HODL-invoice
latch), `PARTIAL-PAYMENT-ECONOMICS.md`, `README.md` (index).

---

## 1. The 4 requirements (+ the delegation refinement)

1. **Off-chain forever, unlimited off-chain state transitions.** Refinement: periodic on-chain is
   acceptable **iff** (a) footprint scales with *activity*, not time — idle coins cost ~nothing — or is
   heavily amortized/batched, **and** (b) users never act personally: an operator/tower does it, priced
   into onboarding/transaction fees.
2. **No/low operator liquidity.** No Ark-style round liquidity, no SSP denomination pools, no
   SuperScalar leaf stock. Native exact-amount split/combine (value conserved from the user's own
   coin) must survive.
3. **Non-custody under operator hack.** A hacked operator may steal *future* deposits and *future*
   state transitions, but pre-hack state left untouched must be absolutely safe (2-of-2, share
   rotation s+e=const, enclave deletion).
4. **Blind operator for RGB.** The SE may know everything about sats; it must never learn RGB
   contents in-system (blind co-signing preserved). Exit-time RGB disclosure is acceptable.

**Hard constraints honored**: no soft forks (no APO/CTV/CSFS; TRUC/v3 + P2A are relay policy and ARE
used); single SE + enclave; blind co-signing, exact-amount split/combine DAG, RGB on un-broadcast
carriers, Lightning latch all kept; towers delegatable + keyless, mandatory-forever acceptable;
unilateral exit always exists for the current owner; **missed liveness must never mean
confiscation-by-design** — nothing in TES-R ever pays the operator by timeout.

---

## 2. Why an absolute-timelock ladder cannot scale (the block-space wall)

This is the reason the protocol uses relative locks. An **absolute** ladder anchors every coin's
decrementing nLockTime chain to its deposit height, so staying alive requires an on-chain re-anchor
(112 vB) per coin per ~initlock (~1,008 blocks ≈ 7 days), regardless of activity:

- **Idle rent**: 112 vB × 52,560/1,008 ≈ **5,840 vB per coin-year** — independent of whether the coin
  ever moves.
- **1M coins**: 1M × 112 / 1,008 ≈ **111,111 vB per block ≈ 11.1%** of all Bitcoin block space.
- **10M coins**: ≈ **111%** — physically impossible.
- **Prefunding the rent** (the "hide it in onboarding" route): 5 idle years at an average 10 sat/vB
  ≈ 292,000 sats ≈ $292/coin @ $100k/BTC — exceeding the entire value of most retail coins. The
  refinement's clause (b) cannot rescue clause (a): delegation changes who *pays*, not how much chain
  space is *burned*.

The root cause is architectural: **absolute timelocks age while un-broadcast**. The clock runs on the
calendar, so the defense must be renewed on the calendar. The rent is structurally
O(coins × time / initlock); no scheduling trick divides it away (§8.2). The only cure is to change the
timelock type: **relative (CSV) locks on un-broadcast transactions do not tick until a parent
confirms** (BIP-112). Then the clock starts on *attack*, not on deposit — and idle coins never age at
all.

---

## 3. Comparison landscape

The **absolute ladder** column is the alternative of §2 — every coin anchored to a decrementing
absolute nLockTime ladder from its deposit height, with a calendar deadline and idle rent. It is the
measuring stick for every number in this design, not a shipped option.

| | **Absolute ladder** | **Spark** | **Ark** | **SuperScalar** | **TES-R** |
|---|---|---|---|---|---|
| Idle on-chain footprint | 5,840 vB/coin-yr (112 vB per ~7d) | **0** | Refresh per round or lose funds | Per-factory ladder txs (~monthly, amortized /N) | **0** |
| Renewal mechanism | On-chain re-anchor per coin | Off-chain re-sign with operator group (`renew_leaf`) | On-chain round through ASP | New factory before dying period | **Off-chain**: DW lower-CSV extension re-sign; self-split rollover at epoch exhaustion |
| What invalidates old state | Consensus (absolute locktime ordering) + enclave refusal | **1-of-n operator honest key deletion** (trust) | Round expiry (consensus) | Decrementing nSequence DW (~64 updates then exhaustion) | **Consensus** (lower-CSV wins the trigger output; old epochs' parents unconfirmable) + the receiver's exact-equality census over the enclave's attested `sig_count` as defense-in-depth (§5.11) |
| Missed-liveness outcome | Raceable after window; never confiscated | Safe (operator renews); trust-dependent | **CONFISCATION** — funds sweep to server after expiry | **CONFISCATION** — the LSP can claim the entire UTXO after the dying period | Raceable **only after** a public on-chain trigger + ≥144-block CSV notice; **never confiscated** — no output ever pays the operator by timeout |
| Operator liquidity | None | SSP denomination-swap pools | Round liquidity fronted per refresh | LSP leaf liquidity | **None** (fee-sized outlays only; §7.4) |
| Exact amounts | Native split/combine | Fixed denominations + SSP swaps | Fixed by round | Fixed by leaf | **Native split/combine preserved** |
| Operators | 1 (blind SE + enclave) | t-of-n group | 1 ASP | 1 LSP | 1 (blind SE + enclave) |
| Unilateral exit wait | ≤ ~7 d | Relative timelock wait (2000→ blocks, decrementing) | Before expiry only | O(log N) txs, force-exits sibling subtrees | E+D sequential CSV: ≤15 d flat, decreasing with activity; deeper for sub-coins (§5.9) |
| Watching duty | Deadline-bounded (~7 d windows) | Operator-dependent | Deadline-critical (miss = lose) | Once per factory lifetime (miss = lose) | Perpetual but **event-driven** (alarm = public trigger tx; ≥1 day notice), keyless, delegable |

Honest placement: **Spark is the footprint benchmark** (zero periodic on-chain), bought with 1-of-n
key-deletion trust for invalidation and fixed denominations. **Ark and SuperScalar both have
expiry-confiscation** — disqualified outright by our constraints. TES-R reaches Spark-class footprint
(idle exactly 0; ~0.06 vB per transfer amortized) with **consensus-level invalidation** and **no
denominations**, at the cost of perpetual (but alarm-driven, keyless, cheap) watching and longer
unilateral-exit latency.

---

## 4. Architecture selection

TES-R is a composition: a CSV trigger ladder as the chassis, plus the cooperative de-trigger (§5.8)
and the off-chain self-split rollover (§5.6). Two structures are rejected on today's Bitcoin: the
**shared-UTXO factory** (a never-rotating n-of-n root that a Sybil cohort can fresh-spend — an
unfixable REQ-3 break, revisitable only under covenants, §10) and the **evolved absolute ladder** (the
rent wall of §2 is structural, and no covenant changes that). Both rejections are stated in full in §8.

Two classes of attack are *accepted* rather than eliminated, and both are quantified below:
**trigger griefing**, which is survivable and bounded but not costly to the attacker (§5.8), and the
**offline-victim race** after a public trigger, which is the race class the hard constraints tolerate
and against which TES-R gives ≥144 blocks of on-chain notice (§5.7).

---

## 5. The architecture: TES-R

### 5.1 What is carried over unchanged

Single blind-MuSig2 SE (signs only 32-byte sighashes); key-share rotation s_n + e_n = const with
enclave deletion of old shares; per-node spend budgets / terminality with public receipts
(`GET /statechain/spend_budget`); receiver-verifies-everything at claim; keyless delegatable watch
bundles (REQ-34); RGB allocations anchored to (possibly un-broadcast) carrier txs with P2P
consignments; Lightning latch; exact-amount split/combine DAG with Σout value conservation. The
enclave's cryptographic core is **unchanged** — blind MuSig2 is sequence-agnostic.

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

| Parameter | Value | Meaning |
|---|---|---|
| D0 / δ / D_floor | 1,440 / **36** / 144 blocks | state tier: 36 hops per epoch, ~6 h head start per hop |
| E0 / δE / E_floor | 720 / **36** / 144 blocks | extension tier: 17 epochs; **forced rollover at m = 15** (never operate at the floor with a minimal edge) |
| Hop budget per depth level | 36 × 16 = **576 transfers** | between depth-increments; see §5.6 |
| Worst flat unilateral wait | E0 + D0 = 2,160 blocks ≈ **15 days**, decreasing 36 blocks/hop | |

These are `TesrParams::mainnet()` in `lib/src/tesr.rs` verbatim (d0 1440 / δ 36 / d_floor 144,
e0 720 / δE 36 / e_floor 144, m_max 15, committed fee 3 sat/vB); a `TesrParams::regtest()` preset
(24/6/6, 12/3/3, m_max 2) exists only so a full lifecycle fits in a test's mining budget. `sdk44`
pins the schedule arithmetic (`state_csv` / `ext_csv` / `needs_renewal` / `needs_rollover`).

δ = 36 (≈6 h) rather than 24 (≈4 h): the head start is the single parameter everything stands on, and
mainnet has sustained >4 h full-block spikes. The budget sensitivity is stated honestly: δ = 24 →
1,350 hops/level; **36 → 576**; 72 → 162; 144 → 45. Because rollover (§5.6) is off-chain, even
conservative δ only trades exit weight, never chain rent.

**The SE does not publish the tier schedule, and the receiver does not need it to.** `/info/config`
serves only `initlock`, `interval`, `batchtimeout` and `version`
(`server/src/endpoints/utils.rs`), and the NIP-100 nostr record carries the flat `timelock` plus
fee/status metadata (`server/src/main.rs`). What defends a receiver against a per-victim parameter
split is receiver-side: a schedule that travels on an artifact is data, never a yardstick.
`cap_schedule` measures every conveyed `TesrParams` against the receiver's OWN network preset field by
field and refuses by name on the first disagreement ("`d0` = 24 where `bitcoin` says 1440"), and
`TesrParams::flat_ladder_params` is the single authority for the flat pair — the coordinator refuses
to boot against an env that disagrees with it (ci-guard `deny_flat_ladder_config_drift`). This is
strictly stronger than publication, because it holds against a lying coordinator.

**The core property**: all three tiers are un-broadcast; BIP-112 CSV ticks only after the parent
confirms; T has no timelock. So **nothing anywhere matures until someone broadcasts T on-chain**. An
idle coin — and an entire idle split/combine DAG — never ages **on the CSV side**: no refresh rent, no
root-deadline materializations.

**Calendar deadlines are NARROWED, not deleted.** The ladder is not the coin's only pre-signed
material: the FLAT BACKUP CHAIN is retained, and it carries an absolute locktime which each whole-coin
hop shortens by `interval`. A coin that has been received `k` times therefore sits on `min(L_k)` — a
real, finite, approaching calendar height held by its PRIOR OWNERS — whatever its ladder is doing.
`SDK_E2E=86` measures exactly this: the tip advances toward that height and each hop spends `interval`
of it. What TES-R deletes is the CSV-side ageing; what survives is `min(L_k)`. `TRUST-MODEL.md` B4(ii)
and `SPEC.md` §1 carry the same statement, and the un-conveyed-ancestor-locktime residual it leaves
open is `TRUST-MODEL.md` B6 — narrowed by laddering, not closed by it.

On the **un-laddered** shape (RGB carriers, §5.10; sub-coins over un-broadcast funding, B0) the coin
rests on the signed-once **absolute-locktime** backup, so it has a root deadline and needs
materialization before it. `auto_exit_due` and `exit_deadline_block` are scoped to that shape
(`SdkConfig::auto_exit`, default on), and are exercised live by `sdk34` (carrier watchtower
materializes before the deadline) and `sdk32` (the residual clawback window on an un-laddered
carrier). A laddered coin has no such deadline for them to act on — it has `min(L_k)`, defended by
`deadline_safety_due`.

### 5.3 Fees

Every pre-signed tx (T, X, S, SP, CB) is **nVersion=3 (TRUC)** and carries:

1. a **small committed fee** drawn from the coin (target 3 sat/vB at signing; Σout = Σin −
   fee_committed) so the base case relays and confirms standalone — the same self-funding property the
   signed-once backup chain has; and
2. a **240-sat P2A anchor** (OP_1 0x4e73, anyone-can-spend) so the owner — or an operator's funded
   tower on their behalf, but **not** a keyless one (§5.13) — attaches a live-rate fee child during
   spikes. That child is **153 vB** (11 base + 41 P2A-in + 58 funding-in + 43 change),
   `estimate_child_vsize`, and the estimate was measured against a real signed child on regtest
   (153 estimated, 153 actual) rather than modelled.

TRUC's 1P1C topology + sibling eviction gives pinning resistance (each tier confirms before the next
is valid, so no long vulnerable chains); v3+P2A is relay *policy*, not consensus — the
miner-direct-submission fallback is documented, the same bet Lightning anchors make. The committed fee
is small and bounded-stale (it is a floor, not the whole fee), so a stranded-reserve pathology does
not arise.

### 5.4 Splits, combines, sub-coins

A split SP is a state-tier tx: spends X_m.out[0] at nSequence Δ_{k+1} (one decrement below the
splitter's own retained state), N child resting outputs with **exact amounts, Σout = Σin −
fee_committed**, + P2A. Parent terminalized at the SE before co-signing. Each child resting output
hosts its own extension + state tiers — **no trigger needed**: SP is itself un-broadcast, so nothing
ticks until it confirms. A combine CB carries per-input nSequence = that input's Δ (BIP-112 is
per-input); Σ-inputs terminal-ancestor verification (R7) unchanged. Child coins get fresh slots via the
free derived-token path (REQ-35).

**The in-ladder split is the only split.** A split is a **state tier** (`SP` spending `X_m.out[0]`),
i.e. a *descendant* of T, never a rival for F. That is what closes the theft vector: a past owner's
retained no-timelock trigger has nothing to race, because the split does not compete for the funding
outpoint. Attack-proven by **sdk58** (11 adversarial cases, all REJECT) and **sdk59** (end-to-end split
payment), with the bundle/backup-chain adversarial cases in **sdk54** / **sdk55**.

**Admission floor.** A child is not a bare output: `establish_child` hangs the child's OWN extension +
state tiers off `SP.out[j]`, each burning committed fee + P2A, and the final state output must still
clear dust. The floor is `mercurylib::tesr::min_child_value(fee_rate, dust)` — **1 560 sat at the shipped 3 sat/vB** — and it is checked **before** the parent is terminalized, so a piece too small to establish
never strands its parent to unilateral-exit-only.

**Received children are FIRST-CLASS** (`CHILDREN.md`). The child claim completes the standard SE key
handover: `A_child` is invariant across the rotation (`sender_share + SE_old == receiver_share +
SE_new`), which is exactly what keeps the pre-signed child exit chain valid, while the sender is
permanently locked out (auth rotated). A child is then payable onward off-chain — **whole** via
`child_retransfer`, or **split** via `child_in_ladder_pay` (a depth-2 `ancestors` chain). Each hop
costs exactly **one co-signature** and discloses exactly **one superseded state**, which the receiver's
census counts and proves out-raced. Evidence: **sdk60** (alice→bob→carol whole re-transfer, the funding
outpoint unspent throughout) and **sdk17** (partial second hop). The **child is deliberately not
terminalized** — the census closes any pre-conveyance rival and the handover closes every later one;
the one exception is the Lightning-latched piece (§5.12), which stays terminalized.

**B0 — root-only laddering.** `claim()` ladders a coin only when its funding output is a **confirmed
on-chain root**. A sub-coin whose funding is an un-broadcast split output cannot root a trigger (the
trigger would have no prevout to spend, and a v3 tier cannot relay over an unconfirmed v2 parent), so
such a coin travels the un-laddered shape instead — checked fail-closed against electrum, never
inferred.

### 5.5 Renewal (pure off-chain; the common case)

When Δ_{k+1} would fall below D_floor, the SDK runs renewal inside `transfer()`, with **zero on-chain
bytes**. Renewal as it ships is **two blind MuSig2 co-signs**: X_{m+1} spending T.out[0] at CSV
E0 − (m+1)·δE, then the transfer's own state S'_0 on X_{m+1}.out[0] at CSV D0. Key rotation covers all
tiers (tweaks are public). `mercuryrustlib::tesr::renew` is exactly two `cosign_tier` calls over the
ordinary `/sign/first` + `/sign/second` pair; `m` and `k` are fields of the client's own bundle,
advanced locally.

**There is no SE-side renewal counter machine, and none is planned.** There is no `POST /renew/init`
route anywhere in the tree, no `total_sigs` column or field, and no {level, m, k} counter machine. The
census (§5.11) does not depend on one: exact equality on the TOTAL detects a hidden co-sign at **any**
level, so per-level counters add nothing the total does not already give. What the SE does enforce
around a co-sign:

- `POST /sign/first` returns **401** unless the request carries a schnorr signature by the coin's own
  auth key (`validate_signature`, `server/src/endpoints/sign.rs`), and every co-sign still needs the
  owner's MuSig2 partial. `m`/`k` are client-local bundle fields the SE never reads, so no
  coordinator-side epoch-burn lever exists.
- `acquire_signfirst_lock` fails **closed** with a retryable 503 rather than proceeding unserialized.

**Known gap — replay.** `/sign/first` takes the replayable `signed_statechain_id`, not the single-use
endpoint-bound `validate_signature_nonce` that `withdraw/complete`, `deposit/get_derived_token` and
`statechain/spend_budget` already use, so a captured request can be re-served. Adding that nonce to
`/sign/first` is the one renewal-side rail still worth building.

The counts the SE publishes are two, and both are load-bearing: the lockbox's **lifetime `sig_count`**
(attested `utexo/sig_count/v2`, served through `GET /info/statechain/<id>` as `num_sigs`), which is the
right-hand side of the receiver's census (§5.11); and the per-node `sig_budget` / terminality receipt
at `GET /statechain/spend_budget/<id>`, which the enclave itself re-checks before consuming a secnonce.

**Why old state dies at the consensus level, not by enclave promise**: X_{m+1} strictly undercuts
every older extension in the CSV race for T.out[0]; every pre-renewal state hangs on an extension
that can now never confirm. This is Decker-Wattenhofer replace-by-lower-timelock at one dedicated
tier — renewal replaces horizontally, tree depth stays constant across all epochs. Honesty note:
"consensus-dead" is *race-conditional* — the newest extension must win a ≥36-block-edge race after a
public trigger. That is strictly stronger than an absolute ladder's post-window position and
categorically stronger than a key-deletion promise, but it is a race, not an axiom.

**There is no "enclave single-active-state refusal".** The enclave does not track which state is
current and cannot refuse a rival one; it *must* co-sign rivals, because that is what a renewal IS (a
lower-CSV extension over the same outpoint). What the enclave enforces is two different things, both
real and neither of them a rival defence: **one signature per secnonce** — the sealed secnonce is
loaded and consumed in the same row-locked transaction, and a second partial signature over a
different challenge finds it NULL and is refused (`lockbox/src/server.cpp`) — which is a MuSig2
nonce-reuse *key-leak* defence; and a per-coin **`sig_budget` vs lifetime `sig_count`** check, run
before the secnonce is consumed, which is what makes terminalization (and the refusal of renewal on
terminal nodes, §5.10 rule 2) enforceable rather than promised. The second layer over the consensus
race is **the receiver's exact-equality census** over that attested `sig_count` (§5.11): an undisclosed
rival state cannot be hidden, because it shows up as a count the disclosed tiers cannot account for.

**Evidence**: **sdk51** — someone else spends F (a prior owner racing a stale state, or a griefer); the
owner runs only `defend_ladders()` and the strictly-lowest-CSV current state matures first, so the
funds land at the owner's key. **sdk40 PART 1** — BIP-68 is enforced by real consensus (X is rejected
before E confirmations of T, S before D of X) and nothing ages while un-broadcast. **sdk40 PART 2** — a
stale ladder is killed outright at the consensus level (X′ can never confirm once its prevout is
gone). **sdk43** — unbounded off-chain renew → rollover → renew, zero on-chain bytes, then a unilateral
exit through the whole deep chain. **sdk42** — the full laddered lifecycle.

### 5.6 Rollover at epoch exhaustion

There is no mandatory on-chain compaction. At m = 15 the owner (SDK, automatic, inside a transfer)
co-signs a 1-in-1-out **off-chain self-split rollover** SP_roll consuming the current state slot, whose
child resting output hosts fresh extension + state tiers (fresh 576-hop budget). Cost: zero on-chain;
+2 pre-signed txs (250 vB) of *contingent* exit weight and +1 depth level. The parent is terminalized —
frozen forever (§5.10).

**Compaction is an optional depth-capping policy, not a liveness requirement.** Default SDK policy:
when depth > 3, the next transfer triggers a **solo compaction** = an on-chain re-anchor (`refresh`:
112 vB, non-interactive), reborn at depth 0, priced into that transfer's fee.

An interactive MuSig2 batch coordinator is **not shipped**: batching saves only 112 → ~103 vB/coin
(57.5 in + 43 out + amortized overhead; there is no output netting) while interactive dropout math is
brutal (0.95^50 ≈ 7.7% clean-batch probability). Solo compaction inside `transfer()` costs +0.005
percentage points of block space and needs no coordinator service, rebuild protocol, or privacy leak.
If batching is ever built, the fresh T' must be pre-signed against the known template output *before*
broadcast, closing the B5 window.

Net budget between on-chain touches (default policy): 576 × 4 depth levels ≈ **2,300 transfers per
112 vB** — and a user who tolerates depth may set the cap higher and touch the chain *never*.

Rollover runs unattended inside `transfer()`: **sdk43** drives renew → rollover → renew past epoch
exhaustion and shows the funding outpoint untouched. The solo-compaction path is the re-anchor of
**sdk30** (both fee models). `refresh_sponsored` sizes the operator's rebate as
`max(fee + DUST_LIMIT, min_child_value)` so it clears the admission floor of §5.4; the operator absorbs
the difference.

### 5.7 Race analysis

| Adversary | Holds | Attack | Honest defence (keyless-tower executable) | Winner | Watching assumption |
|---|---|---|---|---|---|
| Previous owner, same epoch | T, X_m, stale S_j | Broadcast T; at +E_m broadcast X_m; wait Δ_j | Tower sees F spent (loud, on-chain); **preferred: co-op de-trigger (§5.8)**; else broadcast S_k at +Δ_k | **Honest**, ≥36-block (~6 h) CSV edge per tier + fee race | ≥1 tower reacting within a window that opens with ≥E_m + Δ_k ≥ 288 blocks (~2 days) of total notice |
| Previous owner, old epoch m′ | T, X_{m′}, old states | Broadcast T; wait E_{m′} | Co-op de-trigger, or broadcast X_m at +E_m — every epoch-m′ state's parent becomes unconfirmable | **Honest**, edge 36·(m−m′) ≥ 36 blocks | Tower awake within the edge, after ≥E_m ≥ 144 blocks of prior notice |
| Ancestor's past owner vs my sub-coin | Root chain + stale S_j at some level | Confirm T, X; broadcast stale S_j before my SP | Tower broadcasts SP at Δ_{k+1} (undercuts by ≥36); per-tier repeat | **Honest**, per-tier ≥36-block edge. **A CALENDAR DEADLINE DOES EXIST**: the coin still sits on `min(L_k)`, the retained flat-backup height held by its prior owners | Per-tier reaction; ≥144 blocks notice before ANY hostile tx is final — but only until `min(L_k)` |
| Hacked SE alone | e_n (1 of 2-of-2) | Nothing signs; can only refuse (freeze ≠ seize) | Unilateral tree pre-signed, needs nobody | **Honest, unconditionally** | None |
| Hacked SE + past owner w/ retained pre-rotation share (**B1**) | Full key of F | Fresh no-timelock spend, private relay | Race with pre-signed material; blindness blocks target selection; public counter receipts attribute | **Adversary favored** — this is the statechain trust unit, unchanged in kind and mitigations | Enclave deletion + attestation ops; settled coins immune |
| Hacked SE + thief, coin received PRE-hack, untouched | e_n only | Cannot produce owner's partial | 2-of-2 arithmetic | **Honest, unconditionally** (REQ-3 core case) | None |
| Trigger griefer (any past owner, anonymous) | T | Broadcast T to force cost | **Co-op de-trigger: 125 vB, the value comes out (§5.8)** | **Honest** on the coin (nothing is stolen), but **not "economically losing" for the attacker**: at the committed rate he pays 0 and the coin pays ~980 sats in fees + anchors (§5.8) | SE alive; else falls to row 1 |
| Malicious/buggy tower | Bundle (all pre-signed, pays owner only) | Broadcast early / not act | — | **Honest** (early = settles to owner; inaction = no tower) | — |
| Mempool pinner | — | Pin packages | TRUC 1P1C + sibling eviction + committed base fee + P2A | **Honest** | Standard fee-bumping |

**Where each row is exercised**: rows 1–3 (stale state / old epoch / ancestor race) — **sdk51** (a
watchtower defends against a hostile trigger end-to-end) and **sdk40 PART 2** (the stale state dies at
consensus); the honest defence's pre-signed material — **sdk50** (unilateral exit through the full tier
chain) and **sdk45** (the keyless bundle drives it with no key material); adversarial rejection of
forged/hidden structure — **sdk58** (11 cases), **sdk54**, **sdk55**. Rows 4–6 (hacked SE) are
arithmetic properties of 2-of-2, not test-observable beyond the co-sign gates (**sdk56** / **sdk57**,
retry-idempotence and owner-share binding).

**Row 3, stated precisely, because an implementer can get it backwards.** The distinction is not
laddered/un-laddered: **every coin that has been RECEIVED keeps `min(L_k)`**, because the flat backup
chain is retained alongside the ladder and each hop shortens its absolute locktime by `interval`.
Laddering removes the CSV-side ageing and nothing else. `deadline_safety_due` is the ONLY scheduled
defence of `min(L_k)` on a laddered coin and must not be omitted. `SDK_E2E=86` is the measurement;
`TRUST-MODEL.md` B4(ii) is the trust-model statement; `SPEC.md` §1 carries it in the specification.

**The traded property, stated plainly**: an unconditional ~7-day no-watch window is exchanged for
*perpetual but alarm-driven* watching. No theft tx can become valid until ≥144 blocks (~1 day) after a
**publicly visible on-chain trigger** spends F — strictly more defender notice than silent calendar
maturities and post-window minutes-scale mempool races. Missed liveness is never
confiscation-by-design: nothing expires, nothing sweeps to the operator; loss requires a real adversary
winning a telegraphed public race while every tower slept through ≥1 day of alarm. This is the race
class the hard constraints tolerate, answered with cheap, prefunded, redundant towers (§7.4).

### 5.8 Cooperative de-trigger

T's output pays the coin's own aggregate (A + public tweak). A fresh cooperative co-sign has **no
timelock**, and no pre-signed extension is valid before CSV E_m ≥ 144 blocks. Therefore, on any hostile
trigger the owner (or SDK automatically) + SE key-path-spend T.out[0] immediately — one tier-shaped v3
tx, confirmed unopposed inside the ≥144-block window during which no adversary tx is valid.

`build_detrigger` emits a *tier*, anchor and all (`build_tier_tx`, payload out + 240-sat P2A), so it is
**125 vB** (`TIER_VBYTES`, measured), not the 111 vB of a bare 1-in-1-out co-op spend; the coloured
variant is a coloured self-transition at **168 vB** (`COLORED_TIER_VBYTES` — an `opret`, not a tapret,
and exactly one P2TR output wider than the plain tier). Consequences:

- **Trigger griefing is SURVIVABLE — the victim's value always comes out**: it answers with one
  125-vB de-trigger, and the grief is fee-attributable on-chain even though T itself is anonymous.
- **Griefing is NOT economically losing for the attacker.** Both transactions pay out of the **coin**:
  a tier's fee is committed at signing (`tier_out_value` = prev − committed_fee − P2A), and that is as
  true of the hostile T as of the de-trigger. At or below the committed 3 sat/vB the attacker
  broadcasts a transaction he already holds and pays **nothing at all**, while the coin loses
  2 × (250-sat committed fee + 240-sat anchor) ≈ **980 sats**. The attacker begins paying only above the
  committed rate, where T no longer relays on its own and needs a CPFP child he must fund — but at that
  same rate the victim's de-trigger is itself a stuck tier needing an **owner**-funded child (§5.13).
  So griefing is **cheap-to-free, and bounded**: the damage is fee-sized sats out of the coin, never
  the coin.
- **This calculus holds only while every spend of `F` a prior owner still holds is PLAIN.** A retained
  *coloured* backup would turn the same ~112-vB spend from destruction into capture of the whole
  allocation, at which point every number above describes the wrong quantity entirely. §5.10 rule 6 is
  what keeps the premise true.
- **Mass-grief saturation** (residual, quantified): 1M simultaneous triggers force ~125M vB of co-op
  responses within a ~144-block grace ≈ **87%** of a day's block space (144M vB) — strained but
  survivable. The triggers themselves are another ~125M vB of block space, but the attacker does not
  *pay* for that at the committed rate: those fees come out of the victims' coins. Beyond ~1M/day the
  response degrades to prioritized fee auction on the highest-value coins. If the SE is
  *simultaneously* dead, de-trigger is unavailable and rows 1–3 apply per coin. This correlated
  scenario is the design's worst case and is residual risk R-1 (§6).
- The de-trigger requires SE liveness — it is a UX/cost shield, never a safety dependency (the
  unilateral tree always exists).

**What is built and proven.** `mercurylib::tesr::build_detrigger` builds it; `cosign_detrigger` is
wired through `UtexoWallet::detrigger_to_owner` and driven end to end by **`SDK_E2E=89`**: a griefer
confirms `T`; the owner answers with a de-trigger spending `T.out[0]` at no relative timelock; it
confirms; the value lands at an address the OWNER named; and the pre-signed extension is then submitted
to the node and REFUSED — `bad-txns-inputs-missingorspent`. The old ladder is dead, measured against
bitcoind rather than inferred. **sdk40 PART 2** independently proves the consensus half.

**What is NOT built.** There is **no restoration half**: the de-trigger does not spend into a fresh
funding output `F′` and does not rebuild `T′/X′_0/S′_0` on this lane. So the specification may say:
*the owner chooses when the coin lands, in two transactions with zero CSV wait, and every retained tier
dies with it.* It may **not** say "the ladder resets fresh" or that the victim keeps off-chain-ness —
getting back off-chain after a de-trigger is a fresh deposit. The **coloured** de-trigger variant
(168 vB `opret`) is wired only as `cosign_colored_detrigger`, reachable only from `colored_reanchor`,
and is **not test-covered**; neither is the mass-grief prioritization policy (R-1 / O-6).

### 5.9 Exit costs

- **Cooperative** (normal path): 1 fresh co-signed tx ≈ 111 vB, instant, batchable.
- **Unilateral, flat coin**: 3 pre-signed txs (T+X+S = **375 vB**, self-relaying via committed fees) +
  0–3 P2A children (153 vB each) in spikes → **375–834 vB**; wait = E_m + Δ_k sequential, worst
  2,160 blocks (~15 days) fresh, decreasing 36 blocks per hop/renewal. With committed fees the base
  case is 0 children; in a spike, all 3. The signed tier is **125 vB**, measured through the production
  finaliser: TES-R hashes with `TapSighashType::All`, so the witness carries the explicit `0x01`
  sighash byte (a 64-byte `SIGHASH_DEFAULT` witness would be 124 vB and would commit 248 sat to a
  125-vB transaction — 1.984 sat/vB — silently breaking the property the committed fee exists for).
- **Unilateral, depth-d sub-coin**: 3 + 2d txs = **293·d + 375 vB** (`tesr_exit_vbytes`: T, X and the
  final state at 125 each, and per level an `SP` — the only rung with two payload outputs, 125 + 43 —
  plus one 125-vB extension); worst wait ≤ (d+1)·2,160 blocks — depth-3 ≈ 60 days. This is the honest
  cost of relative timelocks (Spark shares the class). Mitigations: cooperative exit covers the normal
  case; default depth cap 3; optional per-level geometrically shrinking E0/D0 schedules (floor 144)
  bound total worst wait < 2×2,160 ≈ 30 days at any depth, trading per-level hop budget — a shipped
  dial, default off (open problem O-4).
- **Token materialization**: confirm through the last colored tx (branch only, 2d+1 txs); no final
  state needed; allocation settles on the resting output.

The unilateral path is driven end-to-end by **sdk50** (and by the keyless tower in **sdk45**). A child
routed to a unilateral exit reports the unilateral shape it actually has — a unilateral exit produces
no withdrawal transaction, so it is never booked `WITHDRAWING`.

### 5.10 RGB integration

There is no re-signed colored defensive extension: it would contradict the signed-once rule and fork
into either 54-fold budget collapse or enclave-trust for invalidation. The rule set:

1. **RGB transitions anchor ONLY in signed-once transactions**: colored splits/combines (SP/CB) and
   colored self-transitions (de-trigger re-anchors, rollover self-splits). Plain T/X/S never host
   anchors and are sats-only (token-destroying if used on a carrier; structurally omitted from carrier
   watch bundles, exactly as the token-destroying plain backup always is).
2. **Terminal-freeze invariant**: a colored tx only ever spends outputs of **terminalized** structure.
   Terminalization (spend_budget → terminal, public receipt) happens before the colored co-sign, and
   the SE **refuses renewal on terminal nodes**, so no ancestor of any RGB anchor is ever re-signed.
   Consequently **no superseded colored witnesses exist anywhere in TES-R**: the rgb-lib "alternative
   unconfirmed closings of one seal" problem shrinks to the already-supported un-broadcast-carrier
   validation (consignments carry the un-broadcast T/X/SP chain as witness txs — more branch, same
   model).
3. **Seals sit on resting outputs** of un-broadcast colored txs; token transfers remain colored splits
   minting fresh receiver pieces — token DAG depth grows per token hop, carrier economics unchanged,
   **including calendar deadlines** (point 4).
4. **Carrier defense** = materialize the colored branch only (REQ-33 semantics), per-tier CSV head
   starts as §5.7 row 3. Because rule 1 forbids laddering a carrier at all, a carrier never gains the
   CSV tiers that would delete its calendar. It keeps the signed-once **absolute-locktime** backup, so
   a received token carrier **does** still have a root-deadline materialization duty, and REQ-33's
   `auto_exit_due` machinery is retained for exactly this shape (default on; **sdk34** materializes
   before the deadline, **sdk32** documents the residual clawback window if nobody does). Only
   *laddered* coins have no root deadline. This is the price of terminal-freeze and is deliberate.
5. **SE blindness fully preserved**: a colored sighash is byte-indistinguishable from a plain one;
   renewal/rollover/de-trigger sign sats-structure or owner-built colored self-transitions the SE never
   parses; consignments stay P2P (ECIES via relay). No batch coordinator exists, so a
   coordinator-sees-carriers concern does not arise.
6. **Every SE co-signed spend of a coloured coin's `F` is one of exactly two things** — there is no
   third category, and "an opret nobody verified" is a refusal rather than a shape:
   (i) **PLAIN**, and therefore an acknowledged allocation-destroying spend that **no exit path may
   recommend** (this is what the retained flat backups of a coloured coin are, and why they are omitted
   from carrier watch bundles); or
   (ii) **COLOURED and receiver-assigned**, verified as such by the receiver's own
   `verify_consignment_assignment` against an outpoint read from the RECEIVER'S own coin record.
   The consequence is a lane rule, because the permitted backup shape is a function of the DECLARED
   lane and not of the union of two lanes' shapes: **on a coloured bundle every conveyed flat backup
   MUST be plain** — any OP_RETURN on one is refused by name, in the same predicate that runs over every
   flat backup, as part of the R′ set (§5.11) alongside the tier colour-shape check. The permissive
   `op_return_outputs <= 1` STAYS for the **un-laddered carrier lane**, where the coloured backup is the
   legitimate exit material; one validator serves two lanes with opposite requirements. Without this, a
   prior owner's retained coloured backup is an *undetectable allocation-theft primitive* — it spends
   `F` and re-assigns the allocation to themselves, invisible to every other receiver check — and it
   also falsifies §5.8's griefing premise. Implemented by `verify_flat_backup_lane`
   (`clients/libs/rust/src/tesr.rs`), run on BOTH acceptance paths (claim and the SSP pre-pay census);
   guarded by `ci-guards/tests/deny_colored_backup_on_a_colored_ladder.rs`; the legitimate
   coloured-ladder-plus-envelope shape it admits is exercised live by **sdk78** (c.2).

**Evidence**: **sdk52** pins rule 1, the invariant everything else here rests on — in one wallet the
plain coin carries a ladder and the token carrier carries **none**, and an off-chain RGB transfer still
settles. **sdk32** pins the terminal-freeze semantics over time (carrier terminal at the SE,
un-laddered, no fresh co-signed sweep possible) and the residual clawback window of point 4. The RGB
suites (`rgb*`, `ta*`, `tb*`) run over the protocol unchanged.

### 5.11 Receiver verification at claim (R′ set)

Derived from public data; any deviation = reject.

**(R3′)** F on-chain, unspent, pays A.

**(R4′)** T spends F, no timelock; tier outputs pay A + the public H_tag tweaks; every later tier's
**signed** nSequence is a BIP-68 *block* relative timelock lying inside the band its kind allows —
`[e_floor, e0]` for an extension, `[d_floor, d0]` for a state, exactly `SPINE_CSV = 0` for a split
tip — and the tier's **declared** `csv` field is bound to that same signed number
(`bind_declared_csv`), so a schedule that contradicts the signatures is refused rather than believed;
superseded tiers are held to the identical band and binding. Headroom ≥ receiver policy.

`verify_bundle_ex` (and `verify_child_bundle`, and the superseded loop) check **bands, never
positions**. No receiver can form an exact per-position term: nothing serves `m` or `k` (§5.5) and they
are fields of the *sender's own* bundle. The band is not the weaker yardstick it looks like, because
its endpoints are not the sender's either — `cap_schedule` runs BEFORE the census on both receive paths
and refuses a conveyed `TesrParams` field by field against the receiver's OWN network preset. What an
exact position would add — that no undisclosed co-sign hides between the disclosed tiers — is carried
instead by R5′ (a hidden co-sign at any level raises the same TOTAL) and by the rival margin, which is
read off the LIVE rival's structural position (`RivalKind::margin` — δ for a state, δE for an
extension; 36 blocks each on mainnet), never off which conveyed list the superseded tier arrived in.

**(R5′)** SE signature count == exact expected tree size. `verify_bundle_bound` enforces the exact
equality **`se_num_sigs == flat_backups + tiers + superseded`**, where `se_num_sigs` is the lockbox's
attested lifetime `sig_count`; any hidden extra co-signed state/extension shows as a count mismatch.
The census only proves anything if that count is the ENCLAVE's, so it is not taken on the coordinator's
word: `/info/statechain` must carry an `utexo/sig_count/v2` attestation over
`(statechain_id, num_sigs, sig_budget, nonce)`, verified against the **PINNED enclave attestation
identity** rather than against any key served in the same response; an unattested or unverifiable count
is refused outright. The enclave signs every attestation with one long-term identity
(`utexo/attestation-identity/v1`, served at `GET /attestation_identity`) which the client pins:
**compiled-in pin → configured value → REFUSE**, never a fallback to the served key. Pinning rather
than chain-anchoring is required because a depth-≥2 in-ladder-split ancestor's funding output is
deliberately **un-broadcast**, so there is nothing on chain to bind to (`TRUST-MODEL.md` B11); the check
therefore holds at every split depth.

**(R6′/R7′)** per-level branch validation + Σ-inputs terminal ancestors, including the terminal-freeze
check for colored ancestry. **(R8)** RGB consignment client-validated, un-broadcast witnesses allowed.
All txs v3, committed-fee + single P2A, Σout = Σin − fee. Claim-time validation is DoS-priced.

**The split-depth cap is derived, not a literal.** There is no fixed number to compare against:
`max_split_depth(base, per_level, epoch_blocks) = 1 + (epoch − base_wait)/level_wait`
(`max_split_depth`, `lib/src/transfer/receiver.rs`), enforced by `enforce_split_depth_cap_shaped`
(`clients/libs/rust/src/tesr.rs`). It MOVES WITH THE NETWORK PROFILE, which is the point — and is why
the profile is explicit per network rather than falling through to a toy schedule.

The R′ set is enforced at claim: **sdk46** (R5′ against the *real* SE counter — the SE increments by
exactly the tier count, `verify_bundle` accepts the true count and rejects a hidden extra signature),
**sdk47** (R′ across a transfer), **sdk54** (adversarial `verify_bundle`), **sdk55** (backup-chain
adversarial), **sdk58** (11 adversarial in-ladder-split cases). The census generalizes to N hops for
first-class children, per segment and with the same three terms
(`child_num_sigs == CHILD_V2_BASELINE + tiers + superseded`, and `CHILD_V2_BASELINE = 0` because a
derived child slot never ran `create_tx1` and so has no flat backup): each hop discloses exactly one
superseded state, so an undisclosed rival shows up as a count mismatch — **sdk60**, **sdk17**.

### 5.12 Lightning latch

Transfer shape is unchanged, so the latch composes over the ladder; the renewal co-signs bundled into a
latched transfer are gated by the same preimage — atomic with the state co-sign. No half-renewed state
is observable to the SSP.

The latch is the **HODL-invoice** construction (`LIGHTNING.md`); PTLCs do not exist in the routing
network, so no adaptor-signature construction is available. Lightning works **both directions on the
ladder**: **sdk63** (pay), **sdk64** (receive), **sdk67** (receive out of an in-ladder split),
**sdk65** (non-exact pay), with the failure/rollback paths in **sdk68** (exact reclaim after a pay
failure) and **sdk66** (non-exact rollback); **sdk53** pins the latch guard.

**This is the one case that stays terminalized.** The LN-latched piece sits unclaimed past the
pending-transfer lock's window (the SSP settles on its own schedule), so the temporary lock cannot be
what protects it — the piece is terminalized instead, a permanent lockout. Every other in-ladder child
is *not* terminalized (§5.4): it is protected by the census plus the key handover.

The one-call Lightning PAY API pays from the ladder directly (it does not mint its input via
`ensure_exact_coin`, which would refuse every laddered coin — i.e. every coin the protocol produces).

### 5.13 Watchtowers (the keyless TES-R watch bundle)

The bundle is {trigger, newest extension, newest state or SP chain, per-tier CSV schedule} — all
pre-signed, pays only the owner, zero key material (REQ-34 preserved). `watch_pass` is a small state
machine: monitor F (one outpoint subscription) → on hostile trigger, broadcast X at +E_m and state/SP
at +Δ. `watch_pass` → `watch_pass_seen` branches only `Idle` / trigger-matched / `Void`.

**The bundle carries no fee-child templates, and cannot.** `TesrBundle` has no such field, and
`WatchEntry` carries only `branch_txs`, `backup_tx`, `backup_locktime`, `deadline_block`, `trigger`.
A fee child needs a funding input a keyless tower does not hold. There is likewise no "request co-op
de-trigger via the owner's SDK" path in a tower.

**WHAT A KEYLESS TOWER CANNOT DO — normative.** A keyless tower can watch `F`, and can broadcast the
pre-signed tiers **at their committed fee**. It **cannot fee-bump them.** A CPFP child spending the P2A
anchor requires an input the tower does not hold and a signature it cannot make, so if the mempool's
floor rises above a tier's committed rate, a keyless tower has no action available: the tier is refused
outright at `sendrawtransaction` and that refusal is one it cannot answer. This is a **stated limit of
the protocol, not an implementation gap** — an implementer must not read "delegable, keyless watching"
as implying spike-time rescue.

Illustrative refusals are **lab numbers, not protocol bounds**. A real tier is 125 vB (`TIER_VBYTES`)
committing 375 sats at the shipped 3 sat/vB, so under a 4 sat/vB floor it reads `min relay fee not met, 375 < 500`.
The one live quote below — `min relay fee not met, 6 < 13` — comes from a **regtest** node at a
**0.1 sat/vB** floor against a parent deliberately built under it; what makes it worth quoting is the
*path*, not the numbers.

**Nor does the anyone-can-spend anchor supply a rescuer, and the reason is structural, not a ratio.**
The child's change must clear `CHILD_CHANGE_DUST = 330` while the anchor is worth `P2A_VALUE = 240`, so
an anchor-only child can never produce a legal change output at any fee rate — and the only builder in
the tree hardcodes two inputs (anchor + the owner's funding UTXO), so it cannot be constructed with repo
code at all. "Anyone-can-spend" is a permission, and there is no shape in which it is an incentive.

**WHO BUMPS, THEN: the owner.** The party that funds a CPFP package is the **coin owner**, from their
own wallet — the only party holding both a spendable UTXO and a motive. The consequence is explicit:
**the guarantee during a fee spike is "the owner is online"**, which is precisely the condition a
watchtower otherwise removes. The protocol does not promise otherwise.

**OPTIONAL: the funded-tower variant.** An operator MAY run a tower with a small hot fee wallet and
bump on the owner's behalf. Its exposure is bounded: such a tower still holds **no coin keys**, so
compromising it costs the operator's fee float and **cannot touch a user's coin** — a materially
smaller claim than "keyless", and materially larger than nothing. It carries an operational duty in
exchange: the float must be refilled, and **a tower that runs dry fails at exactly the moment it is
needed**.

**The float's binding unit is NOT sats.** A bond of "~2 spike bumps (~2 × 153 vB × 50 sat/vB ≈ 15,300
sats)" is the obvious sizing and it is not the binding one. A fee child is v3 and spends two things:
the stuck tier's P2A anchor, and a funding UTXO. Under TRUC (BIP-431) a v3 transaction may have at most
**one** unconfirmed ancestor — and the tier is already it. So a second rescue funded from the
*unconfirmed change of the first* has two unconfirmed ancestor chains and is refused at any price.
Measured, not argued (`clients/libs/rust/tests/live_tower_float.rs`): the chained attempt returns
`TRUC-violation, tx <txid> would have too many ancestors`, while the same tier funded from a second
CONFIRMED output is accepted.

**Therefore a tower's simultaneous-rescue capacity is the number of CONFIRMED fee UTXOs it holds, each
large enough for one bump — not its balance.** A float of 1,000,000 sats in ONE output rescues exactly
one tier per confirmation window however many coins it watches; a tower sized only in sats reads as
solvent and is not. The rail therefore reports in both units (`tower_float::Solvency`), names which one
failed, and gives the matching remedy — short on sats means add money, short on capacity means SPLIT
what you already hold, and the two are not interchangeable. `tower_float::plan_float` distinguishes a
float that needs a top-up from one that needs only re-shaping, because the latter is fixable for free
and an operator told to "top up" will spend money and remain uncovered. This variant is offered so the
choice is informed; it is **not** assumed by any other part of this specification.

**Broadcast shape per variant.** A **funded** tower's `watch_pass` MUST be package-aware
(`submitpackage`, not per-tx `transaction_broadcast_raw`). A **keyless** tower MUST NOT be: it has no
funding input to spend, so a package it could build would be a 1-parent-0-child package, i.e. the same
broadcast with more moving parts. Multiple towers compose idempotently on both.

**What is built.** `mercurylib::wallet::p2a_fee_child::build_p2a_fee_child` builds and prices the v3
owner-funded child, and `mercuryrustlib::core_rpc::submit_package` submits the 1P1C package to a
Bitcoin Core node (electrum has no `submitpackage`, so this is a second, opt-in backend used for this
one call). **Verified live, end to end, through repo code**: a v3 tier paying 0.048 sat/vB was REFUSED
alone by a node whose floor is 0.1 sat/vB (`min relay fee not met, 6 < 13`) and then ACCEPTED as a
package (`package_msg = "success"`, parent in the mempool with a descendant count of 2) —
`clients/libs/rust/tests/live_p2a_package_rescue.rs`. The CALLERS are wired: `exit_pass_with_bump` and
`watch_pass_with_bump` escalate a tier refused at its committed fee into a 1P1C package, and
`unilateral_exit` / `defend_ladders` use them whenever `SdkConfig::fee_bump` supplies an owner fee
source. The capability is an explicit argument, never ambient config, so the plain `exit_pass` /
`watch_pass` keep their exact keyless meaning — and a keyless pass reports a fee-stuck tier as a
**stated limit** ("no fee-bump capability was supplied … a keyless tower cannot bump … it will not
clear by retrying") instead of as one more retryable failure indistinguishable from an immature CSV.
The anchor is located by matching the P2A script, never by a guessed vout, because a coloured tier
carries an extra `opret` and a hardcoded index would spend the payload output instead.

**What is NOT built.** (1) The **prepaid fee BOND** of the optional funded-tower variant — its
actuarial sizing and the operator-signed per-bump refund/rebate rail (O-5). `tower_float::Solvency` and
`plan_float` answer *how much, in how many outputs*; nobody has priced the bond or built the rebate.
(2) **No E2E suite test exercises spike-time bumping.** The two tests that do
(`live_p2a_package_rescue.rs`, `live_tower_float.rs`) are node-gated: they need a Bitcoin Core RPC
endpoint and skip — loudly, printing why — without one, so a green suite run is not evidence the rescue
works. Separately, and by design rather than as a gap: the keyless default walks the tiers with per-tx
`transaction_broadcast_raw` and attaches no P2A fee child — a keyless tower has no funding input to
spend and would not bump even if the code were reachable from it; what it does instead is *say so*.

**Evidence**: **sdk45** for the two properties this section rests on — the watch bundle carries **no key
material**, and a **second independent tower is idempotent** (both explicitly asserted there).
**sdk51** runs the state machine against a real hostile trigger (and is a no-op while the coin is idle —
an un-broadcast coin never ages, so there is nothing to defend). The carrier-side counterpart —
materializing a colored branch before its deadline — is **sdk34** (§5.10 rule 4).

---

## 6. Requirement-by-requirement verification

**REQ 1 — off-chain forever / activity-scaled footprint: MET (per the refinement; near-categorical).**
Idle **laddered** coins: 0 vB forever — no rent, no root deadlines, no forced materializations (the
entire 5,840 vB/coin-yr rent class of §2 is absent). An idle **un-laddered** coin (RGB carrier, B0
sub-coin) also pays 0 vB rent, but it keeps its absolute-locktime root deadline, so a *received*
carrier still owes one materialization before that deadline (§5.10 rule 4) — activity-scaled, but not
deadline-free. Off-chain transitions: 576 hops per depth level, rollover is off-chain (sdk43), so hop
count is **unlimited without any mandatory chain touch**; the default depth-3 compaction policy is an
optional optimization costing 112 vB per ~2,300 transfers ≈ 0.05 vB/transfer, executed by the
SDK/operator inside `transfer()` and priced into that transfer's fee — the user never acts personally.
*Residual*: a hostile ex-owner can force a coin one 125-vB co-op de-trigger, and that costs the attacker
**nothing** at the committed rate (§5.8), so footprint scales with activity OR adversary spite. What is
bounded is the damage: ~980 sats of fees and anchors out of the coin per grief, value intact.

**REQ 2 — no/low operator liquidity: MET.** No round liquidity (nothing expires), no denomination
pools (exact-amount Σ-conserving split/combine preserved verbatim), no leaf stock (all value is the
user's own coin). Operator money is fee-sized only: tower fee bonds (prepaid, priced into onboarding)
and P2A bump children. *Asterisk*: contingent tower fee capital under a correlated grief wave is real —
~$17/coin defended at 30 sat/vB; a 100k-coin wave ≈ $1.7M fronted. Mitigated by committed base fees
(base case needs no child), the de-trigger (defense is one 125-vB co-op tier, not a **375-vB**
pre-signed unilateral walk), and the prepaid bond rail (**unbuilt**, O-5); it is capital-at-cost, never
custody or principal.

**REQ 3 — non-custody under operator hack: MET; improved in notice.** Pre-hack coins left untouched are
unconditionally safe: every spend path needs both 2-of-2 shares; the hacked SE holds only post-rotation
e_n; renewal requires the current owner's partial (the SE cannot mint a lower-CSV extension alone); the
unilateral tree is pre-signed at all times; SE refusal = freeze ≠ seize. The residual is **B1** (SE +
past owner with a retained pre-rotation share — enclave-deletion failure), with the same mitigations
(enclave, blindness blocks targeting, public counter receipts) — and post-hack theft against watched
coins requires a public trigger + ≥144 blocks of on-chain notice instead of a minutes-scale mempool
race. Old-epoch invalidation is race-conditional, not absolute (§5.5).

**REQ 4 — blind operator / P2P RGB: MET.** The SE signs only sighashes; colored = plain at the byte
level; consignments stay P2P; renewal/rollover/de-trigger never touch RGB payloads; anchors live
exclusively in signed-once colored txs over terminal-frozen ancestry (§5.10), so re-signing can never
invalidate an anchor. No batch coordinator ships. Exit-time RGB disclosure unchanged and acceptable.

**Top residual risks (stated plainly):**

- **R-1 Correlated SE-death + mass trigger grief**: with the SE dead, de-trigger is unavailable and
  every triggered coin fights per-tier races; defense capacity saturates around ~10⁵–10⁶ simultaneous
  triggers/day, beyond which small coins are not worth defending at spike rates. Never confiscation;
  bounded in *damage* (fee-sized sats per coin, §5.8) — **not** by the attacker's own spend, which is
  zero at the committed rate. Untriggered coins idle safely under a dead SE forever, so there is no
  deadline-driven exit stampede.
- **R-2 No unconditional no-watch window**: a received coin must be watched (delegated, keyless,
  prepaid) from the moment of receipt, forever. Alarm-driven with ≥1 day notice, accepted deliberately.
  On the un-laddered shape the duty is *calendar*-driven instead (materialize before the root deadline,
  §5.10 rule 4) — **sdk32** is the standing record of what happens to a received carrier whose owner
  never acts.
- **R-3 B1 unchanged**: the statechain trust unit (enclave share-deletion) remains the floor, as in
  every statechain.
- **R-4 δ=36 head starts vs sustained congestion**: a fee spike outlasting the head start converts the
  CSV edge into a pure fee race; committed fees + P2A + funded towers are the answer, and δ is a dial
  (O-2 quantifies against mainnet history before mainnet ship). **Currently carried, not mitigated, for
  the default deployment**: the keyless default tower broadcasts tier-by-tier and attaches no fee child
  (§5.13), so only the committed-fee half defends it there.
- **R-5 Deep-DAG unilateral latency**: depth-3 worst ≈ 60 days (coop exit instant); geometric schedules
  cap it ≈ 30 days at reduced budgets (O-4).
- **R-6 There is no enclave counter machine {level, m, k}, and none is owed.** What is attested is a
  single lifetime `sig_count` per coin plus its budget; every structural expectation (which epoch,
  which level, how many tiers should exist) is reconstructed by the receiver from the bundle and its own
  preset. That is sound for the census — a total is exactly what an exact-equality count needs, because
  a hidden co-sign at **any** level raises the same total. The real residual is the two coordinator-trust
  premises the census rests on: **P3**, that `se_num_sigs` is the true count, now earned by the
  **pinned-identity** `utexo/sig_count/v2` attestation (§5.11); and **P1, the sid ↔ aggregate-key
  binding, still coordinator-supplied and unattested** (`ladder_binding_precheck_cause`). The key
  material and the counter it attests are held by the same party, which no counter machine would have
  fixed. See O-1.

---

## 7. Footprint economics

Primitives: P2TR in 57.5 / out 43 / overhead 10.5 / P2A out 13 vB; **tier tx 125 vB** (measured through
the production finaliser; §5.9), coloured tier **168 vB** (= 125 + one 43-vB `opret` output, exactly);
**fee child 153 vB** (measured); solo compaction 112 vB. There is no output netting, so no batched
figure below 103 vB/coin is achievable.

**7.1 Per coin** (the absolute-ladder column is the alternative of §2–§3)

| | Absolute ladder | TES-R |
|---|---|---|
| Idle coin | 5,840 vB/yr | **0 vB/yr** |
| Active, 10 tx/day, default depth-3 policy | 5,840 vB/yr (rent dominates) | 3,650 hops/yr ÷ ~2,300/compaction ≈ 1.6 × 112 ≈ **~180 vB/yr** (≈0.05 vB/transfer) |
| Active, no-compaction policy | n/a | **0 vB/yr** on-chain; +250 vB contingent exit weight per 576 hops (~0.43 vB/hop, paid only if exiting unilaterally) |

**7.2 System, 1M-vB blocks, 52,560 blocks/yr, 10% of coins active at 10 tx/day (aggressive):**

- **1M coins**: idle 900k → 0; active 100k × ~180 ≈ 18M vB/yr ≈ **342 vB/block ≈ 0.034%** of block
  space (absolute ladder: 111,111 vB/block ≈ 11.1% — a **~320× reduction**). Plus churn: 20%/yr
  deposit+exit at ~111 + 375–834 vB adds ~0.2–0.4% — present in every design including Spark.
- **10M coins**: ≈ **0.34%** (+ churn) — vs the absolute ladder's physically impossible 111%.
  **100M coins ≈ 3.4%** — the "hundreds of millions TVL" target is reachable if value concentrates
  realistically.
- Fully idle custody TVL is exactly free at any scale.

**7.3 The fee model (per the delegation refinement)**

- Onboarding fee: deposit tx share (~58–111 vB) + tower fee bond (~15k sats, actuarially sized — O-5)
  + tower infra (~$0.001/coin-yr: 10M outpoint subscriptions ≈ one indexed node ≈ $3–6k/yr, ~15 GB
  bundles). There is no idle-rent prefund line.
- Per-transfer fee: carries the amortized compaction share (≈0.05 vB) + committed-fee top-ups +
  renewal (0 vB, 2 co-signs). Users never see or perform a maintenance action.
- Grief insurance: a small per-transfer tower-insurance component funds de-trigger re-anchors
  (125 vB each), priced as insurance against a **free** attack (§5.8).

**7.4 Tower economics**: monitoring is cheap and delegable; the priced item is contingent defense
capital (R-1), bounded per coin (≤ 3 × 153 vB × spike rate) and prefunded via the bond. A tower's worst
action remains harmless (everything it holds pays only the owner).

---

## 8. What is NOT chosen, and why

### 8.1 Shared-UTXO factories — REJECTED (factory primitive); the trigger-ladder half is in TES-R

**Fatal**: the factory root is an n-of-n of operator-chooseable, never-rotating signers. A
Sybil-originated factory — operator fills 64 slots with its own keys, funds them itself, distributes
leaves to real victims who pass every R-check — lets the operator (or hacked-SE + original cohort,
**B1-F**) fresh-spend F_root via private relay and confiscate all 64 coins in one confirmation,
*including received-and-untouched coins*, destroying every RGB allocation below. No timelock gates a
fresh signature; the victims' pre-signed tree loses a race they cannot see. This deletes the one
absolute guarantee a plain deposit has (self-deposited never-transferred coin safe vs SE alone) and
breaks REQ-3's "preserve or improve" — unfixably, because the amortization *is* the shared multisig,
which cannot include current holders and whose signer independence Bitcoin cannot prove.

**Cost-benefit inversion**: amortization is input-bound — ~101.5 vB/user real (with change outputs) vs
~111 solo, a one-time ~10–50 vB saving — while the ceremony succeeds with p ≈ 0.98⁶⁴ ≈ 27% per attempt
and free-join griefing stalls onboarding. The CSV trigger ladder, co-op de-trigger, self-split renewal
and tower fee bond deliver the entire footprint win standalone, and are inside TES-R. Factories may
return under CTV (covenant-committed trees have no cohort to collude, deleting B1-F) — §10.

### 8.2 Evolved absolute ladder — REJECTED

**Fatal**: rent is structurally O(coins × time/initlock): 505 vB/coin-yr idle → 0.96% of block space at
1M coins, **9.6% at 10M, 96% at 100M** — an 11.5× postponement, not a cure. **Fatal #2**: pre-signed
refresh chains re-anchor flat coins only; an idle *received sub-coin* is still bounded by its root
deadline, and extending it requires either materializing (footprint spike) or all-descendant re-signing
(multi-party liveness) — unsolved in general, i.e. REQ-1 fails for exactly the split/combine feature
REQ-2 forces us to keep. Additional: 90-day unilateral/SE-death wait (correlated with bank-run
moments); actuarial fee-model insolvency in the tail (13-month frozen fees, no forward market); (k+1)×
enclave share blast radius on an unattested SGX.

Ideas taken from it: P2A-on-everything, tower-executed maintenance (made off-chain), and
**conveyed locktimes**, which are **not moot on a laddered coin**: a laddered coin that has been
received keeps `min(L_k)` from its retained flat backup chain, so the class survives on every received
coin (`TRUST-MODEL.md` B6), absorbed by the `auto_exit_margin_blocks` default and by
`deadline_safety_due` rather than solved — `k_max = 14` is an assumption, not a measurement.
That default is DERIVED, not chosen:
`auto_exit_margin_blocks_for(k_max, interval, d) = k_max·interval + (3 + 2d)·144` — which at
`k_max = 14`, `d = 1` is **2,120 blocks on mainnet** (interval 100) and **860 on regtest** (interval 10).
A flat `k_max·interval + 144` would spend a **single** confirmation window on a walk that lands
`3 + 2d` transactions one after another, each of which must confirm before the next tier's relative lock
even starts counting. The walk's own `Σ csv` is deliberately NOT in the margin: `auto_exit_due` takes
that head start per coin, off the coin's own chain, and subtracts it before comparing.

---

## 9. Test coverage

Every item below names a test that exists today.

| Property | Live evidence |
|---|---|
| Laddered deposit + full lifecycle | sdk48, sdk42, sdk44 (schedule params) |
| Transfer over the ladder (Model A) | sdk41, sdk49, sdk40 |
| Renewal → rollover → renewal, unbounded, 0 vB | **sdk43** |
| CSV enforced by real consensus; stale ladder dies once its prevout is spent | **sdk40** PART 1 / PART 2 |
| Cooperative de-trigger defeats a hostile trigger; owner names the destination; retained tiers die | **SDK_E2E=89**, **sdk40 PART 2** |
| Unilateral exit through the tier chain (public SDK surface) | **sdk50** |
| Watchtower defends a triggered coin; keyless bundle, idempotent second tower | **sdk51**, **sdk45** |
| Receiver verification R′ / census | sdk46, sdk47, **sdk54**, sdk55 |
| In-ladder split (adversarial + end-to-end) | **sdk58** (11 cases), sdk59 |
| First-class children (whole / partial onward hop) | **sdk60**, **sdk17** |
| Retained `min(L_k)` calendar height advances per hop | **SDK_E2E=86** |
| RGB carrier never laddered; terminal-freeze over time | **sdk52**, sdk32; carrier materialization sdk34 |
| Coloured-ladder flat-backup lane rule | **sdk78** (c.2) + ci-guard `deny_colored_backup_on_a_colored_ladder` |
| Lightning both directions on the ladder (HODL latch) | **sdk63**, **sdk64**, **sdk67**; sdk65 non-exact; sdk66/sdk68 failure paths; sdk53 latch guard |
| Concurrency / DAG invariants | chaos22 |

**Not covered by any test in this suite, and honestly open:**

- **Package-relay tower defense / P2A fee-child attachment under a real fee spike.** The code exists
  and is wired (§5.13), but the only tests that exercise it (`live_p2a_package_rescue.rs`,
  `live_tower_float.rs`) require a Bitcoin Core RPC endpoint and skip without one, so a green suite run
  proves nothing here.
- **The coloured de-trigger variant** (§5.8) and the **restoration half** of any de-trigger (fresh `F′`
  + rebuilt `T′/X′_0/S′_0`), which is unbuilt.
- **The funded-tower fee BOND** (O-5) and **mass-grief prioritization** (R-1 / O-6). Design-level only.
- **Deep un-broadcast RGB witness chains** — depth and DoS adversarial suite (O-3).

---

## 10. Open problems & future work

- **O-1 — the trust premise under the census.** The enclave key material and the counter it attests are
  held by the party the receiver is being protected from. The census proof must therefore be published
  *with* its premises: **P3** (`se_num_sigs` is the true count) is **earned** by the pinned-identity
  `utexo/sig_count/v2` attestation (pinned, not chain-anchored: a deep in-ladder-split ancestor has no
  chain anchor by design — §5.11), while **P1 (the sid ↔ aggregate binding) is still
  coordinator-supplied and unattested**. The only construction that closes it is a second,
  independently administered SE write domain under a separate legal entity; an external anchor over
  `(sid, n, h_n)` is rejected with reasons (the attack is *under*-reporting, and the receiver's rule
  would be a floor written by the adversary). *De-risked in practice*: the census is exercised
  adversarially by sdk54/sdk55/sdk58, across hops by sdk46/47/60/17, and for retry-idempotence of the
  count by sdk56.
- **O-2 (blocking dial)**: δ/δE vs sustained mainnet congestion — quantify head-start survival against
  2023–24 spike history; δ=36 is the shipped default (`TesrParams::mainnet()`, arithmetic pinned by
  sdk44), with the budget table of §5.2 as the trade space. **The congestion study is not done.**
- **O-3**: rgb-lib adversarial suite for deep un-broadcast witness chains (terminal-freeze removes
  superseded-witness ambiguity, but depth and DoS bounds need tests). *Status*: the never-laddered
  carrier rule and terminal-freeze semantics are pinned (sdk52, sdk32) and the `rgb*`/`ta*`/`tb*` suites
  run over the protocol; the depth/DoS adversarial suite is still missing.
- **O-4**: deep-DAG unilateral latency — geometric per-level E0/D0 schedules (worst wait <30 days at any
  depth) vs per-level hop budgets; ship as a dial with a chosen default.
- **O-5**: tower fee-bond actuarial sizing + refund/receipt rail (operator-signed per-bump rebates,
  reusing the `refresh_sponsored` rail shape).
- **O-6**: mass-grief saturation modeling (R-1) — prioritization policy for de-trigger waves,
  per-coin-value triage, and whether a standing "grief bond" priced into transfers should scale with
  coin count.
- **O-7**: relay-policy dependence — v3/P2A/1P1C are policy, not consensus; maintain the
  miner-direct-submission fallback and re-verify at each Core release.
- **O-8**: batched compaction (**not shipped**): non-interactive constructions only (no interactive
  MuSig2 coordinator); pre-signed T′ against the template before broadcast is mandatory if built.
- **O-9**: `child_in_ladder_pay` splits a child through a depth-2 `ancestors` chain (§5.4). The N-hop
  census generalizes, but the interaction of *depth* with the **derived** depth cap of §5.11
  (`max_split_depth` — there is no literal to compare against, and the number moves with the network
  profile) and with the `min_child_value` floor at each level is only exercised to depth 2 (sdk17,
  sdk60). Deeper child chains need their own adversarial pass before they are relied on.
- **Covenant future work**: **CTV** would let one on-chain output commit to the whole T/X/S tree (and
  make factories safe — no signer cohort to collude, deleting B1-F — reviving shared-UTXO amortization
  for onboarding). **APO/ANYPREVOUT** would make state txs rebindable (true Eltoo): the trigger tier
  disappears, trigger-griefing with it, and renewal becomes a pure local re-sign. **CSFS** would allow
  delegated de-trigger without SE liveness. None are required: TES-R runs on today's Bitcoin.
