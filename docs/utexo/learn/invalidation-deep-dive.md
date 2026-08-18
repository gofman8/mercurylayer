# Old-state invalidation — the deep dive

How this system makes yesterday's owner unable to spend today's coin, what that machinery does over
days and weeks, and what it feels like to hold, receive, and exit a coin. This is the long-form
explainer; the short comparison is [invalidation.md](invalidation.md), exit mechanics are in
[exits.md](exits.md), and partial amounts in
[granularity-deep-dive.md](granularity-deep-dive.md). The normative accounts are
[PROTOCOL.md](../spec/PROTOCOL.md) (the TES-R ladder), [SPEC.md](../spec/SPEC.md) (REQ/INV/ERR),
[TRUST-MODEL.md](../spec/TRUST-MODEL.md) (who trusts whom, boundaries B1–B11),
[CHILDREN.md](../spec/CHILDREN.md) (first-class split children) and
[PARTIAL-PAYMENT-ECONOMICS.md](../spec/PARTIAL-PAYMENT-ECONOMICS.md) (what a payment costs).
Audience: developers, integrators and researchers who have not read the code. Every number below
comes from a named symbol or a named live test; where behaviour is open we say so rather than round
it off.

## The one sentence you must not misread

**A laddered coin has two clocks, not zero.** The *relative* CSV tiers of the TES-R ladder do not
tick while un-broadcast, so an idle coin's exit chain never ages and costs 0 vB of rent. But the coin
also retains a **flat backup chain** with *absolute* locktimes, held in copies by every prior owner,
and its lowest rung `min(L_k)` is a finite calendar height that mining approaches and every whole-coin
hop shortens. "Idle coins never age" (**INV-27**) is a statement about the tiers. `sdk86` measures
both clocks on the same coin and shows they behave differently.

Everything in this page follows from that pair.

## One protocol, two coin shapes

There is exactly **one protocol**. `claim()` establishes a TES-R ladder — trigger `T` → extension
`X_m` → state `S_k`, all **relative-CSV** and all **un-broadcast** — for every fresh confirmed
**root** coin, unconditionally. There is no per-deposit protocol switch and no escape hatch. Under
that one protocol a coin takes one of two **shapes**, and both are live:

- **LADDERED** — every plain BTC deposit, and every RGB carrier on a network whose enclave identity
  is pinned (there the ladder is *coloured*, and it is a ladder in every other respect). Old state is
  outranked by **relative** timelock ordering (each whole-coin transfer co-signs a state one δ
  lower), backed by the receiver's disclosure census and a keyless watchtower. Its **tiers** never
  age and cost **0 vB** of on-chain rent; its retained flat chain still carries `min(L_k)`.
- **FLAT-LANE** — a coin resting on the signed-once **absolute-locktime** backup chain,
  transferring by backup-chain handover. An **RGB carrier** takes this shape wherever no enclave
  attestation identity is pinned, because a *plain* tier spend would destroy the allocation
  (terminal-freeze, [PROTOCOL.md §5.10](../spec/PROTOCOL.md); `sdk52`) and the *coloured* ladder
  that would carry it safely cannot be established without an identity to verify the enclave
  against. This shape is **load-bearing for RGB tokens**, not dead code (`sdk52`, `sdk39`,
  `sdk32`, `sdk34`, `sdk78`).

`SdkConfig::colored_ladder` (`clients/libs/rust-sdk/src/config.rs`) is what selects between the two
for a carrier, and it no longer states a bool: both constructors READ the compiled-in pin,
`TesrParams::attestation_identity_const` (`lib/src/tesr.rs`). Regtest pins the repo's own dev
enclave, so the flag is **true** and a carrier is laddered like any other coin — coloured, every tier
carrying a real RGB state transition, wired through `build_colored_ladder_auto` /
`cosign_colored_ladder` (`clients/libs/rust/src/tesr.rs`), the coloured in-ladder split,
`colored_reanchor` and an RGB-aware `defend_ladders`. Mainnet's const returns `None` because no
mainnet enclave is provisioned, so the flag is **false** there — a statement about what exists to
attest, not a verdict on the lane (**V-6**; TRUST-MODEL **B11** is why the coordinator's own answer
cannot stand in for a pin). Pin a mainnet identity and the flag flips with nothing else changing.

**What did NOT change is the un-broadcast half, and it is permanent.** A **split sub-coin** whose
funding output is still un-broadcast cannot root a trigger (**B0** — the trigger would have no
prevout to spend, and a v3 tier cannot relay over an unconfirmed v2 parent), which is why
`claim()`'s ladder pass is root-only. Colouring a tier does not broadcast a funding output, so this
holds on both lanes: every in-ladder split **child** and every spine-tip **change leg** is funded
by an un-broadcast `SP.out[j]`, and that is the point of the design rather than a gap in it — it is
where the 0 vB of idle rent comes from. Such a coin is not "un-laddered": its ladder simply hangs
off `SP` instead of off `F`.

Almost every mechanic below exists in both shapes but in a different key. Each is labelled.

Vocabulary, and the first two are different axes rather than opposites: the **flat lane** is the
SHAPE above — a coin whose defence is the absolute-locktime backup chain — while a **flat coin**'s
*funding output* is on-chain, which a laddered root also is. A **sub-coin**'s funding tx is
pre-signed but un-broadcast (*materializing* the branch broadcasts it, turning the sub-coin flat in
the second sense only); a **child** is the piece minted by an in-ladder split, and it is laddered
while its funding stays un-broadcast; a coin's **epoch** is the `initlock`-long window its flat
backup chain measures.

Contents: [the problem](#1-the-problem-from-first-principles)
· [two clocks](#2-two-clocks-what-relative-locks-delete-and-what-survives)
· [four layers](#3-the-four-defence-layers) · [walkthroughs](#4-lifecycle-walkthroughs)
· [over time](#5-over-time-what-a-coin-actually-costs-to-hold)
· [real-world situations](#6-real-world-situations) · [UX](#7-the-ux-perspective) · [FAQ](#8-faq)
· [recap](#9-comparison-recap-over-time-behaviour).

---

## 1. The problem, from first principles

An off-chain transfer changes who owns a UTXO without touching the chain. The chain therefore cannot
referee: at any moment there may be several *mutually exclusive*, *individually valid* descriptions
of who owns the coin — the current one, and every state the coin passed through on the way. Each past
owner once held (and may have kept) a pre-signed transaction that pays the coin to *them*. If nothing
distinguishes old state from new state, the first past owner to reach the chain wins, and off-chain
"ownership" means nothing. Old-state invalidation is the set of mechanisms that make the newest state
win — cryptographically, economically, or temporally — against every stale copy in every past owner's
backup folder.

The design space is small and every deployed L2 picks from it: **expiry** (old state simply dies after
a window — Ark/Second's round expiry; cheap, but missing the window forfeits funds to the operator),
**revocation** (handing over a secret makes old state punishable — Lightning; strong, but requires
per-counterparty state and constant watching), **decrementing locks** (every new state carries a lower
timelock, so the newest matures first and wins any honest race — Spark's relative ladder,
SuperScalar's nSequence counters, and the absolute-nLockTime ladder that is this design's measuring
stick), and **operator refusal** (a semi-trusted co-signer refuses to sign conflicting or expired
state — every statechain; instant and race-free, but only as strong as the operator's honesty).

This system takes decrementing locks as the trustless floor and layers operator refusal,
receiver-side verification, and watching on top — four layers, each covering the failure mode of the
one below. Its distinguishing choice is *which kind* of decrementing lock the exit chain uses:
**relative** (BIP-68/BIP-112 CSV) rather than **absolute** (nLockTime). §2 is that whole argument,
including the part the substitution does not buy.

## 2. Two clocks: what relative locks delete, and what survives

### 2.1 Why an absolute exit ladder cannot scale

1. **Bitcoin cannot revoke a signature.** When a coin changes owner off-chain, every previous owner
   keeps their old pre-signed transaction — cryptographically valid forever. No opcode deletes,
   expires or punishes it. (Lightning fakes revocation with penalty keys; that requires
   per-counterparty state, constant watching, and a larger collusion surface — rejected here, §1.)
2. **So old states can only be *outranked*, never destroyed.** The only tool for ranking mutually
   exclusive pre-signed transactions in time is the timelock, and ranking requires strictly
   decreasing timelocks: each handover gives the new owner a lock that matures strictly *before*
   anything the sender kept.
3. **An *absolute* lock ticks while un-broadcast.** The clock runs on the calendar whether or not
   anyone is attacking, so a defence built on it must be renewed on the calendar: one on-chain
   re-anchor per coin per horizon, **whether or not the coin ever moves**. Measured
   ([PROTOCOL.md §2](../spec/PROTOCOL.md)): 112 vB per coin per ~1,008 blocks ≈ **5,840 vB per
   coin-year of pure idle rent**; one million coins ≈ **11.1%** of all Bitcoin block space; ten
   million is physically impossible. Delegation changes who *pays*, not how much chain space is
   *burned*.
4. **A *relative* lock does not tick until its parent confirms** (BIP-112). Put the exit ordering on
   relative locks and that clock starts on **attack**, not on deposit.

### 2.2 The ladder

```
F   on-chain funding UTXO (P2TR key-path, aggregate A = KeyAgg(P_user, P_SE))
└─ T    TRIGGER    v3/TRUC, NO timelock, signed ONCE at deposit detection, never re-signed
   │                out[0] → A + H_tag("TES/trigger",A)·G      out[1] → 240-sat P2A anchor
   ├─ X_0 … X_m   EXTENSIONS   mutually exclusive spends of T.out[0]
   │                X_m: input nSequence = relative CSV E_m = E0 − m·δE
   │                re-signed OFF-CHAIN at each renewal; a lower CSV replaces the old one
   └─ (on X_m.out[0])  S_0 … S_k   STATES, plus SP split states and CB combines
                    S_k: input nSequence = relative CSV Δ_k = D0 − k·δ
                    one δ LOWER at every whole-coin transfer — the new owner matures first
```

All three tiers are pre-signed and **un-broadcast**. `T` carries no timelock, and every CSV below it
counts only from the confirmation of its parent. Therefore **nothing in the tier tree matures until
somebody broadcasts `T` on-chain**. An idle coin — and an entire idle split DAG — has an exit chain
that is byte-identical after any amount of waiting, and **0 vB of idle rent**. `sdk40` PART 1 proves
BIP-68 is enforced by real consensus and that un-broadcast material does not age; `sdk43` runs a coin
through unbounded off-chain renewal and rollover with the funding outpoint untouched throughout.

The ordering property is intact. A transfer co-signs a fresh state one δ lower than the one it
replaces (replace-by-lower-timelock, Decker–Wattenhofer at one dedicated tier), so the new owner's
state always matures **first**, and the state it replaces is disclosed to the receiver as *superseded*
and counted (§3, layer 3 — **INV-28**). Renewal does the same thing horizontally at the extension
tier: `X_{m+1}` strictly undercuts every older extension in the race for `T.out[0]`, so every
pre-renewal state hangs on a parent that can now never confirm (`sdk40` PART 2 and PART 3 kill a stale
ladder outright at the consensus level).

### 2.3 The clock that survives: the flat backup chain

The ladder is **not** the coin's only pre-signed material. Every coin also carries the **flat backup
chain**: signed-once transactions spending `F` straight to the owner's own address, at **absolute**
locktimes `L_k = L_0 − k·interval`, where `L_0 = H_deposit + initlock` and `k` counts whole-coin hops
(**INV-5**). The chain is built, conveyed and structurally validated on every transfer — its *count*
is a term in the receiver's census, and INV-5 is the only defence against a sender inverting or
padding it.

Consequences a reader must not skip:

- A coin that has been **received `k` times sits on `min(L_k)`** — a real, finite, approaching
  calendar height, and the copies that mature *above* it are held by its **prior owners**. When
  `min(L_k)` passes, an ancestor's matured rung spends `F` and takes the coin. The relative-locked
  tiers cannot out-race a transaction that is simply valid now.
- Broadcasting `T` spends `F` and therefore **kills every flat backup permanently**. That is why
  `deadline_safety_due`'s unilateral fallback is exactly "broadcast the trigger": `T` carries no
  timelock, so it beats every retained *timelocked* rung by being valid first
  (`clients/libs/rust-sdk/src/refresh.rs`; the sever is also reachable directly as `sever_from_f`).
- The chain is a finite budget: `initlock / interval` = **100 decrements**, of which **99 are
  usable** — hop 100 lands on the co-sign anchor and the receiver's `lock_time <= tip` rule refuses
  it.

`initlock` and `interval` are **compiled in per network** (`TesrParams::flat_ladder_params`,
`lib/src/tesr.rs`): **10,000 / 100 on mainnet, testnet and signet**; **1,000 / 10 on regtest**. The
mainnet epoch is therefore ~**69.4 days**; the ~6.9-day figure is the regtest clock and must never be
quoted as the deployed one. The coordinator publishes both at `GET /info/config`, but only as a
cross-check the client refuses to proceed past on mismatch — taking `interval` from the coordinator
would let the coordinator define the defence, and deriving it from the conveyed chain is circular (a
padded chain of uniform `interval/2` hops validates against itself). The ci-guard
`deny_flat_ladder_config_drift` pins that the coordinator refuses to boot against an env that
disagrees.

`sdk86` measures all three facts on one coin across two owners: after 300 idle blocks the received
coin's ladder fingerprint is byte-identical and `F` is unspent (the CSV half); the same coin's `L` is
300 blocks nearer (the calendar half); and each hop cost exactly `interval` more (INV-5).

### 2.4 What replaces "the timeout"

Four numbers, three of which are not a calendar:

- **the alarm** — the only way to start a CSV clock is to broadcast `T`, which spends the funding
  outpoint `F` and is therefore **publicly visible on-chain**. A watchtower subscribes to one outpoint
  per coin and does nothing until that happens;
- **the notice** — after `T` confirms, the newest extension needs `E_m` confirmations and the newest
  state another `Δ_k` before *anything* is final. That is ≥ 288 blocks (~2 days) of defender notice on
  the shipped schedule, and never less than 144 blocks (~1 day) for any one tier;
- **the head start** — δ = 36 blocks (~6 h) per whole-coin hop at the state tier, δE = 36 at the
  extension tier. This is the margin by which the honest owner's transaction outruns the newest stale
  rival, and it must absorb a plausible reorg plus watchtower reaction time;
- **the epoch** — `min(L_k)`, defended by `deadline_safety_due` at `auto_refresh_margin_blocks` = 144.
  This one *is* a calendar, and it is the reason a received coin is not maintenance-free.

### 2.5 What it costs, honestly

Three things. (a) **No unconditional no-watch window**: an absolute-ladder design gives a received
coin a period in which *the chain itself* rejects every stale backup and nobody has to be awake; TES-R
replaces that with perpetual — but alarm-driven, keyless, delegable — watching (residual **R-2**).
(b) **Longer worst-case unilateral latency**: a fresh flat coin's unilateral exit walks `T → X → S`
and waits `E_m + Δ_k` sequentially — 2,160 blocks of timelock plus one confirmation per transaction
(≈ 15 days), decreasing 36 blocks per hop; much deeper for children (§4c). (c) **Invalidation is
race-conditional, not axiomatic**: "consensus-dead" means the newest extension must win a
≥ 36-block-edge race after a public trigger. Strictly stronger than an absolute ladder's post-window
position, and categorically stronger than a key-deletion promise — but a race, not a theorem.

**Is the trigger itself dangerous?** Only someone holding a copy of `T` can start a coin's CSV clock,
and copies travel with the coin: the owner has one, and so does every *previous* owner. A
self-deposited coin that has never been transferred therefore has **no one who can trigger it**. When
a past owner (or a griefer) does trigger, the answer is the **cooperative de-trigger** — `T.out[0]`
pays the coin's own aggregate, so the owner and the SE key-path-spend it with **no relative timelock**,
confirming ahead of every pre-signed extension inside the ≥ 144-block window during which no adversary
transaction is even valid. `build_detrigger` emits a *tier*, anchor and all, so it is **125 vB**
(`TIER_VBYTES`), and it is driven end to end by `SDK_E2E=89`: the griefer's `T` confirms, the
de-trigger confirms spending `T.out[0]`, the value lands at an address the **owner** named, and the
pre-signed extension is then submitted to the node and refused for a missing/spent input.

Two things the de-trigger is not. It has **no restoration half**: it does not spend into a fresh
funding output `F′` and does not rebuild `T′/X′_0/S′_0`, so on the plain lane it is an **exit** —
getting back off-chain is a fresh deposit. And it is **not economically losing for the attacker**.
Both transactions pay out of the *coin*: a tier's fee is committed at signing
(`tier_out_value` = prev − `committed_fee(rate)` − `P2A_VALUE`), so at or below the committed rate the
griefer broadcasts a transaction he already holds and pays **nothing**, while the coin loses
`2 × (committed_fee + 240)` = **1,230 sats** at the shipped 3 sat/vB. Griefing is **cheap-to-free and
bounded**: the damage is fee-sized sats out of the coin, never the coin. The de-trigger needs the SE,
so it is a cost shield, never a safety dependency — the unilateral tree always exists without anyone.

## 3. The four defence layers

**Ladder schedule** — `TesrParams::mainnet()` (`lib/src/tesr.rs`), **compiled into the client, not
served by the SE**; arithmetic pinned by `sdk44`:

| Parameter | Value | Meaning |
|---|---|---|
| `d0` / `delta` / `d_floor` | 1,440 / **36** / 144 blocks | state tier: 36 hops per epoch, ~6 h head start per hop |
| `e0` / `delta_e` / `e_floor` | 720 / **36** / 144 blocks | extension tier: forced rollover at `m_max = 15`, i.e. 16 usable epochs |
| Hop budget per depth level | 36 × 16 = **576** whole-coin transfers | between depth increments (§4c) |
| `committed_fee_rate` | **3.0 sat/vB** | `committed_fee(3.0)` = 125 vB × 3 = **375 sat** per tier, plus a 240-sat P2A anchor |
| Worst fresh unilateral wait | `e0 + d0` = 2,160 blocks + 3 confirmations ≈ **15 days** | decreasing 36 blocks per hop/renewal |
| `SPINE_CSV` | **0** | a split state `SP` is its own kind, pinned at zero (§4b) |

A `TesrParams::regtest()` preset (24/6/6, 12/3/3, `m_max` 2) exists only so a full lifecycle fits
inside a test's mining budget; **testnet and signet run the mainnet schedule**, so the timings that
ship are the timings that are rehearsed, and an unrecognised network name panics rather than falling
through to a toy schedule. δ = 36 rather than a shorter value because the head start is the single
parameter everything stands on and mainnet has sustained > 4 h full-block spikes; the budget
sensitivity is stated in [PROTOCOL.md §5.2](../spec/PROTOCOL.md) (δ = 24 → 1,350 hops/level,
**36 → 576**, 72 → 162, 144 → 45). Because rollover is off-chain, a conservative δ trades exit weight
only — never chain rent.

**Flat-chain schedule** — `TesrParams::flat_ladder_params`, also compiled in: `initlock`/`interval` =
10,000 / 100 (mainnet, testnet, signet) or 1,000 / 10 (regtest); 100 decrements of capacity, 99
usable.

---

**Layer 1 — timelock ordering.** One idea, two mechanisms, on every coin.

*The tiers (relative).* Every owner holds the whole pre-signed tier chain. A whole-coin transfer
co-signs a new state at `Δ_{k+1} = Δ_k − 36`; a renewal co-signs a new extension at
`E_{m+1} = E_m − 36`. Because every lock is relative and every tier is un-broadcast, none of them
counts down while the coin rests. When someone does broadcast `T`, the tiers become a strict ordering:
the current owner's state matures ≥ 36 blocks (~6 h) before the newest stale one, and a stale
*epoch*'s states hang on an extension that can never confirm at all. Evidence: `sdk40` (consensus
enforcement, stale-ladder death, renewal supersession), `sdk41` (after Alice pays Bob, Bob's lower-CSV
state wins the exit race and Alice cannot claw back), `sdk51` (a hostile trigger defended end to end
by a watchtower pass), `sdk50` (the full unilateral walk).

*The flat chain (absolute).* Every owner also holds a pre-signed **backup transaction** spending `F`
to their own address with an absolute nLockTime anchored at the deposit: the depositor's locks at
`H + initlock`, where `H` is the chain tip when the SE first co-signs the deposit backup — at deposit
*detection*, in practice ≈ the confirmation height — and each transfer hands the new owner a backup
locking `interval` blocks *lower*. After `k` hops the current owner holds `L_k`, the lowest locktime
in existence for that coin. While the tip is below `L_k`, *no* pre-signed backup is final: the chain
rejects them all. From `L_k` to `L_{k−1}` the current owner has an **exclusive exit window** of
`interval` blocks in which their backup is the only final transaction on Earth for this coin. After
that, stale backups mature one by one (newest first) and honesty degrades to a first-seen broadcast
race the current owner has already had a head start in. `calculate_block_height`
(`lib/src/transaction.rs`) is the arithmetic; `ladder_decrements_by_interval` inside
`validate_signature_scheme` (`lib/src/transfer/receiver.rs`) is the receive-side enforcement, with
`LocktimeTooLow` and `LocktimeTooHigh` as its two named refusals.

On a **laddered** coin the flat chain is not the exit path — `unilateral_exit` walks the tiers and
broadcasts no absolute-locktime backup (`sdk50`) — but it is still the coin's calendar. On a
**flat-lane** coin it *is* the exit material, and `unilateral_exit`'s last arm is where that is
read: `branch-` rows first, then the latest backup.

**Layer 2 — SE refusal** (`server/src/endpoints/sign.rs`). The statechain entity refuses to co-sign
when:

- the request does not carry a schnorr signature by the coin's own auth key — **401** (`validate_signature`);
- a `single_use` coin already has one finalized signature — **410**, ERR-1;
- a coin's `sig_budget` is exhausted — **410**, ERR-3 ("terminal node");
- a coin's `epoch_deadline` (unix seconds) has passed — **410**, ERR-2;
- **a transfer of the coin is currently open** — **409 Conflict**, the *pending-transfer lock*.

All of these **fail closed**: any database error yields 503 and no signature. Crucially the gates are
re-checked in **both** legs — a signing session is two calls and `sign/first`'s state is durable, so
`sign/second` re-runs single-use, budget and epoch before the signature is issued. The budget is
monotonic: `set_sig_budget` (`server/src/database/deposit.rs`) writes
`min(count_finalized + remaining, existing)`, an absolute count on both sides, so a terminal node can
never be un-terminated through the API (**INV-19**), and terminal status is publicly auditable via
`GET /statechain/spend_budget/<id>`. Independently, the enclave loads and consumes each sealed
secnonce inside the same row-locked transaction (`lockbox/src/server.cpp`), so a second partial
signature over a different challenge finds it NULL and is refused (**INV-23**, `sdk12`).

Be precise about what that last one is. **There is no "enclave single-active-state refusal."** The
enclave does not track which state is current and cannot refuse a rival: it *must* co-sign rivals,
because that is what a renewal is. One-signature-per-secnonce is a MuSig2 nonce-reuse *key-leak*
defence; `sig_budget` vs the lifetime `sig_count` is what makes terminalization enforceable. The
second layer over the consensus race is the **receiver's census** (layer 3), not an SE promise.

The **pending-transfer lock** is load-bearing (see [CHILDREN.md](../spec/CHILDREN.md)): once
`/transfer/sender` opens a transfer, the coordinator refuses every further co-sign on that statechain
id, and `has_open_transfer_to_other_auth` (`server/src/database/transfer_sender.rs`) refuses to
re-address an open transfer to a different recipient. It closes the window in which a still-owner
sender could co-sign a rival after the receiver had already checked everything. It is releasable, not
monotonic, so it never fights the budget clamp — and it is only safe because every legitimate sender
pre-sign happens **before** `get_new_x1` opens the transfer, the ordering `sign_first`'s own gate
records (`server/src/endpoints/sign.rs`).

A second hazard lives in the same window, and it is the sender's *own* watchtower. `defend_ladders`
broadcasts a retained state only for a coin whose local status reads `CONFIRMED`, and it re-reads
that field every pass — so a lane that writes the status *after* the recipient already holds material
leaves a window in which the sender's tower reads a stale `CONFIRMED` and broadcasts over the very
outpoint the recipient's new state depends on. Every value-handing lane therefore writes a durable
`CoinStatus::IN_TRANSFER` **before** the first call that can produce material for anybody else — the
receiver-paying co-sign, the backup co-sign, the coordinator open — and refuses if that write fails.
The ci-guard `deny_armed_tower_during_conveyance` pins it per lane on code rather than prose:
position, statement extent, and refusal-on-failure, each clause exercised against a replanted
mutation.

> **The honest limit of that lock, measured.** The non-batch branch of `OPEN_TRANSFER_WINDOW_SQL`
> (`server/src/database/transfer_sender.rs`) is a hard-coded `updated_at > NOW() - INTERVAL '1 hour'`.
> `sdk91` drives it on a live stack: a payer who skips his own client and POSTs `/sign/first` directly
> with his own genuine credential gets **HTTP 409** while the window is open, and **HTTP 200 with a
> `server_pubnonce`** once the row is older than an hour — the timer expires on wall-clock time
> whether or not the payee has claimed. So that window is the **only server-side gate on this path**.
> `sdk90` measures the two client-side gates (the wallet's own coin lookup and
> `refuse_outstanding_conveyance`, `clients/libs/rust/src/tesr.rs`) and reaches no conclusion about
> the server, because both are the payer's own software. Scope: a `sign/first` session is the first
> link, not a theft — `sign/second` and a broadcast race against the payee's strictly-lower-CSV state
> still stand between it and money moving, and in the measured run the payee claimed his coin intact.
> The specified fix is SPEC **REQ-61**, the owner latch (co-signing bound to ownership rather than to
> elapsed time); it is **design, not built**, and `EXPECT_LATCH=1` converts both recordings into hard
> assertions the day it ships.

What terminality is used for is narrow and sharp. **An in-ladder split terminalizes the node being
split** (the parent for a root split, the child for a child-level split), so after a split even the
legitimate owner cannot get that node co-signed again — `sdk04` pins the refusal, with the cause
asserted negatively so a plumbing error cannot make it pass vacuously. An **RGB carrier's ancestry is
terminal-frozen** so no colored anchor can ever be re-signed out from under an allocation. The
**piece being conveyed is deliberately *not* terminalized** — the census closes any pre-conveyance
rival and the SE key handover closes every later one. The single exception is the
**Lightning-latched piece**, which sits unclaimed past the pending lock's window while the SSP settles
on its own schedule, so it is terminalized instead: a permanent lockout
([LIGHTNING.md](../spec/LIGHTNING.md), PROTOCOL.md §5.12).

**Layer 3 — receiver-side verification** (`clients/libs/rust/src/tesr.rs`,
`clients/libs/rust/src/transfer_receiver.rs`). A receiver trusts neither sender nor SE blindly.

*Laddered — the R′ census.* The receiver rebuilds the whole tier structure from public data.

- **(R3′)** `F` is on-chain, unspent and pays the aggregate `A`.
- **(R4′)** `T` spends `F` and carries no timelock; every tier output pays `A` plus the public tagged
  tweak; and every later tier's **signed** nSequence is a BIP-68 *block* relative timelock lying
  inside the band its kind allows — `[e_floor, e0]` for an extension, `[d_floor, d0]` for a state,
  exactly `SPINE_CSV = 0` for a spine tier (the split state `SP` itself). A spine **tip's cap** is
  deliberately *not* pinned to zero: `SpineTipBundle::validate` requires it in the state band
  `[d_floor, d0]`, because a cap at zero would leave the next batch's `SP` no margin to out-race it
  and the builders' own `s0_csv <= SPINE_CSV` guard would then refuse to build that batch, stranding
  the tip. The tier's **declared** `csv` field is bound to that same
  signed number by `bind_declared_csv` (`lib/src/transfer/receiver.rs`), so a bundle whose two copies
  disagree is rejected rather than believed on either — `sdk82` executes that bypass and then shows
  the shipped verifier refusing it by name.
  **On the child lane the band is not the whole check.** `verify_child_bundle` additionally requires
  the CSV to sit **on the schedule's grid** — `is_on_ext_grid` / `is_on_state_grid`
  (`lib/src/tesr.rs`) admit only `e0 − m·δE` / `d0 − k·δ` and the floor clamp — because an honest
  renewal steps by exactly δE and an honest hop by exactly δ, so a value between two rungs is a state
  the design does not define and the *sender* chose it at 1-block granularity. And a child that
  discloses **no** superseded tiers is a fresh mint, which must be minted at the schedule **head**:
  anything below `e0`/`d0` on a bundle with nothing to disclose is budget the sender spent before
  handing the leaf over, and it is refused by name. Grid and head-equality are both positional; what
  no check can be is *absolute*, because nothing serves `m` or `k` (they are fields of the sender's
  own bundle). The band's endpoints are not the sender's either: `cap_schedule` runs **before** the
  census on both receive paths and measures every conveyed `TesrParams` field by field against the
  receiver's OWN network preset, refusing by name on the first disagreement — strictly stronger than
  publication, because it holds against a lying coordinator. The remaining question — whether an
  undisclosed co-sign hides between the disclosed tiers — is carried by R5′, and the rival margin is
  read off the **live** rival's structural position (`RivalKind::margin` — δ for a state, δE for an
  extension) rather than off which conveyed list a superseded tier arrived in.
- **(R5′)** the SE's signature count equals the exact expected tree size. `verify_bundle_bound`
  enforces **`se_num_sigs == flat_backups + tiers + superseded`**. Any hidden extra co-signed state or
  extension shows up as a count mismatch. `flat_backups` is the conveyed signed-once chain's length —
  `PARENT_V2_BASELINE = 1` for an ordinary on-chain root coin, whose deposit co-signs one backup
  before the ladder is established, and `CHILD_V2_BASELINE = 0` for a derived child slot that never
  ran `create_tx1` — so the term is never assumed zero and never assumed one. Each hop discloses
  exactly one superseded state, so an undisclosed rival cannot hide; at depth 1 in `sdk60` the child
  census reads `child_num_sigs == 0 + 2 + 1`.
  **The count is not taken on the coordinator's word.** `get_statechain_info`
  (`clients/libs/rust/src/utils.rs`) sends a fresh random 32-byte nonce and refuses any answer that
  does not carry a `utexo/sig_count/v2` schnorr signature over (statechain id, `num_sigs`,
  budget-presence, `sig_budget`, nonce), verified by `verify_sig_count_attestation`
  (`lib/src/transfer/receiver.rs`) against a **pinned enclave attestation identity**
  (`utexo/attestation-identity/v1`, derived from the enclave seed and published by the lockbox at
  `GET /attestation_identity`). Resolution is
  compiled-in pin → configured value → **refuse**; never a fallback to the key served in the same
  response. Pinning rather than chain-anchoring is required because a depth-≥2 split ancestor's
  funding output is deliberately un-broadcast, so there is nothing on chain to bind to
  (TRUST-MODEL **B11**). Terminality is derived the same way — `attested_terminal` reads it from the
  enclave-signed payload and keeps the coordinator's `spend_budget` answer only as a cross-check that
  refuses on disagreement (ci-guards `deny_unattested_terminality`, `deny_unattested_num_sigs_reader`).
- **Binding, not just consistency.** `verify_bundle` proves a ladder is *internally* consistent, which
  is the wrong question: a sender can convey a self-consistent decoy ladder over an
  attacker-controlled outpoint with the census padded to balance, and keep the real trigger.
  `verify_bundle_bound` is the acceptance-path entry point: the bundle's statechain id, funding
  outpoint, funding value and aggregate address must all match the coin being accepted, and that
  aggregate must equal the coordinator's recorded (and `UNIQUE`-constrained) aggregate for the sid.
  Adversarially exercised by `sdk70`.
- **The exit-headroom gate.** A child's unilateral exit is a chain of sequential relative timelocks,
  and the funding epoch is only `initlock` blocks long. `check_exit_headroom_with_margin`
  (`lib/src/transfer/receiver.rs`) refuses a conveyed child whose exit provably cannot finish before
  `min(L_k)` passes, naming the shortfall — computed from the *signed* chain, plus
  `exit_slack_margin` = `max(required/4, required/tiers)`. Without it a sender could hand a payee a
  coin that was worthless while the census balanced and Model A held. `sdk82` drives it on the plain
  lane, `sdk88` on the coloured one (where every tier is 168 vB rather than 125, so the floors differ).
- **The split-depth cap is derived, not a literal.** `max_split_depth(base, per_level, epoch_blocks)`
  searches for the deepest child whose exit still fits under that same margin, and
  `enforce_split_depth_cap_shaped` (`clients/libs/rust/src/tesr.rs`) enforces it. On mainnet the
  answer is **depth 8**, i.e. a **19-transaction** exit chain; on regtest 54 and 111. It moves with the
  network profile, which is the point — the ci-guard `deny_stale_depth_cap` exists because a builder
  that trusted a stale closed form once minted children no receiver would adopt, after terminalizing
  their parent.

Evidence: `sdk46` (the census against the *real* SE counter — accepts the true count, rejects a hidden
extra signature), `sdk47` (R′ across a transfer of a pre-established ladder), `sdk54` (adversarial
`verify_bundle`), `sdk58` (12 adversarial in-ladder-split cases, all REJECT — aggregates, hidden
state, Model-A, parent terminality, child-superseded race, count padding, value spoof), `sdk60` and
`sdk17` (the N-hop child census), `sdk76` (a received parent's split ancestor census).

*Flat lane — branch validation.* The receiver checks the backup ladder decrements and signature
count, then runs `validate_branch` (root on-chain, unspent, confirmed; every branch tx locktime
`≤ tip` — **INV-4**; value conservation Σout ≤ Σin per hop — **INV-25**; `reject_non_tree_branch` for
any branch consuming an outpoint twice; full script/signature verification) and
`verify_terminal_parents` (`required_terminal_ancestors` names one terminal ancestor per structural
input the branch consumes, so a multi-input combine names all N — **INV-20** — and each must report
terminal, ERR-7). Blind-SE caveat (SPEC §14, TRUST-MODEL **B2**): ancestor *ids* are not
cryptographically bound to branch outpoints, so the count check defeats omission, not substitution;
the compensating control is that the receiver holds the full locktime-free branch and can materialize
immediately. **Laddered coins are structurally exempt from B2** — `verify_child_bundle` derives
`A_parent` from the *fetched on-chain* `F.spk` and walks each intermediate segment deriving its
aggregate from the funding output it actually spends, so a substituted id fails on the key, not on a
name. Adversarial coverage: `sdk55`.

*The coloured lane rule.* On a coloured bundle every conveyed flat backup **must be plain**: an
`OP_RETURN` on one is refused by name by `verify_flat_backup_lane`
(`clients/libs/rust/src/tesr.rs`), run on both acceptance paths and guarded by
`deny_colored_backup_on_a_colored_ladder`. Without it a prior owner's retained *coloured* backup is an
undetectable allocation-theft primitive — it spends `F` and re-assigns the allocation to themselves,
invisible to every other receiver check.

**Layer 4 — watching.** The wallet's background task (`start_background`,
`clients/libs/rust-sdk/src/wallet.rs`) runs three deadline defences, in order. They are not
interchangeable.

1. **`deadline_safety_due(margin)`** at `auto_refresh_margin_blocks` = 144 — the whole-coin `min(L_k)`
   clock, and it is **unconditional**. It tries the cooperative re-anchor first (route 1) and, if that
   is refused, **severs from `F`** by broadcasting the already-co-signed trigger (route 2). The
   fallback is the point: the party that would most like this coin's deadline to pass is the same
   party being asked to co-sign the re-anchor, and a defence the adversary can decline is not a
   defence. The pass is unconditional by construction — the ci-guard `deny_optional_deadline_safety`
   exists because putting it behind the routine-maintenance flag would have disabled it on every
   default wallet. `sdk87` drives the carrier variant and asserts the allocation survives the sever.
2. **`defend_ladders()`** — one `watch_pass` per adopted `tesr-` bundle plus one `watch_child_pass`
   per adopted `ctesr-` split child, wired into the background loop unconditionally and gated to one
   pass per new block (a relative CSV can only mature on a block). If `F` is unspent it is a **no-op**.
   If someone has triggered the coin, the pass races the owner's tiers, broadcasting each as its
   relative timelock matures; because the adopted current state carries the strictly-lowest CSV
   (enforced at adoption), it matures first and the funds land at the owner's own key. It emits
   `WalletEvent::LadderDefended{tiers_broadcast}`, is idempotent and incremental, and the bundle
   carries **zero key material**, so the duty is fully delegable and a second independent tower is
   idempotent (both asserted in `sdk45`). `sdk79` and `sdk80` cover the split and plain-child-split
   lanes.
3. **`auto_exit_due(margin)`** at `auto_exit_margin_blocks`, which is **derived, not chosen**:
   `auto_exit_margin_blocks_for(k_max, interval, child_depth) = k_max·interval + tesr_exit_txs(d)·144`
   (`clients/libs/rust-sdk/src/config.rs`), evaluating to **2,120 blocks on mainnet** (14·100 + 5·144)
   and **860 on regtest** (14·10 + 5·144). It force-exits plain sub-coins that carry an exit branch
   (a verified-empty branch means no ancestor can race the coin, so it is skipped) and
   **materializes** received token carriers (branch only — a plain sweep would destroy the
   allocation), emitting `ExitDeadlineApproaching` / `TokenCarrierMaterialized` / `LeafExitForced`.
   The `k_max = 14` term is an **assumption**, not a measurement (§5).

Routine **background** re-anchoring is default-**off** (`background_auto_refresh = false`), because
paying rent on an idle coin is an economics choice — the re-anchor cost is folded into `transfer` and
paid on demand as part of the payment fee. That flag governs *maintenance*, never *safety*: the
`deadline_safety_due` half runs either way.

*Flat-lane specifics.* Structural branch transactions are **locktime-zero** (INV-4,
`lib/src/transaction.rs`): broadcastable *now*, they beat any deposit-anchored stale backup — provided
they reach the chain before the earliest stale ancestor backup matures. `estimate_exit_cost` surfaces
that bound as `exit_deadline_block` (`ExitCostEstimate`, `clients/libs/rust-sdk/src/types.rs`), and
`None` alone is **not** "safe": read it with `exit_deadline_blind`, which carries a reason when the
deadline could not be computed, and with `deadline_is_unknown()`. Meanwhile `unilateral_exit`
broadcasts branch-first, reports remaining `wait_blocks` instead of failing, and raises
`WalletEvent::ExitBranchConflict` if a *different* transaction is spending the branch root.

*What a keyless tower cannot do — normative.* It can watch `F` and broadcast the pre-signed tiers **at
their committed fee**. It **cannot fee-bump them**: a CPFP child spending the P2A anchor needs a
funding input it does not hold and a signature it cannot make, so above the relay floor a tier is
refused at `sendrawtransaction` and the tower has no move. Nor does the anyone-can-spend anchor supply
a rescuer: the child's change must clear `CHILD_CHANGE_DUST = 330` while the anchor is worth
`P2A_VALUE = 240`, so an anchor-only child can never produce a legal change output at any fee rate.
"Anyone-can-spend" is a permission, never an incentive. The party that funds a CPFP package is the
**coin owner**. `mercurylib::wallet::p2a_fee_child::build_p2a_fee_child` builds the v3 owner-funded
child (**153 vB**, measured), `mercuryrustlib::core_rpc::submit_package` submits the 1P1C package to a
Bitcoin Core node, and `exit_pass_with_bump` / `watch_pass_with_bump` are wired into `unilateral_exit`
and `defend_ladders` whenever `SdkConfig::fee_bump` supplies an owner fee source — which ships as
`None` on **both** presets. The capability is an explicit argument, never ambient config, so a plain
keyless pass reports a fee-stuck tier as a **stated limit** rather than one more retryable failure
(ci-guard `deny_unqualified_keyless_rescue`). **The honest gap**: the two tests that exercise the
rescue (`live_p2a_package_rescue.rs`, `live_tower_float.rs`,
`clients/libs/rust/tests/`) need a Bitcoin Core RPC endpoint and skip — loudly — without one, so a
green suite run is not evidence the rescue works (residual **R-4**). The child lane is narrower still:
`exit_child_pass_with_bump` exists, but `watch_child_pass_seen` has no bump variant.

## 4. Lifecycle walkthroughs

### 4a. A laddered coin, deposited and transferred three times

Alice deposits; her funding tx confirms and `claim()` establishes the ladder, emitting
`LadderEstablished`. She then pays Bob a whole coin, Bob pays Carol, Carol pays Dave — all off-chain,
minutes apart, none touching the chain.

| State | Holder | Relative CSV on `X_0.out[0]` | Status after hop 3 |
|---|---|---|---|
| `S_0` (deposit state) | Alice | 1,440 | superseded, disclosed |
| `S_1` | Bob | 1,404 | superseded, disclosed |
| `S_2` | Carol | 1,368 | superseded, disclosed |
| `S_3` | Dave | **1,332** | current — lowest |

All four are mutually-exclusive spends of the same output, and all four are inert. `F` is untouched,
`T` is un-broadcast, and `X_0` (CSV `E_0 = 720`) is un-broadcast too. The chain contains exactly one
transaction for this coin: Alice's deposit. **Nothing in this table changes with time.** What *does*
change with time is the flat chain underneath it: Dave holds `L_3 = H + 10,000 − 300` on mainnet, and
each of those three hops spent 100 blocks of the coin's epoch.

Now suppose Bob — a past owner — decides to steal. His only move is to broadcast `T`:

```
   T confirms                X_0 confirms        +1,332      +1,368     +1,404
   |                         |                   |           |          |
   | PUBLIC ALARM:           | the extension is  | S_3 Dave  | S_2      | S_1 Bob
   | F is spent; every       | on-chain; state   | (current) | Carol    |
   | tower watching that     | CSVs start HERE   |           |          |
   | one outpoint sees it    |                   |           |          |
   |<- 720 blk (~5 d): X_0 ->|<- 1,332 blk (~9 d) ---------->|
   |   is not even valid     |                   |<- 36 blk ->|
   |                                                (~6 h): Dave alone
```

At each boundary:

- **before `T` is broadcast** — indefinitely, at zero CSV cost and with no watching action required.
  There is no transaction anyone could send that the chain would accept. (The one thing still running
  is the calendar: `min(L_3)`.)
- **`T` confirms** — the alarm. `defend_ladders()` sees `F` spent and switches from no-op to active.
  The preferred response is not a race: Dave and the SE **de-trigger**, spending `T.out[0]` with no
  relative timelock inside the 720-block window during which no pre-signed extension is valid. Bob has
  bought Dave a 125-vB on-chain settlement at a moment Dave chose, and bought himself nothing.
- **`T` + 720** — if the SE is unavailable and the de-trigger cannot happen, `X_0` is broadcast. Bob
  waits exactly as long: the extension is *shared*, not per-owner.
- **`X_0` + 1,332** — Dave's `S_3` is spendable, and it is the only spendable state on Earth for this
  coin for the next 36 blocks. Bob's `S_1` cannot confirm until `X_0` + 1,404. Dave (or his tower,
  keylessly) broadcasts and the coin settles at Dave's own key.

What each hop consumes is a *decrement*, not lifetime: the current owner's edge over the previous one
stays a constant 36 blocks however many hops have happened, while the number of decrements left before
`d_floor` shrinks by one. When the next decrement would breach the floor, the SDK renews the extension
off-chain and the state tier starts again at `d0` (§4c). Evidence: `sdk41` (this race, run for real),
`sdk40` (the consensus properties underneath it), `sdk51` (the watchtower response), `sdk42` (the whole
lifecycle including persistence and reload).

### 4b. A partial payment: the in-ladder split

Payments are arbitrary amounts, and an arbitrary amount equals a coin the sender already holds only by
coincidence — simulated against a realistic mix, the exact-subset hit rate is **5 in 3,000**
([PARTIAL-PAYMENT-ECONOMICS.md §1.2](../spec/PARTIAL-PAYMENT-ECONOMICS.md)). So **essentially every
payment is an in-ladder split**, and its shape is the single most safety-critical detail in the
system.

Alice holds a laddered 50,000-sat coin and owes Bob 20,000 sats.

1. The SDK checks admission **before touching anything**, and the floor is **per leg**, not one number
   for both. Bob's piece is a full child — it must fund its **own** extension and state tier, each
   burning `committed_fee + P2A` = 615 sat, and still clear dust — so
   `min_child_value(rate, dust)` = `2·(committed_fee + 240) + 330` = **1,560 sat** at the shipped
   3 sat/vB (`lib/src/tesr.rs`). Alice's change leg is a **spine tip**: one cap tier over `SP.out[K]`
   and no extension, so `min_spine_tip_value` = **945 sat**. Applying the tip's floor to a payee's
   piece admits a piece that dies inside `establish_child` — *after* the parent is terminalized — so
   the leg's shape is not a caller's choice: `change_leg_role` derives it from the lane, and a
   **child**-level split's change leg is a full `Piece` at 1,560, not a tip.
2. The SDK sets the parent's spend budget to `finalized + 1`, then co-signs the **split state `SP`**.
   `SP` spends **`X_m.out[0]`** — it is a *state tier*, a **descendant of the trigger**, and it is
   **not** a rival spend of `F`. That is the whole game: a past owner's retained no-timelock trigger
   has nothing to race, because the split does not compete for the funding outpoint.
   `SP` is signed at **`SPINE_CSV = 0`** — a spine tier is its own kind, not a rung on the state
   schedule. Zero is right rather than merely cheap: over the outpoint it spends, the only competing
   transaction is the state it replaces (whose CSV is necessarily ≥ `d_floor`), so
   replace-by-lower-timelock wins by the largest possible margin; and the retained-untimelocked-tier
   hazard does not arise, because `SP` is signed by the sole current owner of the outpoint it is
   simultaneously giving up — the voiding party and the victim are the same entity. The builders
   refuse outright unless the `S_0` being replaced sits strictly above it. Consequently **a split
   consumes no state rung at all**, and `SP` contributes one block (its parent's confirmation) to the
   exit walk rather than a full CSV.
   `SP` pays exact resting outputs (Σout = Σin − committed fee) plus its P2A anchor, and is
   **un-broadcast** like everything else. A multi-child `SP` is charged
   `committed_fee_for_outputs(n, rate)` over `TIER_VBYTES + (n−1)·43` — quoting the one-payload
   constant understates it.
3. `establish_child` hangs each child's own extension + state tiers off its resting output — no
   trigger needed, because `SP` is itself un-broadcast, so nothing below it ticks until `SP` confirms.
4. The split consumed the parent's last co-signature: the parent is **terminal** at the SE, and
   `GET /statechain/spend_budget/<parent>` shows it to the world (`sdk04`).
5. Alice conveys the child bundle **with the key-handover material** — `x1` from `get_new_x1`,
   `t1`/`transfer_signature`, and the ancestor chain `F → T → X_m → SP` so Bob can validate over
   un-broadcast funding — under message shape **4**, "a child conveyance with key handover".
   `ADMISSIBLE_PROTOCOL_VERSIONS = [0, 2, 4]` is an exact set (`admissible_shape`), so an unknown
   value is refused by name rather than read as "at least". She does **not** terminalize the child.
6. Bob claims: `verify_child_bundle` (parent `F` read on-chain through the ancestor chain,
   exact-equality census, headroom gate, depth cap), then he **completes the key handover** — the SE
   rotates its share so that `A_child` is *invariant*
   (`sender_share + SE_old == receiver_share + SE_new`), which is exactly what keeps the pre-signed
   child exit chain valid, and re-points `auth` to Bob. Alice is now **permanently locked out**.

Stale-state inventory after the split: Alice's superseded state, disclosed and counted, rivalling `SP`
over `X_m.out[0]` and losing to it by the whole state schedule. Nothing is on-chain and the funding
outpoint is still unspent — but Bob's child inherits the **parent's** `min(L_k)`, because a derived
child slot has no flat backup of its own (`CHILD_V2_BASELINE = 0`). That is what the headroom gate is
measuring against.

**The child is first-class, not an exit-only claim.** Bob can pay it onward off-chain — whole via
`child_retransfer`, or split again via `child_in_ladder_pay` / `child_in_ladder_pay_many`. A whole-coin
re-transfer builds a replacement state over the *same* `ext_child.out[0]`: it spends zero sats, adds
zero depth, and costs exactly **one co-signature** while disclosing exactly **one superseded state**,
which the next receiver's census counts and proves out-raced. A child-level split terminalizes the
*child* and hands the terminalized segment to the grandchildren as an ancestor. The rule is uniform at
every level: **the node being split is terminalized; the piece being conveyed is not.** `sdk60` runs
alice → bob → carol with the funding outpoint **unspent throughout** — only Carol's exit ever touches
the chain; `sdk17` runs a partial second hop. Attack coverage: `sdk58` (12 cases, all REJECT),
`sdk59` (the end-to-end payment), `sdk77` (the coloured split), `sdk81` (split recovery).

Why the child is not terminalized, and why its budget is never re-opened: terminalize-then-reopen
fights the monotonic clamp (INV-19), and the sender stays the child's owner until the receiver
completes, so it could re-address the pending row to an attacker key *after* the victim accepted,
self-complete, and reopen to itself — a double-spend of the child. With the handover instead, the
sender's share is rotated out, so it can never co-sign a child rival at all. The child's two-layer
safety is one indivisible change: the **census** closes any *pre*-conveyance rival, and the
**pending-transfer lock** closes any *post*-conveyance rival until the handover makes the lockout
permanent — with the lock's one-hour limit as recorded above.

#### The same payment on the flat lane

Coloured splits on the legacy lane — the lane a network with no pinned enclave takes — travel the
branch machinery, and this is where the locktime-0 branch lives. (The other producer of this shape,
a plain off-chain split of a coin that had no ladder, is **gone**: `split_coin` and the plain split
are deleted, so the walkthrough below is the coloured lane's, and any coin still carrying `branch-`
rows is exit material rather than a splittable parent.) Alice deposits 50,000 sats at
**H = 100,000** on the regtest profile (`initlock` 1,000, `interval` 10; backup `L₀` = 101,000,
never transferred). At height **100,300** she splits:

1. The parent's budget is set to `finalized + 1`, then the **split tx** is co-signed: one input, two
   outputs — 20,000 to a fresh 2-of-2 for Bob's piece, change to a fresh 2-of-2 for Alice, with a fee
   reserve `(amount/100).clamp(300, 2000)` left behind as the split tx's miner fee. The split tx has
   **locktime 0** and is *not broadcast*.
2. The parent is terminal, publicly.
3. Each sub-coin gets a **fresh first backup** at split time: locktime `100,300 + 1,000 = 101,300`.
   Depth does not consume ladder.
4. Bob verifies branch linkage root-first: the root input is on-chain, unspent, confirmed; the split
   tx's locktime is 0 ≤ tip; no value created; scripts verify; ≥ 1 terminal ancestor named and
   reporting terminal.

Stale-state inventory: exactly one item — Alice's parent deposit backup at **101,000**. It is
locktimed; the branch is not. Bob's `exit_deadline_block` is `H + initlock = 101,000`, and here it is
**exact** (up to the small deposit co-sign→confirmation gap) because the parent was never transferred
before the split. If Bob distrusts anything, he broadcasts the branch (fee already committed) and waits
out his own fresh ladder.

```
height   100,000       100,300                101,000      101,300
         |             |                      |            |
         | deposit H   | split (locktime 0,   | parent's   | sub-coins' fresh
         |             | un-broadcast)        | stale bkup | backups mature
         |             |                      | matures    |
         |<———— branch is broadcastable anywhere here ————→|
                        Bob MUST have the branch on-chain before 101,000
```

That deadline is real, and it is why `auto_exit_due` exists and is default-on for this shape.

### 4c. Depth: off-chain rollover, child chains, and the flat-lane tree

**Laddered: depth is bought off-chain and bounded by a derived cap.** When the next state's CSV would
fall below `d_floor`, the SDK renews inside `transfer()`: two blind co-signs mint `X_{m+1}` (CSV
`e0 − (m+1)·δE`) and the transfer's own state `S'_0` on top of it — **zero on-chain bytes**.
`mercuryrustlib::tesr::renew` is exactly two `cosign_tier` calls over the ordinary
`/sign/first` + `/sign/second` pair; `m` and `k` are fields of the client's own bundle, advanced
locally. **There is no SE-side renewal counter machine and none is planned** — no `/renew/init` route,
no `total_sigs` column, no `{level, m, k}` state. The census does not need one: exact equality on the
TOTAL detects a hidden co-sign at *any* level, so per-level counters add nothing (residual **R-6**).
`renew_child` does the same for a received child (`sdk84`).

When the extension tier exhausts (`m = 15`), the SDK performs an off-chain **self-split rollover**: a
1-in-1-out self-split rollover consuming the current state slot, whose child resting output hosts fresh extension
+ state tiers and a fresh 576-hop budget. Cost: zero on-chain, +2 pre-signed txs (~250 vB) of
*contingent* exit weight, +1 depth level; the parent is terminalized. `sdk43` drives renew → rollover →
renew past epoch exhaustion, unattended, with the funding outpoint untouched, then exits unilaterally
through the whole deep chain.

Depth costs exit *weight* and *latency*:

| Quantity | Formula | Symbol |
|---|---|---|
| Transactions | `3 + 2d` | `tesr_exit_txs` |
| Block space | `293·d + 375` vB | `tesr_exit_vbytes` |
| Wait | `720·d + 2,160 + (3 + 2d)` blocks | `tesr_exit_wait_blocks` |

(all in `clients/libs/rust-sdk/src/config.rs`; the shape matters — a two-tier level costs `SP` + an
extension, a spine level costs `SP` alone, so `ExitShape` is an argument rather than an assumption.) A
depth-1 leaf is 5 transactions, 668 vB and 2,885 blocks (~20 days); the mainnet cap of depth 8 is 19
transactions, 2,719 vB and 7,939 blocks (~55 days), and the cap is exactly where
`check_exit_headroom_with_margin` stops fitting inside the 10,000-block epoch. Cooperative exit remains
one transaction and one confirmation at any depth. An optional per-level geometrically shrinking
`e0`/`d0` schedule would bound the total worst wait further, trading per-level hop budget; it is a
dial, default off (open problem **O-4**).

The default SDK policy also caps *rollover* depth: past the cap the next transfer triggers a **solo
compaction** — one on-chain re-anchor, 112 vB, non-interactive — rebearing the coin at depth 0 and
priced into that transfer's fee. Net budget between chain touches: 576 × 4 ≈ **2,300 transfers per
112 vB**, and a user who tolerates depth may raise the cap.

**Flat lane: depth is a tree, and the deadline is the minimum over its ancestors.** Same deposit,
**H = 100,000** on the regtest profile, splits at 100,050 (parent → A + change), 100,120 (A → B +
change), 100,200 (B → leaves), nothing transferred in between:

| Node | Created | Terminal at SE? | Its (now stale) backup locktime | Held by |
|---|---|---|---|---|
| Root parent (deposit) | 100,000 | yes (split 1) | 101,000 | original owner |
| A (split-1 piece) | 100,050 | yes (split 2) | 101,050 | whoever split A |
| B (split-2 piece) | 100,120 | yes (split 3) | 101,120 | whoever split B |
| Leaves (split-3 outputs) | 100,200 | no — live coins | 101,200 (current, not stale) | current owners |

The earliest hostile maturity across *all* stale ancestors is `min(101,000; 101,050; 101,120)` =
**101,000** — the root's deposit backup. Because every fresh sub-ladder anchors at its own later split
height, *and nothing here was transferred between splits*, one number — `H_deposit + initlock` — bounds
the entire tree, however deep and bushy. That "nothing transferred in between" is load-bearing: an
intermediate ancestor transferred `k` times *before* its split retains a backup at its own anchor
`+ initlock − k·interval`, which undercuts the root's as soon as `k·interval` exceeds the split-height
gap. The true deadline is the minimum over **every** ancestor's retained backups, not the root's alone
(TRUST-MODEL **B6**). A coloured tree is exactly this shape; `sdk39` materializes a token piece two
coloured splits deep with its allocation intact.

## 5. Over time: what a coin actually costs to hold

**Laddered: no CSV rent, and one calendar.**

- The tiers consume **0 vB per year** of block space, forever, and require no renewal traffic.
- No pre-signed *tier* anywhere can become valid until someone spends `F` in public and then waits out
  ≥ 288 blocks of relative timelocks.
- The **hop budget** is finite per depth level, not per coin: 36 state decrements per epoch × 16
  epochs = **576 whole-coin transfers** before the SDK adds a depth level by rolling over — off-chain,
  automatically, inside `transfer()`. `needs_renewal(k)` and `needs_rollover(m)` (`lib/src/tesr.rs`)
  are internal scheduling predicates a user never sees.
- The **epoch** is finite and shared: `min(L_k)`, `initlock` blocks from the deposit, spent from two
  directions — 1 block per block of wall clock, and `interval` per whole-coin hop. On mainnet that is
  10,000 blocks total and 100 per hop. This is what `deadline_safety_due` defends, and it is the real
  maintenance cadence.

**Flat lane: the calendar is the whole story.** A carrier and a sub-coin over un-broadcast funding
rest on the absolute-locktime backup anchored at the root deposit. A *received* one must reach the
chain before the earliest stale ancestor backup matures. The extension options, with real costs:

| Option | What it extends | What it does NOT extend | Cost |
|---|---|---|---|
| **Re-anchor** — `refresh` / `refresh_sponsored` | **Everything** — the coin moves to a brand-new funding outpoint with a fresh chain and `k = 0` prior owners; the old outpoint is spent, so every old backup dies | — | **1 on-chain tx** (`BACKUP_TX_VBYTES` = 112 vB, SE-co-signed spend into a fresh aggregate) + a derived slot. Fee **user-paid** (coin = amount − fee) or **operator-paid** (a funded sponsor rebates off-chain). Cooperative. `sdk30`, `sdk38` |
| Self-split | The *leaf* chain: the new sub-coin anchors at `H_split + initlock` with a fresh budget | The **root deadline** — unchanged, still bounds the whole tree | One SE co-sign; a 300–2,000-sat fee reserve locked into the branch; more future exit weight |
| Materialize the branch | Puts the sub-coin's funding on-chain; the coin becomes flat and its **own** chain governs | Its own chain (already ticking since the split) | Branch miner fees (pre-committed reserves) |
| Cooperative withdraw + redeposit | Everything | — | 2 on-chain txs — the re-anchor gets the same result in one |

**The deadline, exactly.** `exit_deadline_block = H_deposit_root + initlock` is deposit-anchored, and
it is a *safe, early* bound in one case and **too late** in another:

- **Exact** (up to the deposit co-sign→confirmation gap) when the split parent was never transferred:
  its only stale backup is the deposit backup.
- **Too late by `k·interval`** when the parent was transferred `k` times before the split — the
  splitter's own retained backup matures that much earlier. On the mainnet profile a parent transferred
  20 times then split has a true deadline `2,000` blocks (~14 days) below the reported one. A receiver
  who timed an exit to the reported number could be clawed back inside that gap; in a deep tree the
  same applies per ancestor.

This is TRUST-MODEL **B6**, and its status is *scoped and half-closed*. The shipped half is
`auto_exit_due(margin_blocks)`, which force-exits owned off-chain plain sub-coins and materializes
carriers, run from the background watcher every poll by default. The open half: the transfer message
does not convey ancestor backup locktimes, so the receiver cannot compute the true minimum locally.
An **online** receiver is always safe — the branch is locktime-free, so broadcasting it ends the race
— but an owner offline past the *true* deadline has no defence. The margin absorbs an *assumed* span:
`auto_exit_margin_blocks_for(k_max, interval, d)` with **`k_max = 14`**, plus one confirmation window
per sequential transaction of the exit walk, giving 2,120 blocks on mainnet and 860 on regtest. `k_max`
is an assumption; the true fail-closed bound is the chain's own capacity of 100 hops, which no margin
can absorb. If no bound on `k` is justifiable, fall back to eager broadcast. A laddered coin has the
same un-conveyed-`k` problem in its `min(L_k)` root clock, absorbed by
`auto_refresh_margin_blocks` = 144 and by `deadline_safety_due`'s sever fallback.

**The block-space ledger, per payment.** Because essentially every payment is a split, the leaf lane
is the only lane that describes a real user ([PARTIAL-PAYMENT-ECONOMICS.md
§1.3](../spec/PARTIAL-PAYMENT-ECONOMICS.md); ordinary on-chain comparison ~154 vB for a 1-in-2-out
payment):

| leaf lane, per payment | block space | against ~154 vB on chain |
|---|---:|---|
| spent onward off-chain | **0 vB** | this is the product |
| swept and settled | **~105 vB** | **1.47× better — and this is the cap without the discharge round** |
| walked out unilaterally | **250 – 2,719 vB** | **worse than on-chain** |
| **shipped default** | **418 vB** | 2.7× worse |

State both sides or the number is marketing: for the population that actually exists, the shipped
default settles a payment for **more** block space than doing it on chain. Read the swept row with its
own status: the sweep is **design, not built** — `combine_leaves` (`clients/libs/rust/src/combine.rs`)
has no caller outside a test, the absorption predicate exists as no function, and the cooperative
child exit every §3 number rests on is unverified. The sweep is what changes that row, and the
**discharge round** (SPEC §5.4) is what would change it by an order of magnitude — and
the round is **design, not built**: its SE enforcement point is empty, so the coordinator would
presently co-sign a collapse that pays out nobody. The satoshi ledger is a different and larger
quantity: every pre-signed tier permanently burns `committed_fee(3.0) + 240` = **615 sat**, a leaf's
own two tiers burn 1,230, and a combine that spends `SP.out[j]` directly never broadcasts those tiers
at all.

## 6. Real-world situations

### 6.1 Receiver goes offline for N days

**Laddered coin: nothing *triggers*, but the calendar still runs.** There is no CSV maturity to sleep
through, because no CSV clock has started. Two things the offline period does cost:

- **reaction time** — if a past owner triggers the coin while you are away, someone has to run
  `defend_ladders()` (or a delegated tower has to) within the notice window: ≥ 288 blocks (~2 days)
  before *any* hostile transaction is final, and ≥ 36 blocks of head start at each tier after that.
  Since the alarm is a public on-chain event and the bundle is keyless, running two or three
  independent towers reduces this to a liveness question about towers. This is residual **R-2**;
- **`min(L_k)`** — this one no keyless tower covers. A laddered watch entry is exported with
  `deadline_block: u32::MAX`, which disables the height predicate by construction, so a delegate
  watches only the *event* of `F` being spent, while both remedies (cooperative re-anchor, sever by
  broadcasting `T`) need the owner's keys. An offline laddered holder is therefore safe against
  triggers if towers are running, and safe against `min(L_k)` only until it approaches. This is
  TRUST-MODEL **B4**.

**Flat-lane sub-coin or received carrier: the calendar is the whole exposure.** The danger height is
the root deadline (§5), anchored at the *root deposit*, not at the moment of receipt. Offline within
the margin: nothing happens; locktimes hold everyone off. Offline past the *true* deadline: a stale
ancestor backup becomes final and the sender-side holder can claw back the shared root while your
locktime-free branch sits unbroadcast. If you plan to be offline, broadcast the branch first (it costs
only the pre-committed reserves), or leave `auto_exit` on with a wallet that stays alive. One benign
wrinkle: any co-descendant of the same tree can broadcast the *shared* branch txs at any time — that
materializes your funding output without your involvement and only improves your position.
`ExitBranchConflict` means a *different* transaction spending the same input; rebroadcasts of the
identical branch tx are tolerated.

### 6.2 SE goes down permanently — the day it happens, and a year later

The SE's death removes the *cooperative* paths only; every unilateral path is pre-signed and needs
nobody — with one onboarding boundary and one calendar caveat.

**The onboarding window.** `T` is signed once, when the SE's watcher first sees your funding tx
(`check_deposit`, `clients/libs/rust/src/coin_status.rs`), and the same is true of the first flat
backup. Between broadcasting the funding tx and that co-sign you have **no** unilateral path at all: an
SE that dies inside that window strands the deposit in the 2-of-2 permanently. Fund only after deposit
init succeeds, and treat a missing `LadderEstablished` / `DepositConfirmed` as a reason to stop
funding. (TRUST-MODEL **B5**; `sdk16` covers the fresh-user onboarding path.)

**Once the ladder exists**, an SE that never returns costs you latency, never funds — and the latency
does not grow with how long the SE has been dead, because nothing in the tier tree has been ageing.
`unilateral_exit` runs `exit_pass`, which is idempotent and incremental: it broadcasts `T`, reports
`complete: false` with the blocks remaining until the next tier matures, and advances one tier per call
as the chain moves. `Err` from that pass means **blind** — the chain backend could not be read — and is
never reported as a healthy wait (ci-guard `deny_silent_degradation`). Total wait: `E_m + Δ_k`
sequentially, worst 2,160 blocks + confirmations (≈ 15 days) for a never-transferred coin, decreasing
36 blocks per hop and per renewal; `720·d + 2,160 + (3 + 2d)` for a depth-`d` child. `sdk50` drives
the whole walk; `sdk45` drives it from a **keyless** watch bundle, which is the same path a delegated
tower takes.

Note what is *not* available under a dead SE: the cooperative de-trigger, and the cooperative
re-anchor. So a hostile trigger costs you a per-tier race rather than a chosen settlement, and
`min(L_k)` can only be answered by severing — broadcasting `T` yourself and walking out. The correlated
case (dead SE *and* mass grief) is residual **R-1**; it is never confiscation.

**Flat-lane coins** under a dead SE: the branch broadcasts *immediately* (locktime 0), and only the
leaf backup waits its own ladder — the `wait_blocks` figure `unilateral_exit` reports. **Outcome, both
shapes:** funds recovered on-chain; the only variable is how long you wait, never whether you win.

### 6.3 SE compromised or colluding with a previous owner

The trust floor, demonstrated by `sdk15`: a malicious SE can co-sign a *fresh* transaction for a
previous owner, and a fresh signature carries no timelock at all — the ordering machinery, which ranks
only *pre-signed* state, gives no advantage against it. This is **B1**, the statechain trust unit, and
TES-R leaves it byte-identical.

**There is no race advantage to lean on here, and the honest statement is symmetric.** The collusive
spend is un-timelocked; so is the owner's own trigger `T` over the same `F`. The two are conflicting
spends decided by first-seen and fee, between an attacker who is by construction online and a defender
who may not be — and `fee_bump` ships as `None` on both presets, so no wallet bumps anything out of the
box.

What constrains the collusion: the SE alone can do nothing — the coin is a 2-of-2 and the SE never
holds your share; freeze ≠ seize. The *API* refuses to raise a budget (`set_sig_budget` clamps to
`min()`), so un-terminating a node requires the operator to rewrite its own database — the clamp is
application code, not cryptography. What that subversion cannot do is **hide**: anyone holding an
earlier terminal receipt catches the flip, and a fresh co-signature spending a
terminal/single-use/expired node is publicly attributable misbehaviour. Note also that in production
the lockbox and the coordinator are run by the **same operator**, so any argument of the form "the
coordinator cannot do X because the enclave would have to agree" is an argument about software, not
about incentives; and there is **no enclave-residency attestation** — what the enclave key attests is
the *numbers the census rests on*, not that the share lives in an enclave.

One thing genuinely improves. Post-hack theft against a *watched* laddered coin that goes through the
tier machinery requires a public trigger plus ≥ 144 blocks of on-chain notice. Coins received before
the hack and left untouched are unconditionally safe (the hacked SE holds only the post-rotation share,
and the owner's partial is required for every spend path).

### 6.4 Fee spike during a unilateral exit

Every pre-signed transaction has a fee decided at signing time; **there is no RBF** — re-signing would
need the SE. Two answers, one per shape.

**Laddered.** Each tier is nVersion=3 (TRUC) and carries a **committed fee** at 3 sat/vB drawn from
the coin (375 sat on a 125-vB tier), so the base case relays and confirms standalone, plus a **240-sat
P2A anchor** (`OP_1 0x4e73`) so a party holding a funding UTXO can attach a live-rate fee child. TRUC's
1P1C topology plus sibling eviction gives pinning resistance: each tier confirms before the next is
even valid, so there are no long vulnerable chains to pin. `tier_is_relayable` is the predicate that
decides whether a tier can enter a mempool at all, and it deliberately **ignores the anchor** — the
conservative direction, because a tier that cannot be broadcast never enters the race a timelock
argument is about.

The rescue path is built and wired (`build_p2a_fee_child` → `submit_package`, escalated by
`exit_pass_with_bump` / `watch_pass_with_bump`), and its two limits are structural rather than
incidental: a **keyless** tower cannot use it at all, and a funded tower's simultaneous-rescue capacity
is the number of **confirmed** fee UTXOs it holds, not its balance — under TRUC a v3 child may have at
most one unconfirmed ancestor, and the stuck tier is already it, so a second rescue funded from the
first's unconfirmed change is refused at any price (measured in `live_tower_float.rs`;
`tower_float::Solvency` and `plan_float` report in both units and name which one failed). A fee spike
outlasting the 36-block head start converts the CSV edge into a pure fee race — δ is a dial, and
quantifying it against mainnet fee history is open problem **O-2**, not done.

**Flat lane.** Branch txs carry the split's pre-committed reserve, so a large spike can strand them
in the mempool — but stranded is not lost: the transaction stays valid forever, and once the leaf
backup's locktime matures, the backup — which pays to *your own address* — can be spent by a high-fee
child that CPFPs the entire ancestor package onto the chain. The sharp edge is timing: the branch must
land before the earliest hostile ancestor maturity, which is why exiting *early*, not at the deadline,
is the rule.

### 6.5 A previous owner broadcasts stale state

**Laddered — loud, slow, and answerable.** A past owner cannot broadcast a stale state directly; it
spends `X_m.out[0]`, which does not exist on-chain. Their only opening move is `T`, which spends `F`
in public. Four phases:

1. **`T` in the mempool / confirmed.** Every tower watching that one outpoint sees it. Nothing else is
   valid yet. Preferred response: the **de-trigger** — spend `T.out[0]` with no relative timelock,
   unopposed, inside the ≥ 144-block window (`SDK_E2E=89`; after it, the pre-signed extension is
   refused by the node).
2. **`T` + `E_m`.** If the SE is unreachable, the owner's tower broadcasts the newest extension. Every
   extension is a rival spend of the same output, and the newest carries the *lowest* CSV, so it
   matures ≥ 36 blocks before any older one; once it confirms, every older extension's prevout is gone
   and every state hanging on an old epoch can never confirm at all. An old-epoch attacker loses here
   outright (`sdk40` PART 3).
3. **`X` + `Δ_k`.** The owner's current state, carrying the strictly-lowest CSV, matures 36 blocks
   before the newest stale one and settles the coin at the owner's key. `sdk51` runs this end to end
   against a real hostile trigger; `sdk41` proves the payer cannot claw back after paying.
4. **Never a first-seen free-for-all** — provided the defender acted within their head start. If the
   defender sleeps through the whole notice window *and* the head start, it becomes a fee race: the
   accepted **R-2**/**R-4** residual, not a design property.

**Laddered — the other move.** A past owner who waits for `min(L_k)` does not need `T` at all: their
retained flat backup simply becomes valid and spends `F`. Nothing in the tier tree out-races a
transaction that is valid now. That is exactly the attack `deadline_safety_due` exists to beat, and
why it severs rather than reporting a clean pass over an undefended coin.

**Flat lane — three phases.** While `tip < stale locktime`, the network **rejects the transaction as
non-final**: the broadcast achieves nothing. At the honest owner's own maturity, the **exclusive
window** — for `interval` blocks the honest backup is the only final tx, so a broadcast that *confirms*
inside it is uncontestable. After the stale state matures too: a first-seen race a watchtower wins by
detecting the hostile mempool entry and broadcasting immediately; for sub-coins the locktime-0 branch
outruns any locktimed backup at any time before maturity, and a mempool conflict surfaces as
`ExitBranchConflict` rather than a silent failure.

### 6.6 The high-velocity merchant coin

**Laddered.** A merchant coin gets 36 hops per epoch and 16 epochs — **576 whole-coin transfers per
depth level** — and when a level exhausts, `transfer()` rolls it over **off-chain**, unattended, for
zero on-chain bytes. `sdk43` is the standing proof that renewal and rollover are unbounded and free.
Elapsed time contributes nothing *to the hop budget*; it does contribute to `min(L_k)`, and so does
each whole-coin hop, at `interval` blocks apiece. So the honest merchant budget has two lines: hop
count against 576 per level, and **calendar plus 100 blocks per hop against the 10,000-block epoch** —
the second is what forces a re-anchor, and it is the tighter one for a fast coin. A coin hopped 99
times has exhausted its flat chain regardless of how much CSV schedule remains.

What a high-velocity operator should also budget for is **exit weight**: every depth level adds 2
pre-signed txs and 720 blocks to a contingent unilateral exit. Neither is visible to the payer.

**Flat lane.** Remaining life is `initlock − k·interval − (tip − H)`; receive-side validation
hard-rejects any handover whose backup locktime is at or below the tip (`LocktimeTooLow`), and at the
floor the coin must be re-anchored or cooperatively withdrawn.

### 6.7 Long-hold cold storage

A self-deposited, never-transferred **laddered** coin has: no counterparty holding stale state; no
party other than the owner able to start its CSV clock at all (copies of `T` travel only with
ownership history); and the SE alone cannot spend a 2-of-2 it holds one share of. It also sits on
`L_0 = H + 10,000`, its full epoch, with `k = 0` — the furthest a coin's calendar ever is. Such a coin
can sit for most of that epoch untouched, and then needs one cooperative re-anchor (112 vB) or an exit.

A **received** laddered coin is different in two respects: its previous owners hold `T` and stale
states, so it carries the perpetual alarm-driven watching duty of **R-2**; and its epoch has already
been spent down by `k·interval` before you ever saw it. Cold storage means nobody is watching — so for
a received coin, cold storage means delegating the *reactive* half to towers that are watching, and
keeping a wallet alive (or exiting) for the *calendar* half no keyless tower covers.

The remaining reasons a statechain coin is an imperfect vault are operational: cooperative paths depend
on SE liveness (and any epoch deadline, §6.8); pre-signed tier fees are frozen at signing time and
drift against the fee market, with owner-funded bumping the only rescue; and the exit material is not
seed-derivable, so the *real* long-hold risk is losing `wallet.db` and the recovery bundle (§6.9,
TRUST-MODEL **B7**). **Flat-lane** carriers must be materialized before their root deadline
(`sdk32` is the standing record of a received token idled past every horizon; `sdk34` shows the
watchtower doing it).

### 6.8 Epoch-bounded coins (compliance / limited mandate)

An optional per-coin `epoch_deadline` (unix seconds, set at deposit) makes the SE refuse **new**
co-signatures once its clock passes the deadline (410, ERR-2; `RGB_E2E=7`). Unlike round expiry there
is no sweep: unilateral exit never needs the SE, so the pre-signed exit path — the tier chain on a
laddered coin, the branch plus backup on a flat-lane one — lives on. Use it to hard-bound
circulation: a custodial mandate ending on a date, a compliance-scoped instrument, a bounded
delegation. Note the interaction with a laddered coin: past the epoch the SE also refuses renewal,
rollover **and the cooperative re-anchor**, so the coin cannot answer `min(L_k)` cooperatively and must
eventually be severed or exited rather than kept off-chain indefinitely. Nobody, including the SE, can
confiscate it.

### 6.9 Receiving as a fresh user with zero on-chain footprint

`sdk16`: a brand-new wallet with no UTXOs, no deposits and no chain history receives off-chain and is a
first-class owner. Its exit material is entirely local — for a laddered coin the tier bundle
(`tesr-<id>` for a whole coin, `ctesr-<id>` for a received split child) with its trigger, extension,
current state and per-tier CSV schedule; for a flat-lane coin the pre-signed backup chain plus the
branch rows (`branch-<id>`, root-first) and the ancestor list (`parents-<id>`). Either bundle is a
complete, SE-independent exit containing no key material, which is what makes it delegable (`sdk45`).

The corollary applies to **every** coin shape: **a mnemonic alone does not restore a wallet.** The seed
rebuilds the key hierarchy but not the per-coin exit material — statechain ids, the tier chain, the
backup chain, the `branch-*`/`parents-*` rows — which lives only in the wallet database and which the
blind SE cannot re-serve after a claim (TRUST-MODEL **B7**). `export_recovery_bundle` snapshots all of
it; `sdk73` covers structural recovery. Token wallets additionally need the entire `rgb_data_dir`
(including its own plaintext RGB seed), which the recovery bundle deliberately does not embed.

### 6.10 The griefing cases

**A hostile trigger.** Anyone holding a copy of `T` — i.e. any past owner — can broadcast it to force
the victim a cost. The response is the de-trigger: 125 vB, one confirmation, the value landing where
the **owner** says. The attacker pays **nothing** at or below the committed rate, and the coin loses
~1,230 sats of fees and anchors; the damage is bounded and fee-attributable on-chain even though `T`
itself is anonymous. The residual is *saturation*: ~1M simultaneous triggers demand ~125M vB of
responses inside a ~144-block grace ≈ **87%** of a day's block space — strained but survivable; beyond
that the response degrades to a prioritized fee auction on the highest-value coins. If the SE is
*simultaneously* dead the de-trigger is unavailable and every coin fights the per-tier races of §6.5.
That correlated scenario is residual **R-1**, and the mass-grief prioritization policy is open work
(**O-6**), not shipped code.

**Voiding a sub-economic piece.** A prior owner of an *ancestor* can void a piece too small to be worth
walking out, with one 112-vB backup, at zero marginal cost per extra piece — the transactions are
already signed and no operator can stop it. That is the economic reason `min_child_value` exists and
the reason a piece received and immediately cashed out should never have been an off-chain split.

**Irreversible-endpoint replay.** The two irreversible owner endpoints — `POST /statechain/spend_budget`
and `POST /withdraw/complete` — demand a single-use, endpoint-bound challenge: a 5-minute SE nonce
(`GET /auth/challenge/<sid>`) signed as `sha256(nonce ‖ endpoint)` and atomically consumed
(`validate_signature_nonce`, `server/src/endpoints/utils.rs`); `POST /deposit/get_derived_token` and
the transfer-sender recipient leg use the same rail. `/sign/first` and `/sign/second` deliberately keep
the static `signed_statechain_id` auth, with harm bounded by the coin protocol, the pending-transfer
lock and the enclave's secnonce consume — adding the nonce there is the one renewal-side rail still
worth building.

**Outcome, all cases:** griefing costs the victim fees, inconvenience, and sometimes off-chain-ness. It
cannot take funds.

## 7. The UX perspective

**What the wallet surfaces.** `estimate_exit_cost(coin)` returns an `ExitCostEstimate`
(`branch_txs`, `branch_vbytes`, `backup_vbytes`, `total_vbytes`, `wait_blocks`, `exit_deadline_block`,
`exit_deadline_blind`) — and `exit_deadline_block: None` means "safe" only when `exit_deadline_blind`
is also `None`; otherwise it means "I could not tell", which `deadline_is_unknown()` names.
`unilateral_exit` returns per-coin `ExitStatus{complete, wait_blocks}` and is idempotently re-callable:
on a laddered coin it advances the tier chain one maturity at a time, so "call it again next block" is
the whole protocol. Events:

| Event | Means |
|---|---|
| `DepositConfirmed` | funding reached the confirmation target |
| `LadderEstablished` | a fresh confirmed deposit was laddered by `claim()` — it is now transferable |
| `LadderSkipped{reason}` | `claim()` could not ladder this coin; `LadderSkipReason` names why (`RgbCarrier`, `FundingNotOnChain`, `AttestationIdentityUnpinned`, `BindingUnresolved`, …). It is retried on the next pass |
| `TransferClaimed` / `TokenTransferClaimed` / `TransferCancelled` / `BalanceUpdate` | ordinary bookkeeping |
| `LadderDefended{tiers_broadcast}` | someone triggered a coin of yours and your pass raced its tiers — watch it through |
| `ExitBranchConflict` | a *different* tx is spending your branch root: someone is racing your exit. Bump/alert; do not assume the exit landed |
| `ExitDeadlineApproaching` | a coin is inside its margin — either `auto_exit_due` broadcast its branch, or `deadline_safety_due` could not defend it |
| `TokenCarrierMaterialized` / `LeafExitForced` | a carrier was materialized, or a leaf force-exited, before its deadline |
| `WatchtowerBlind{pass, detail}` | a defence pass could not read what it needed. **Not** "nothing was due" |
| `CoinRefreshed` | a coin was re-anchored — **re-export your recovery bundle** (new statechain id, new exit material) |

Deposit-time cost surfaces as `SdkError::TokenPaymentRequired{token_id, deposit_address, fee_sats}`.

**What a user must do, and when.**

| Trigger | Deadline | Action |
|---|---|---|
| Holding a laddered coin, nothing happening | none for the tiers | nothing. The exit chain does not age |
| Holding a **received** laddered coin as its epoch runs down | inside `auto_refresh_margin_blocks` = 144 of `min(L_k)` | keep a wallet alive so `deadline_safety_due` runs, or re-anchor / exit deliberately. No keyless tower covers this |
| `LadderDefended` fires, or you see `F` spent | within the tier head starts (≥ 288 blocks total notice, 36 per tier) | let `defend_ladders()` keep running; if the SE is up, take the de-trigger instead of racing |
| Holding a flat-lane sub-coin or a received carrier, going offline | before `exit_deadline_block` minus an assumed `k_max·interval` margin | broadcast the branch first, or leave `auto_exit` on / delegate a tower running `auto_exit_due` |
| `ExitDeadlineApproaching` / `TokenCarrierMaterialized` / `LeafExitForced` | within your margin | the action is already taken — confirm it lands |
| `ExitBranchConflict` | immediately | bump/alert/re-attempt; treat the exit as contested |
| `WatchtowerBlind` | immediately | fix the backend. A blind pass is not a quiet one |
| A flat-lane coin nearing its floor | before it floors out | `refresh` (user-pays) or `refresh_sponsored` (operator rebates off-chain) |
| SE unreachable and you want out | none — any time | `unilateral_exit`, re-call each block until `complete` |
| `CoinRefreshed` (any cause) | promptly | re-export the recovery bundle |

**Re-anchoring, in one line.** `refresh(coin)` spends the coin's outpoint into a fresh aggregate with
one SE-co-signed 112-vB transaction, and the coin comes back with a brand-new funding outpoint, a
brand-new ladder, and `k = 0` prior owners. On the **flat lane** that is the lifetime reset:
the old outpoint is spent, so every old backup dies. On the **laddered** shape it is the answer to
`min(L_k)` and the optional solo compaction that returns a deep coin to depth 0. Either way it is
cooperative — if the SE is gone, sever and exit instead.

Two fee models: **user-pays** (`refresh` — coin = amount − fee) or **operator-pays**
(`refresh_sponsored` — a funded operator rebates the fee off-chain). Why a rebate rather than the
operator adding an input? Because the re-anchor is **single-input by construction**: the blind SE
co-signs exactly one input and holds no funds or chain view, so nobody can co-fund the transaction. A
sponsor paying from a laddered coin rebates via an in-ladder split, whose child must clear
`min_child_value`, so the rebate is `max(fee_sats + DUST_LIMIT, min_child_value)` and the operator
absorbs the round-up, leaving the user ≥ whole. `sdk30` is the happy path (both fee models); `sdk38`
pins what a *broke* sponsor does — it errors cleanly and the user keeps the refreshed coin.

**Auto-refresh, honestly.** `SdkConfig::auto_refresh` is on by default and keys off a coin's absolute
backup locktime: `auto_refresh_due(margin)` re-anchors any confirmed non-carrier coin whose headroom
has fallen to `auto_refresh_margin_blocks` (144 ≈ 1 day), and `transfer` runs it as a pre-spend hook so
an aging coin never becomes un-transferable mid-payment. Three things to know. First, routine
**background** re-anchoring is default-**off**, so a running wallet never silently shrinks a balance —
but that flag governs maintenance only; `deadline_safety_due` runs regardless. Second, when the
pre-spend hook catches an aging coin, the transfer **waits** for the re-anchor to confirm — a bounded
poll, after which it returns an explicit retry-shortly error rather than hanging. Third, carriers are
excluded from the *cooperative* route (a plain re-anchor destroys the allocation) and included in the
*unilateral* one — and there they reach only a **coloured** ladder. Where an attestation identity is
pinned that is the ordinary case, and the carrier is defended like any other laddered coin; where
none is, every carrier is refused and **a branch-free carrier on the flat lane has no automatic
defence of `min(L_k)` at all**. That is visible rather than silent: the pass reports every coin it
could not defend and returns `Err` rather than a clean `Ok` (ci-guard
`deny_uncovered_carrier_deadline`). A coin too small to cover the re-anchor fee above dust is
deliberately **skipped**, not failed — rescue it by combining.

**What a watchtower must watch** (per coin):

1. **Laddered:** the funding outpoint `F`. A spend of it is the alarm — run `watch_pass` per block from
   there, broadcasting each tier as its CSV matures. Idle coins need no polling beyond the
   subscription, and multiple towers compose idempotently (`sdk45`). A keyless tower must **not** try
   to be package-aware (it has no funding input, so its package would be 1-parent-0-child); a funded
   tower **must** be. Neither covers `min(L_k)`.
2. **Flat lane:** (a) any mempool/chain spend of the funding outpoint that is not the owner's own tx
   → race immediately with the branch (sub-coin) or matured backup (flat); (b) tip vs the owner's
   backup locktime → broadcast at maturity, inside the exclusive window; (c) tip vs
   `exit_deadline_block` minus the assumed margin → force-broadcast the branch, which is exactly
   `auto_exit_due(margin_blocks)`; (d) persistence of already-broadcast low-fee exit txs → rebroadcast
   if purged, CPFP once the backup is spendable. An offline-capable tower must cache `initlock` and the
   root deposit height rather than deriving the deadline from a leaf's locktime.

Both bundle kinds are **snapshots**: re-export after any operation that mints or replaces coins.

**Time-to-money, per flow:**

| Flow | On-chain txs | Wait |
|---|---|---|
| Deposit → spendable | 1 (your funding tx) | confirmation target + SE registration + `claim()` laddering |
| Off-chain receive (whole coin or split child) | 0 | seconds (API round-trips + validation queries) |
| Off-chain onward send of a received child | 0 | seconds — `sdk60` does two hops with the funding outpoint never spent |
| Cooperative exit | 1 per coin (~111 vB) | ~1 conf |
| Cooperative re-anchor | 1 (112 vB) | ~1 conf |
| De-trigger after a hostile trigger | 1 (125 vB) | ~1 conf, no CSV wait |
| Unilateral exit, laddered flat coin | 3 (T+X+S = 375 vB) + 0–3 fee children in a spike | `E_m + Δ_k` sequential — 2,160 blocks + confirmations ≈ 15 d fresh, −36 per hop |
| Unilateral exit, laddered depth-`d` child | `3 + 2d` (`293·d + 375` vB) | `720·d + 2,160 + (3 + 2d)` blocks — depth-1 ≈ 20 d, the mainnet cap of depth 8 ≈ 55 d |
| Unilateral exit, flat-lane sub-coin | N branch txs + 1 backup | branch: now; backup: its own fresh chain |
| Token materialization | branch only (2d+1 txs) | now — the allocation settles on the resting output |

The latency line deserves emphasis, because it is the real price of relative timelocks: **a unilateral
exit is slow, and it gets slower with depth.** Cooperative exit, when the SE is alive, is one
transaction and one confirmation at any depth — and that is the path essentially every user takes. The
unilateral chain is the guarantee that makes the cooperative path safe to prefer, not the path itself.

**Sharp edges, honestly.**

- **No unconditional no-watch window** on the ladder (**R-2**): a received coin must be watched —
  keyless and delegable for the reactive half, alarm-driven with ≥ 1 day of notice.
- **`min(L_k)` needs the owner's keys** (**B4**): the reactive half delegates, the calendar half does
  not, and a branch-free carrier on the flat lane has no automatic defence of it at all.
- **The conveyance window is a wall clock, not ownership** (§3, `sdk91`); REQ-61's owner latch is
  **design, not built**.
- **The discharge round is design, not built** (SPEC §5.4) — and it is what the block-space economics
  of §5 need to improve by an order of magnitude.
- **Spike-time bumping is owner-funded and node-gated** (**R-4**): the code is wired, but no suite test
  exercises it and a keyless tower structurally cannot.
- **Deep-DAG unilateral latency** (**R-5**): the mainnet cap of depth 8 is ~55 days; the geometric
  schedule that would shorten it is default off (**O-4**).
- **The census's trust premises** (**O-1**): `se_num_sigs` is earned by the pinned-identity
  attestation (**P3**), but the sid ↔ aggregate-key binding (**P1**) is still coordinator-supplied and
  unattested, and a malicious enclave can attest anything (**B11/CO-1**).
- **Un-conveyed ancestor locktimes** (**B6**) — the `k_max = 14` assumption absorbs a span nobody has
  measured — plus no RBF on any pre-signed tx, and ancestor-id substitution (**B2**) on the branch lane.
- **Recovery bundle is not seed-derivable** for any coin (**B7**); token wallets also need
  `rgb_data_dir`.

## 8. FAQ

**Does an idle coin have a deadline?** Its *tiers* do not — that is INV-27, and it is real: nothing in
the tier tree matures until someone broadcasts `T`, so an idle coin's exit chain is byte-identical
after any amount of waiting and costs 0 vB of rent. But the *coin* does: it retains a flat backup chain
whose lowest absolute locktime `min(L_k)` is `initlock` blocks from the deposit (10,000 on mainnet,
≈ 69 days) minus `interval` for every whole-coin hop it has taken. Anyone quoting "no deadline" without
that second sentence is describing half the machinery. `sdk86` measures both clocks on one coin.

**Do I lose anything if I do nothing for a year?** On mainnet a year is longer than a coin's epoch, so
yes — something must happen inside it: one cooperative re-anchor (112 vB), an exit, or the sever
`deadline_safety_due` performs on your behalf if your wallet is running. Nothing is *forfeited* by
timeout — no output ever pays the operator — but a coin whose `min(L_k)` passes unanswered can be taken
by an ancestor's matured rung. The reactive duty is separate and smaller: if a past owner triggers your
coin, you or a tower must respond inside the notice window.

**Can the SE steal my coin?** Not alone: the coin is a 2-of-2 and the SE never holds your key share.
Colluding with a *previous owner* it can fresh-sign a competing spend and force a symmetric first-seen
race against your own un-timelocked trigger (§6.3, **B1**) — the trust floor shared with every
statechain design. It cannot forge terminal state through the API (monotonic), and misbehaviour on
structural nodes is publicly queryable.

**Can the SE freeze my funds?** It can refuse to co-sign (or die, or be legally compelled), which kills
the *cooperative* paths only: transfers, renewal, rollover, cooperative withdraw, de-trigger, and
re-anchor. Unilateral exit is pre-signed and SE-independent; worst case is the tier wait. The sharp
edge is that losing the re-anchor also loses the cheap answer to `min(L_k)`, so a frozen coin must be
severed and exited before its epoch runs out. The one boundary case is the onboarding window — the
guarantee begins when the SE co-signs the trigger at deposit detection (§6.2).

**What if I lose my wallet database?** That is loss of funds, and **no** statechain coin restores from
the mnemonic alone (**B7**). Back up with `export_recovery_bundle` and re-export after every transfer,
claim, split, child re-transfer, **or refresh**. Token wallets must additionally copy the whole
`rgb_data_dir`.

**Can two people be handed the same coin?** The enclave consumes each secnonce atomically (one
signature per nonce — **INV-23**, `sdk12`), the coordinator refuses every co-sign while a transfer of
that coin is open, and refuses to re-address an open transfer to a different recipient. A *malicious*
SE could still try; what a second "owner" cannot satisfy without the SE visibly double-signing is the
receiver's census — `se_num_sigs` must equal `flat_backups + tiers + superseded` against an
enclave-attested count, so a hidden extra co-signed state shows up as a mismatch (`sdk46`, `sdk54`,
`sdk58`, `sdk60`).

**Why does the split spend the extension rather than the funding output?** Because a split that spent
`F` would be a *rival* of the trigger, and a past owner's retained `T` — which has no timelock at all —
would win that race, voiding the payee's coin while the ladder paid the splitter the full parent value.
An in-ladder split is a state tier `SP` spending `X_m.out[0]`: a **descendant** of `T`, never a rival
for `F`, so a retained trigger has nothing to race (§4b; `sdk58`, `sdk59`).

**Why is the split state's timelock zero?** Because over the outpoint `SP` spends, its only rival is
the state it replaces, whose CSV is ≥ `d_floor` — so zero is replace-by-lower-timelock at its extreme,
winning by the whole schedule. The hazard that makes an un-timelocked tier dangerous (`T` over `F`)
does not arise, because `SP` is signed by the sole current owner of the outpoint it is simultaneously
giving up: the voiding party and the victim are the same entity. The payoff is that a split consumes no
state rung, so a coin can be partially paid from as often as it likes, and `SP` contributes one block
to the exit walk rather than a full CSV.

**Is a received partial payment a real coin, or an exit-only claim?** A real coin. The claim completes
the standard SE key handover: `A_child` is invariant across the share rotation — precisely what keeps
the child's pre-signed exit chain valid — and the sender's auth is rotated out, so the sender is
permanently locked out. The child can be paid onward whole (`child_retransfer`, zero sats, zero added
depth) or split again, one co-signature and one disclosed superseded state per hop, counted by the next
receiver's N-hop census (`sdk60`, `sdk17`).

**Why relative locks, when Spark also uses them?** Both use relative CSV, but the invalidation
*authority* differs. Spark's old state dies by **operator key deletion** — an honest-1-of-n trust
assumption — and its leaves need renewal churn with the operator group. Here, old state dies at the
**consensus** level: a renewal mints an extension that strictly undercuts every older one in the race
for `T.out[0]`, so every pre-renewal state hangs on a parent that can never confirm, and the receiver's
exact-equality census is a second, independent layer on top. Renewal and rollover are fully off-chain
and unbounded (`sdk43`), and amounts are exact rather than denominated. Two honest caveats:
"consensus-dead" is *race-conditional*, and the flat chain's calendar is a maintenance duty Spark's
leaves answer differently.

**Why absolute locktimes on the flat lane, then?** Because that shape exists precisely where a
relative-CSV tier chain cannot go: an RGB carrier must never be given a *plain* ladder (a plain tier
spend destroys the allocation), and the *coloured* one that would carry it needs a pinned enclave
identity to establish, which a network with no provisioned enclave does not have. The signed-once
absolute backup is what such a coin can carry. Note what is **not** on that list any more: "a
sub-coin over un-broadcast funding". That coin still has no confirmed prevout for a trigger to spend
(a v3 tier cannot relay over an unconfirmed v2 parent) — **B0** is permanent — but it is no longer
pushed onto the absolute-locktime shape for it. Its ladder hangs off `SP.out[j]` instead, which is
what makes an in-ladder child and a spine tip laddered coins with un-broadcast funding rather than a
contradiction in terms.

**Why locktime 0 on flat-lane split transactions?** Because the branch must beat every
deposit-anchored stale backup unconditionally. Any nonzero branch locktime can end up *above* an aged
parent's backup, letting the stale state mature first and win. A height-0 branch is broadcastable now
and sits below every backup by construction (**INV-4**); receivers reject any branch tx with locktime
above the tip.

**What does "terminal" mean — can it be undone?** Terminal = the SE will never co-sign this statechain
again (`finalized ≥ sig_budget`). `set_sig_budget` writes `min(count_finalized + remaining, existing)`,
so no request *via the API* raises it. An operator subverting its own database could, but the flip
contradicts every terminal receipt the public endpoint served before — and on the ladder lane
terminality is read from the **enclave-signed** payload rather than the coordinator's answer, with the
coordinator's kept only as a cross-check that refuses on disagreement. Terminalized today: the **node
being split** (parent or child), the ancestry of any RGB anchor, and the **Lightning-latched piece** —
but *not* an ordinary conveyed piece.

**Who pays exit fees?** On the ladder, every tier carries a committed fee (3 sat/vB — 375 sat on a
125-vB tier) drawn from the coin at signing time, plus a 240-sat P2A anchor that lets a party holding a
funding UTXO top it up at live rates. That party is the **owner** (or an operator's funded tower), never
a keyless one. On the flat lane the *splitter* pre-pays each branch tx's fee at split time and
the *exiter* pays the backup's fixed pre-signed fee plus any CPFP top-up. Cooperative exits pay normal
fees at live rates.

**What if the mempool purges my pre-signed tx?** Nothing is lost: pre-signed transactions never expire
and rebroadcast is free. `exit_pass` and `watch_pass` are idempotent and incremental, so the next call
re-broadcasts what is missing.

**Is there any scenario where an honest, online user loses funds?** Within the model, one: the SE
colludes with a past owner and wins the first-seen race against the online user (§6.3) — and the
default wallet does not bump, so "wins the race" is not merely theoretical. Plus one boundary case with
no adversary at all: an SE that dies before co-signing the trigger strands the deposit in the 2-of-2.
Every other adversary — stale broadcasters, triggers, griefers, dead SEs, fee spikes — loses to an
online user mechanically. Offline is where the qualifiers pile up (§6.1, §6.7).

**What happens when the hop budget runs out?** Nothing visible. At 36 state decrements the SDK renews
the extension off-chain inside the next `transfer()`; at `m = 15` it rolls over to a fresh level, also
off-chain; past the depth policy it folds one 112-vB re-anchor into that transfer's fee. `sdk43` runs
the whole sequence unattended. The *flat* chain's 99 usable hops are the budget that does run out
visibly: the receiver's `lock_time <= tip` rule refuses the hop that would breach it
(`LocktimeTooLow`), and the answer is a re-anchor.

**Does splitting extend my coin's life?** No. An in-ladder split gives each child its own extension and
state tiers, costs the split node its terminality, and gives the child **no flat backup of its own** —
a child inherits its parent's `min(L_k)`, which is exactly what the exit-headroom gate measures
against. On the flat lane the *leaf's* chain resets (fresh `initlock` at split height) but the
*tree's* root deadline never moves; only materialization or a re-anchor resets the wall clock.

**Can a previous owner do anything at all before broadcasting the trigger?** Two things. Their states
and extensions spend outputs that do not exist on-chain, and the SE will not co-sign for them — key
rotation plus the secnonce consume plus the pending-transfer lock mean the SE answers the current
owner. So their *tier-tree* move begins with `T`, which is public. But they also hold a flat backup at
`L_j > min(L_k)`, and that one needs no trigger: it simply becomes valid when its height arrives.

**What if my watchtower dies?** You inherit its duties on your next wake: check that no funding
outpoint was spent hostilely (and if one was, run `defend_ladders()` immediately), and check both the
laddered `min(L_k)` margin and the deadline on any flat-lane sub-coin or carrier. Exposure is limited
to coins triggered during the outage, or whose deadline passed during it. Towers are keyless and
idempotent, so run more than one — but remember they never covered `min(L_k)`.

## 9. Comparison recap: over-time behaviour

| | **Ours (TES-R, laddered)** | **Ours (flat lane)** | **Spark** | **Ark / Second** | **Absolute ladder** |
|---|---|---|---|---|---|
| Exit-chain ageing | **None** — relative CSV on un-broadcast txs; the tier tree never ages | Absolute locktime from deposit | Relative ladder, decrementing per hop; unbounded *if* renewed | Round expiry (~weeks), hard | Absolute, one horizon |
| Calendar deadline | `min(L_k)` over the retained flat chain — `initlock` = 10,000 blocks (~69 d) minus `interval` = 100 per hop | Root deadline `H_deposit + initlock`, minimum over every ancestor | Operator-renewed | Round expiry | Same as the exit chain |
| Idle on-chain footprint | **0 vB/yr** of tier rent; one ~112-vB re-anchor per epoch | 0 vB/yr, but a received coin owes one materialization before its root deadline | 0 | Refresh per round or lose funds | 5,840 vB/coin-yr |
| Renewal | **Off-chain and unbounded** — lower-CSV extension re-sign, then off-chain self-split rollover (`sdk43`); on-chain only for the epoch re-anchor and the optional depth compaction | On-chain re-anchor (1 tx), or self-split (leaf only) / materialization | Operator-group churn | Mandatory per-round refresh participation | On-chain re-anchor per coin per horizon |
| What invalidates old state | **Consensus** — lower CSV wins the trigger output, old epochs' parents unconfirmable — plus SE refusal and the receiver's attested census | Consensus (absolute locktime ordering) + SE refusal | Operator honest key deletion (1-of-n trust) | Round expiry | Absolute locktime ordering |
| Operator dies | All exits pre-signed; wait `E_m + Δ_k` (≈ 15 d flat, ~55 d at the depth cap), funds whole; `min(L_k)` answered by severing | Branch broadcasts now; leaf backup waits its chain | Unilateral path exists; timelock race | Exit window critical; miss it → server sweeps | Wait ≤ `initlock` |
| Stale state over time | Inert until a **public** trigger, then ≥ 288 blocks of notice and a ≥ 36-block head start per tier — **or** simply valid once `min(L_k)` passes | Non-final → exclusive window → watched race; branches beat ancestors before the root deadline | Timelock race; key-deletion honest-1-of-n | Dies at expiry (the same knife that threatens users) | Timelock race |
| Missed-liveness outcome | Raceable after a public trigger + ≥ 1 day notice; losable to an ancestor after `min(L_k)`; **never confiscated by design** | Raceable after the deadline; never confiscated | Safe if renewed; trust-dependent | **Confiscation** — funds sweep to the server | Raceable after maturity |
| Operator misbehaviour visibility | Terminal state enclave-attested and publicly queryable per node | Terminal state coordinator-reported per node | Not queryable per node | Round tree is public | None |
| Offline requirement | **Alarm-driven for triggers** (keyless, delegable to N towers) **plus a calendar for `min(L_k)`** (owner keys required) | Online (or watched) before the root deadline | Online for renewals, forever | Online every round, forever | Online after maturity |

Further reading: the ladder in [PROTOCOL.md](../spec/PROTOCOL.md) and
[CHILDREN.md](../spec/CHILDREN.md); Lightning over the ladder in
[LIGHTNING.md](../spec/LIGHTNING.md); what a payment costs in
[PARTIAL-PAYMENT-ECONOMICS.md](../spec/PARTIAL-PAYMENT-ECONOMICS.md); the normative requirements in
[SPEC.md](../spec/SPEC.md) (REQ-33/34/35/36/38/61, INV-4/5/19/20/23/25/27/28, ERR-1/2/3/7); the trust
map in [TRUST-MODEL.md](../spec/TRUST-MODEL.md) (B1–B11); the short comparison in
[invalidation.md](invalidation.md); partial amounts in
[granularity-deep-dive.md](granularity-deep-dive.md); exit mechanics in [exits.md](exits.md).

Test evidence cited on this page: `sdk04`, `sdk12`, `sdk15`, `sdk16`, `sdk17`, `sdk30`, `sdk32`,
`sdk34`, `sdk38`, `sdk39`, `sdk40`, `sdk41`, `sdk42`, `sdk43`, `sdk44`, `sdk45`, `sdk46`, `sdk47`,
`sdk50`, `sdk51`, `sdk52`, `sdk54`, `sdk55`, `sdk58`, `sdk59`, `sdk60`, `sdk70`, `sdk73`, `sdk76`,
`sdk77`, `sdk78`, `sdk79`, `sdk80`, `sdk81`, `sdk82`, `sdk84`, `sdk86`, `sdk87`, `sdk88`, `sdk89`,
`sdk90`, `sdk91`, plus `RGB_E2E=7` (epoch deadline) and the concurrency chaos test `chaos22`
(`clients/tests/rust/src/chaos22_concurrent_users.rs`, oracle in `chaos22_oracle.rs`). The
node-gated fee-bump tests
(`live_p2a_package_rescue.rs`, `live_tower_float.rs`) skip without a Bitcoin Core RPC endpoint.
