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

**A laddered coin has one clock, and it is stopped.** The *relative* CSV tiers of the TES-R ladder do
not tick while un-broadcast, so an idle coin's exit chain never ages and costs 0 vB of rent — and
there is nothing else on the coin that could tick. No coin carries an absolute-locktime backup
transaction: none is co-signed at deposit, none at any hop, `coin.locktime` is `None` for life, and no
previous owner holds a matured spend of `F`. "Idle coins never age" (**INV-27**) is therefore a
statement about the coin, unconditionally — the flat backup chain it used to be qualified by is
RETIRED 2026-09-06 (INV-5 with it), and so is the calendar it carried.

Everything in this page follows from that one fact. Where a paragraph below describes what a prior
owner *cannot* do, the reason is always the same: the only spends of `F` they ever held are copies
of the un-timelocked trigger, which the current owner can always broadcast first, and superseded
states, which lose the CSV race.

> **Test evidence on this page.** The rule landed together with re-derivations of every flow whose
> subject was the flat lane, and those re-derivations have **not yet been run** against the regtest
> stack. Where this page cites `sdk12`, `sdk15`, `sdk17`, `sdk30`, `sdk32`, `sdk34`, `sdk39`,
> `sdk40`, `sdk41`, `sdk42`, `sdk43`, `sdk44`, `sdk45`, `sdk46`, `sdk47`, `sdk48`, `sdk50`, `sdk54`,
> `sdk55`, `sdk58`, `sdk59`, `sdk60`, `sdk70`, `sdk71`, `sdk72`, `sdk74`, `sdk76`, `sdk77`, `sdk79`,
> `sdk80`, `sdk82`, `sdk84`, `sdk86`, `sdk87`, `sdk88`, `tb05`, `chaos22` or `RGB_E2E=7`, read
> "re-derived, pending run". `sdk04`, `sdk16`, `sdk38`, `sdk51`, `sdk75`, `sdk81`, `sdk83`, `sdk89`,
> `sdk90`, `sdk91` and `sdk94` were **not** touched by the rule. `sdk73`, `sdk78` and the branch-lane
> `RGB_E2E=1–3, 5, 6, 8–10` are **deleted** — their files are gone and their dispatch arms removed
> from `main.rs`, so they cannot be cited at all.
> [build/testing-guide.md](../build/testing-guide.md) carries the flow-by-flow list, and
> `git diff -- clients/tests/rust/src/` is the authority on what has been re-derived so far.

## One protocol, one exit material

There is exactly **one protocol**. A coin's TES-R ladder — trigger `T` → extension `X_m` → state
`S_k`, all **relative-CSV** and all **un-broadcast** — is established at the **first mempool
sighting** of its funding transaction, before confirmation, and it is the coin's only exit material.
`coin_status::check_deposit` does it itself under `LadderAtSight::Plain`; an SDK wallet's watcher
runs under `LadderAtSight::Defer` and `claim()`'s establish pass ladders every un-laddered
`IN_MEMPOOL` / `UNCONFIRMED` / `CONFIRMED` root coin in the same pass — plain, or **coloured** for
a carrier whose allocation is booked. There is no per-deposit protocol switch, no escape hatch, and
no second shape:

- **LADDERED** — every deposit, plain or carrier. Old state is outranked by **relative** timelock
  ordering (each whole-coin transfer co-signs a state one δ lower), backed by the receiver's
  disclosure census and a keyless watchtower. The coin never ages and costs **0 vB** of on-chain
  rent; what bounds its off-chain life is its renewal/rollover capacity, and the only on-chain
  cadence is the cooperative re-anchor at that cap. A carrier's ladder is *coloured* — every tier a
  real RGB state transition — and a ladder in every other respect (`sdk74`, `sdk75`, `sdk77`).
- **NO LADDER** — not a lane but a fault. A deposit whose ladder cannot be established at first
  sight is not booked *on the `Plain` lane* (it stays `INITIALISED`) and is retried; on the SDK's
  `Defer` lane the coin is already booked `IN_MEMPOOL` when the establish pass runs, so a failure
  leaves a booked coin with no exit material, recorded as `LadderSkipped{reason}` (§6.2). A carrier
  that cannot be coloured —
  below the coloured floor, RGB state unavailable, no enclave identity pinned — has **no exit
  material** until a later pass colours it (`LadderSkipReason::RgbCarrier`) and cannot be conveyed:
  a *plain* tier spend would destroy the allocation (terminal-freeze,
  [PROTOCOL.md §5.10](../spec/PROTOCOL.md)), and there is no flat backup to fall back on. A plain
  ladder found over a carrier is recorded `PlainLadderOverCarrier`, which has **no remedy**:
  `colored_reanchor` refuses a plain ladder by name and a plain `refresh` would burn the allocation,
  so the coin exits as satoshis and the tokens are stranded. The flat conveyance lane's licence classifier is **deleted**, not tightened —
  `assert_flat_conveyance_is_legitimate` and `PermanentLicence` survive only inside comments — and
  what stands in its place is `is_legitimate_flat_reason`, which returns `false` unconditionally, so
  a `ladderskip-` record is diagnostic and licenses nothing. The off-chain branch split/combine
  (`register_split_subcoins_n`, `register_combine_subcoins`) and the flat exit fallback in
  `unilateral_exit` **refuse by name**.

`SdkConfig::colored_ladder` (`clients/libs/rust-sdk/src/config.rs`) decides whether a carrier gets a
coloured ladder or no ladder, and it does not state a bool: both constructors READ the compiled-in
pin, `TesrParams::attestation_identity_const` (`lib/src/tesr.rs`). Regtest pins the repo's own dev
enclave, so the flag is **true** and a carrier is laddered like any other coin — coloured, wired
through `build_colored_ladder_auto` / `cosign_colored_ladder` (`clients/libs/rust/src/tesr.rs`), the
coloured in-ladder split, `colored_reanchor` and an RGB-aware `defend_ladders`. Mainnet's const
returns `None` because no mainnet enclave is provisioned, so the flag is **false** there — a
statement about what exists to attest, not a verdict (**V-6**; TRUST-MODEL **B11** is why the
coordinator's own answer cannot stand in for a pin). Pin a mainnet identity and the flag flips with
nothing else changing.

**The same pin gates the SDK's establish pass for *every* deposit, plain ones included — say this
one precisely or it comes out backwards.** `tesr::establish_auto` / `cosign_tier` do **not** call
`get_statechain_info`, so `coin_status::check_deposit`'s own `LadderAtSight::Plain` lane ladders a
plain deposit on an unpinned network. The SDK `claim()` establish pass **does** call it — it needs
the coordinator's aggregate to bind against — and where `attestation_identity` can resolve neither a
compiled-in pin nor a configured value, that call fails, the pass records
`LadderSkipReason::AttestationIdentityUnpinned`, and it ladders **nothing**. The consequence, for
the only lane a wallet user takes: on mainnet, testnet or signet an SDK deposit is booked and has no
exit material — **it cannot be conveyed and it cannot be unilaterally exited**, and cooperative
withdrawal is the only route out. The flat backup used to supply that unilateral exit with no
attestation at all; it is gone (TRUST-MODEL **B12**). Mainnet has no enclave provisioned, so this is
a not-yet-deployable state rather than a live regression — but "deposits and exits work without a
pin, only receiving does not" is false, and this page said something close to it.

**What did NOT change is the un-broadcast half, and it is permanent.** A **split sub-coin** whose
funding output is still un-broadcast cannot root a trigger (**B0** — the trigger would have no
prevout to spend, and a v3 tier cannot relay over an unconfirmed parent), which is why the deposit
pass is root-only. Colouring a tier does not broadcast a funding output, so this holds for a
coloured child too: every in-ladder split **child** and every spine-tip **change leg** is funded by
an un-broadcast `SP.out[j]`, and that is the point of the design rather than a gap in it — it is
where the 0 vB of idle rent comes from. Such a coin is not "un-laddered": its ladder simply hangs
off `SP` instead of off `F`, and its chain reaches back to the parent's on-chain `F`.

Vocabulary: a coin's *funding output* is on-chain (a root) or an un-broadcast `SP.out[j]` (a
**child** or a spine **tip**); a **leaf** is a child at the edge of the tree. The words "flat lane",
"flat coin", "epoch" and "branch" described the retired shape and appear below only in dated notes
about what is gone.

Contents: [the problem](#1-the-problem-from-first-principles)
· [one clock](#2-one-clock-what-relative-locks-delete-and-what-is-gone)
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
**relative** (BIP-68/BIP-112 CSV) rather than **absolute** (nLockTime) — and, since 2026-09-06,
*only* that kind: no absolute lock exists anywhere on a coin. §2 is that whole argument, including
what the substitution costs.

## 2. One clock: what relative locks delete, and what is gone

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
└─ T    TRIGGER    v3/TRUC, NO timelock, signed ONCE at first sight of F (still in the mempool), never re-signed
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

### 2.3 The clock that is gone: there is no flat backup chain

The ladder **is** the coin's only pre-signed material. Until 2026-09-06 every coin also carried a
**flat backup chain** — signed-once transactions spending `F` straight to the owner's own address at
**absolute** locktimes `L_k = L_0 − k·interval`, with `L_0 = H_deposit + initlock` and `k` counting
whole-coin hops (**INV-5**) — and that chain was the coin's calendar: a coin received `k` times sat
on `min(L_k)`, a height an ancestor's matured rung could spend `F` at. **None of that is built any
more.** `create_tx1` is deleted, no hop co-signs a receiver-paying backup, and a transfer message
carrying any `backup_transactions` is refused by the receiver (`verify_flat_backup_lane` refuses any
non-empty vector on either lane; `refuse_conveyed_flat_backups` on the child, tail and stub lanes).
INV-5 is RETIRED with the chain.

Consequences a reader must not skip:

- A coin that has been **received `k` times sits on nothing.** `coin.locktime` is `None` at every
  `k`. The copies a prior owner holds are the un-timelocked trigger `T` — which the current owner or
  their watcher can always broadcast first, since it needs no maturity — and superseded states,
  which lose the CSV race. There is no transaction in anyone's hands that becomes valid on a date.
- Broadcasting `T` still spends `F`, and so still pre-empts every other spend of `F` — which now
  means only the retained copies of the same `T`. `sever_from_f`
  (`clients/libs/rust-sdk/src/wallet.rs`) exposes that move by name; it is mechanically
  `unilateral_exit` on one coin. `deadline_safety_due`
  (`clients/libs/rust-sdk/src/refresh.rs`) still calls it as its fallback, but its due-predicate
  `coin_near_final` reads `coin.locktime`, which is `None` on every coin — so nothing schedules it,
  because there is no deadline for it to beat.
- The 100-decrement / 99-usable-hop budget is gone with the chain. A coin's off-chain life is
  bounded by its **renewal/rollover capacity** (§4c) and nothing else; the on-chain cadence is the
  cooperative re-anchor at that cap.

`initlock` and `interval` survive — in `GET /info/config` and in `TesrParams::flat_ladder_params`
(`lib/src/tesr.rs`: 10,000 / 100 on mainnet, testnet and signet; 1,000 / 10 on regtest) — only as
**compatibility constants**. `initlock` is now the FIXED exit window the split-depth cap measures a
leaf's exit walk against (§3, layer 3); `interval` is applied to nothing. The client still refuses a
coordinator whose table disagrees (ci-guard `deny_flat_ladder_config_drift`), so both sides read
the depth cap's window from the same place. The "~69.4-day mainnet epoch" this section used to
derive from them is not a property of any coin.

`sdk86` measured the two clocks on one received coin across two owners: after 300 idle blocks the
ladder fingerprint is byte-identical and `F` is unspent (part A, the CSV half — still true), and the
same coin's `L` was 300 blocks nearer (part B, the calendar half — now describing material that
does not exist). Part B is falsified and its re-derivation is pending; until it runs, `sdk30` (a) is
the standing evidence that an idle ladder does not age, and the received-coin case has no run.

### 2.4 What replaces "the timeout"

Four numbers, none of which is a calendar:

- **the alarm** — the only way to start a CSV clock is to broadcast `T`, which spends the funding
  outpoint `F` and is therefore **publicly visible on-chain**. A watchtower subscribes to one outpoint
  per coin and does nothing until that happens;
- **the notice** — after `T` confirms, the newest extension needs `E_m` confirmations and the newest
  state another `Δ_k` before *anything* is final. That is ≥ 288 blocks (~2 days) of defender notice on
  the shipped schedule, and never less than 144 blocks (~1 day) for any one tier;
- **the head start** — δ = 36 blocks (~6 h) per whole-coin hop at the state tier, δE = 36 at the
  extension tier. This is the margin by which the honest owner's transaction outruns the newest stale
  rival, and it must absorb a plausible reorg plus watchtower reaction time;
- **the cap** — the renewal/rollover capacity of the schedule (§4c). It is spent by *hops*, never
  by blocks, and when it is spent the coin is re-anchored (one cooperative on-chain transaction) or
  exited. `deadline_safety_due` at `auto_refresh_margin_blocks` = 144 still runs unconditionally
  every tick and has **no laddered subject**: it selects coins by `locktime`, and no coin has one.

### 2.5 What it costs, honestly

Three things. (a) **No unconditional no-watch window**: an absolute-ladder design gives a received
coin a period in which *the chain itself* rejects every stale backup and nobody has to be awake; TES-R
replaces that with perpetual — but alarm-driven, keyless, delegable — watching (residual **R-2**).
(b) **Longer worst-case unilateral latency**: a fresh root coin's unilateral exit walks `T → X → S`
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

**Compatibility constants** — `TesrParams::flat_ladder_params`, also compiled in: `initlock`/`interval`
= 10,000 / 100 (mainnet, testnet, signet) or 1,000 / 10 (regtest). No coin carries the chain these
used to parameterize; `initlock` is the fixed exit window of the split-depth cap (layer 3 below) and
`interval` is applied to nothing.

---

**Layer 1 — timelock ordering.** One idea, one mechanism, on every coin.

*The tiers (relative).* Every owner holds the whole pre-signed tier chain. A whole-coin transfer
co-signs a new state at `Δ_{k+1} = Δ_k − 36`; a renewal co-signs a new extension at
`E_{m+1} = E_m − 36`. Because every lock is relative and every tier is un-broadcast, none of them
counts down while the coin rests. When someone does broadcast `T`, the tiers become a strict ordering:
the current owner's state matures ≥ 36 blocks (~6 h) before the newest stale one, and a stale
*epoch*'s states hang on an extension that can never confirm at all. Evidence: `sdk40` (consensus
enforcement, stale-ladder death, renewal supersession), `sdk41` (after Alice pays Bob, Bob's lower-CSV
state wins the exit race and Alice cannot claw back), `sdk51` (a hostile trigger defended end to end
by a watchtower pass), `sdk50` (the full unilateral walk).

*There is no absolute mechanism beside it.* The signed-once backup with an absolute nLockTime — the
depositor's at `H + initlock`, each hop's `interval` lower, the exclusive exit window between
`L_k` and `L_{k−1}`, and the receive-side `ladder_decrements_by_interval` check with its
`LocktimeTooLow` / `LocktimeTooHigh` refusals — is the retired shape. Nothing builds it, nothing
conveys it, and the receiver refuses it if offered (`verify_flat_backup_lane`). `unilateral_exit`
walks the tiers and broadcasts no absolute-locktime transaction (`sdk50`); a coin with no ladder row
has no exit material and is refused by name, because there is no last arm to read `branch-` rows and
a latest backup from.

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
broadcasts a retained state only for a coin whose local status is on its **allowlist**,
`is_live_for_defence` — `IN_MEMPOOL`, `UNCONFIRMED` or `CONFIRMED`, because a ladder exists and is
defended from the block its deposit is first seen in — and it re-reads that field every pass. So a
lane that writes the status *after* the recipient already holds material leaves a window in which
the sender's tower reads a stale live status and broadcasts over the very outpoint the recipient's
new state depends on. The allowlist form is what makes that safe to extend: every route that hands
value away moves the coin OUT of those three statuses (`IN_TRANSFER`, `TRANSFERRED`, `WITHDRAWING`,
`WITHDRAWN`), so a lane added tomorrow is refused by default rather than by remembering to add it to
a denylist. Every value-handing lane therefore writes a durable `CoinStatus::IN_TRANSFER` **before**
the first call that can produce material for anybody else — the receiver-paying co-sign, the
coordinator open — and refuses if that write fails.
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
  enforces **`se_num_sigs == tiers + superseded`** — live tiers plus disclosed superseded tiers,
  summed over every hop of the conveyed ancestor chain. Any hidden extra co-signed state or
  extension shows up as a count mismatch. The flat term is pinned to **0** by REQUIRING
  `backup_transactions` to be empty (`PARENT_V2_BASELINE = 0`, `CHILD_V2_BASELINE = 0`): a deposit
  co-signs exactly three tiers and nothing else, so the enclave count after a deposit is 3, and a
  root's `T` counts as a live tier. Each hop discloses exactly one superseded state, so an
  undisclosed rival cannot hide; at depth 1 in `sdk60` the child census reads
  `child_num_sigs == 2 + 1`, and a received parent conveys an EMPTY parent chain at every `k`
  (`sdk76`). The receiver takes `F` from the bundle, fetches `tx0` from the chain, binds with
  `coin_authority_from_tx0`, and books `locktime = None`.
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
- **The split-depth cap is derived, not a literal — and it is measured against a fixed window.** A
  child's unilateral exit is a chain of sequential relative timelocks, and a laddered coin has no
  epoch deadline for that chain to race: nothing in its exit material matures on its own. So the
  cap bounds the *length and latency* of the walk a leaf would inherit — `exit_wait_blocks +
  exit_slack_margin`, with `exit_slack_margin` = `max(required/4, required/tiers)`, computed from
  the *signed* chain — against `initlock` as a constant. `max_split_depth(base, per_level,
  epoch_blocks)` (`lib/src/transfer/receiver.rs`) searches for the deepest child whose walk still
  fits, and `enforce_split_depth_cap_shaped` (`clients/libs/rust/src/tesr.rs`) enforces it with
  `epoch_blocks = initlock`. On mainnet the answer is **depth 8**, i.e. a **19-transaction** exit
  chain; on regtest 54 and 111. It moves with the network profile, which is the point — the
  ci-guard `deny_stale_depth_cap` exists because a builder that trusted a stale closed form once
  minted children no receiver would adopt, after terminalizing their parent. The receive-side
  **exit-headroom gate** that used to sit beside it — `check_exit_headroom_with_margin`, refusing a
  child whose walk could not finish before the parent's `min(L_k)` — has **no caller**: there is no
  deadline to measure headroom against. `sdk82` (plain) and `sdk88` (coloured) drove that gate and
  are falsified with it; `sdk17` now pins that a deep split on a received child SUCCEEDS.

Evidence: `sdk46` (the census against the *real* SE counter, at first mempool sight — accepts the
true count, rejects a hidden extra signature), `sdk47` (R′ across a transfer of a pre-established
ladder), `sdk54` (adversarial `verify_bundle`), `sdk58` (12 adversarial in-ladder-split cases, all
REJECT — aggregates, hidden state, Model-A, parent terminality, child-superseded race, count
padding, value spoof), `sdk60` and `sdk17` (the N-hop child census), `sdk76` (a received parent's
split ancestor census, with an empty parent chain). The re-derived ones among these are pending run
(see the note at the top of the page).

*There is no second lane to validate.* The receiver used to run a branch-validation path for
sub-coins over un-broadcast funding — `validate_branch` (locktime-zero branch, INV-4; value
conservation, INV-25; `reject_non_tree_branch`), `verify_terminal_parents` (one terminal ancestor
per structural input, INV-20, ERR-7) — with the blind-SE caveat that ancestor *ids* were not bound
to branch outpoints (TRUST-MODEL **B2**). That lane is deleted rather than hardened:
`refuse_branch_material` refuses any exit-branch transaction or terminal-parent id beside a ladder,
and the shape that carried them (`protocol_version` 0) cannot be received —
`ADMISSIBLE_PROTOCOL_VERSIONS` is exactly `[2, 4]`. On the ladder the ancestor chain is
key-derived: `verify_child_bundle` derives `A_parent` from the *fetched on-chain* `F.spk` and walks
each intermediate segment deriving its aggregate from the funding output it actually spends, so a
substituted id fails on the key, not on a name (B2 RETIRED). `sdk55`, which padded and inverted a
conveyed backup chain, is retired with the chain.

*The one lane rule that remains, tightened.* On a coloured bundle every conveyed flat backup used
to have to be **plain** — an `OP_RETURN` on one was refused by `verify_flat_backup_lane`
(`clients/libs/rust/src/tesr.rs`), because a prior owner's retained *coloured* backup would be an
undetectable allocation-theft primitive: it spends `F` and re-assigns the allocation to themselves.
The same function now refuses **any** conveyed flat backup, plain or coloured, on either lane — a
laddered coin has none, so a conveyed one is a co-sign the census cannot account for and a spend of
`F` a prior owner would keep. It runs on both acceptance paths and is guarded by
`deny_colored_backup_on_a_colored_ladder`; its unit tests
(`plain_backups_are_refused_on_both_lanes_and_the_refusal_names_the_lane`,
`rgb_material_on_a_flat_backup_is_refused_like_any_other_flat_backup`) pin the refusal.

**Layer 4 — watching.** The wallet's background task (`start_background`,
`clients/libs/rust-sdk/src/wallet.rs`) runs three passes, in order. On a laddered wallet only one of
them has a subject, and that one is event-driven.

1. **`deadline_safety_due(margin)`** at `auto_refresh_margin_blocks` = 144 — still **unconditional**
   (the ci-guard `deny_optional_deadline_safety` keeps it in `maintenance_plan` for every config),
   and it now has **no laddered subject**. Both of its remedies — the cooperative re-anchor, then a
   sever from `F` — are applied to coins whose `locktime` is within the margin, and `coin.locktime`
   is `None` for every coin, so on a laddered wallet it returns `(vec![], vec![])` every tick. It is
   kept unconditional, not kept busy: there is no deadline for it to beat. (`sdk87`, which drove its
   carrier variant against a deadline, is falsified; re-derivation pending.)
2. **`defend_ladders()`** — one `watch_pass` per adopted `tesr-` bundle, one `watch_child_pass` per
   adopted `ctesr-` split child and one per `spinetip-` tip, wired into the background loop
   unconditionally and gated to one pass per new block (a relative CSV can only mature on a block).
   Its liveness rule is `is_live_for_defence` — `IN_MEMPOOL | UNCONFIRMED | CONFIRMED` — so a ladder
   is defended from the block its deposit is first seen in. If `F` is unspent it is a **no-op**. If
   someone has triggered the coin, the pass races the owner's tiers, broadcasting each as its
   relative timelock matures; because the adopted current state carries the strictly-lowest CSV
   (enforced at adoption), it matures first and the funds land at the owner's own key. It emits
   `WalletEvent::LadderDefended{tiers_broadcast}`, is idempotent and incremental, and the bundle
   carries **zero key material**, so the duty is fully delegable and a second independent tower is
   idempotent (both asserted in `sdk45`). `sdk79` and `sdk80` cover the split and plain-child-split
   lanes. **This pass is the only defence a laddered coin needs**, root or leaf.
3. **`auto_exit_due(margin)`** at `auto_exit_margin_blocks` — still **derived, not chosen**:
   `auto_exit_margin_blocks_for(k_max, interval, child_depth) = k_max·interval + tesr_exit_txs(d)·144`
   (`clients/libs/rust-sdk/src/config.rs`), **2,120 blocks on mainnet** (14·100 + 5·144) and **860
   on regtest** (14·10 + 5·144) — but consumed only by a **legacy subject**: a coin still carrying
   `branch-` rows from the retired coloured split/combine lane, force-exited or (for a received
   carrier) materialized branch-only against its deposit-anchored `exit_deadline_block`, emitting
   `ExitDeadlineApproaching` / `TokenCarrierMaterialized`. **The leaf near-deadline loop is
   deleted**: an adopted child or spine tip has no height deadline, because no ancestor holds a
   matured spend of `F`, and `LeafExitForced` is no longer emitted (the variant survives on the
   enum). The `k_max = 14` term was the assumed ancestor-locktime span of the retired chain and
   bounds nothing on a laddered coin. On a wallet with no legacy rows the pass finds nothing.

Routine **background** re-anchoring is default-**off** (`background_auto_refresh = false`) and, like
the pre-spend hook `auto_refresh_due`, it is inert on a laddered wallet: it selects coins by
`locktime`, and none has one. Nothing on a coin needs re-anchoring on a schedule; the re-anchor is
reached at the renewal/rollover cap, by hand today.

*What the deadline machinery still reads.* `estimate_exit_cost` (`ExitCostEstimate`,
`clients/libs/rust-sdk/src/types.rs`) reports, for a laddered coin, `wait_blocks: 0` and
`exit_deadline_block == None` with `exit_deadline_blind == None` — which for every laddered coin
means "laddered, event-driven", not "I could not tell". Its `backup_vbytes` is the tier walk's
signed vsize only for a **root**: the lookup is a `tesr-` row, so a `ctesr-` child and a
`spinetip-` tip both report `0` and their walk must be read from `tesr_exit_vbytes(d)` instead.
A `Some` deadline, or
`exit_deadline_blind == Some(reason)` (a deadline exists and could not be computed;
`deadline_is_unknown()`), appears only for a legacy `branch-` coin, and only for that shape does
`unilateral_exit` broadcast branch-first and raise `WalletEvent::ExitBranchConflict` when a
*different* transaction is spending the branch root.

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

Alice deposits; the moment her funding tx is seen in the mempool the ladder is established, emitting
`LadderEstablished` — the enclave has co-signed exactly three times (`T`, `X_0`, `S_0`) and nothing
else. Confirmation later adds nothing. She then pays Bob a whole coin, Bob pays Carol, Carol pays
Dave — all off-chain, minutes apart, none touching the chain.

| State | Holder | Relative CSV on `X_0.out[0]` | Status after hop 3 |
|---|---|---|---|
| `S_0` (deposit state) | Alice | 1,440 | superseded, disclosed |
| `S_1` | Bob | 1,404 | superseded, disclosed |
| `S_2` | Carol | 1,368 | superseded, disclosed |
| `S_3` | Dave | **1,332** | current — lowest |

All four are mutually-exclusive spends of the same output, and all four are inert. `F` is untouched,
`T` is un-broadcast, and `X_0` (CSV `E_0 = 720`) is un-broadcast too. The chain contains exactly one
transaction for this coin: Alice's deposit. **Nothing in this table changes with time, and there is
nothing underneath it that does.** Each hop cost one co-signature and disclosed one superseded
state; the enclave count is 3 + 3 = 6, every one of them accounted for, and no one — not Alice, Bob
or Carol — holds anything that matures on a date.

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

- **before `T` is broadcast** — indefinitely, at zero cost and with no watching action required.
  There is no transaction anyone could send that the chain would accept, and nothing is running.
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
   `ADMISSIBLE_PROTOCOL_VERSIONS = [2, 4]` is an exact set (`admissible_shape`): `2` is a root-ladder
   conveyance, `4` a child conveyance, the un-laddered shape `0` no longer exists, and an unknown
   value is refused by name rather than read as "at least". The bundle's `parent_flat_backups` is
   **empty** — a laddered parent has none to convey. She does **not** terminalize the child.
6. Bob claims: `verify_child_bundle` (parent `F` read on-chain through the ancestor chain,
   exact-equality census with the flat term 0, depth cap), then he **completes the key handover** — the SE
   rotates its share so that `A_child` is *invariant*
   (`sender_share + SE_old == receiver_share + SE_new`), which is exactly what keeps the pre-signed
   child exit chain valid, and re-points `auth` to Bob. Alice is now **permanently locked out**.

Stale-state inventory after the split: Alice's superseded state, disclosed and counted, rivalling `SP`
over `X_m.out[0]` and losing to it by the whole state schedule. Nothing is on-chain and the funding
outpoint is still unspent — and Bob's child inherits **no calendar** from its parent, because the
parent has none: neither the root nor the child carries a flat backup (`PARENT_V2_BASELINE = 0`,
`CHILD_V2_BASELINE = 0`). What the child does inherit is the *length* of the walk back to `F`, which
is what the split-depth cap bounds against the fixed `initlock` window.

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

#### The same payment on the flat lane — RETIRED 2026-09-06

There is no second way to carve a payment any more. The walkthrough that used to sit here — a
coloured split on the legacy branch lane: a locktime-0 split transaction over `F`, un-broadcast,
each sub-coin given a fresh first backup at `H_split + initlock`, the payee racing the parent's
deposit backup at `H + initlock` with `auto_exit_due` force-broadcasting the branch before that
height — describes material nothing builds. `register_split_subcoins_n` and
`register_combine_subcoins` refuse by name, `create_tx1` (which gave the sub-coins their backups) is
deleted, and a transfer message carrying branch material beside a ladder is refused
(`refuse_branch_material`). A coloured payment is the coloured in-ladder split — `SP` over `X_m`'s
payload output with a headless coloured ladder per child (`sdk77`) — and a token piece two splits
deep is walked out, not materialized (`sdk39`, re-derived to the coloured lane, pending run). The
only coins that still carry `branch-` rows predate the rule; `materialise_carrier` and
`auto_exit_due`'s legacy loop exist for them alone.

### 4c. Depth: off-chain rollover and child chains

**Depth is bought off-chain and bounded by a derived cap.** When the next state's CSV would fall
below `d_floor`, the coin is renewed: two blind co-signs mint `X_{m+1}` (CSV `e0 − (m+1)·δE`) and a
fresh state on top of it — **zero on-chain bytes**. `mercuryrustlib::tesr::renew` is exactly two
`cosign_tier` calls over the ordinary `/sign/first` + `/sign/second` pair (`renew_auto` picks the
schedule's next rung); `m` and `k` are fields of the client's own bundle, advanced locally. **There
is no SE-side renewal counter machine and none is planned** — no `/renew/init` route, no
`total_sigs` column, no `{level, m, k}` state. The census does not need one: exact equality on the
TOTAL detects a hidden co-sign at *any* level, so per-level counters add nothing (residual **R-6**).
`renew_child` does the same for a received child (`sdk84`). **Renewal and rollover are library
calls, not yet invoked on the transfer path**: a wallet at the floor is refused with the remedy
named, and renewal is by hand today.

When the extension tier exhausts (`m = 15`), the coin rolls over off-chain (`rollover` /
`rollover_auto`): a 1-in-1-out self-split consuming the current state slot, whose child resting
output hosts fresh extension + state tiers and a fresh 576-hop budget. Cost: zero on-chain, +2
pre-signed txs (~250 vB) of *contingent* exit weight, +1 depth level; the parent is terminalized.
`sdk43` drives renew → rollover → renew past epoch exhaustion through those calls, with the funding
outpoint untouched, then exits unilaterally through the whole deep chain.

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
`exit_wait_blocks + exit_slack_margin` stops fitting inside the fixed 10,000-block `initlock` window
(`enforce_split_depth_cap`). Cooperative exit remains one transaction and one confirmation at any
depth. An optional per-level geometrically shrinking `e0`/`d0` schedule would bound the total worst
wait further, trading per-level hop budget; it is a dial, default off (open problem **O-4**).

What bounds *rollover* depth is the same cap, and past it the coin is **re-anchored**: one
cooperative on-chain transaction, 112 vB, rebearing the coin at depth 0. Net budget between chain
touches: 576 × 4 ≈ **2,300 transfers per 112 vB**. That re-anchor is a manual `refresh` today — the
"solo compaction priced into the next transfer's fee" is a design, not a scheduler that exists.

**The tree has no deadline.** The retired shape's depth was a tree of un-broadcast branch
transactions whose earliest hostile maturity was the minimum over every ancestor's retained backup
(`H_deposit + initlock` at best, `k·interval` earlier for each ancestor transferred `k` times
before its split — TRUST-MODEL **B6**). No ancestor holds a matured spend of `F` any more: a
laddered tree's exposure is the parent's un-timelocked trigger being broadcast, an event the
per-block `defend_ladders` child loop answers, and B6 is RETIRED with the chain. What is conveyed
and counted instead — the schedule and the SE's signature budget — is the coin's true remaining
capacity, readable off the bundle.

## 5. Over time: what a coin actually costs to hold

**No rent, and no calendar.**

- The tiers consume **0 vB per year** of block space, forever, and require no renewal traffic.
- No pre-signed *tier* anywhere can become valid until someone spends `F` in public and then waits out
  ≥ 288 blocks of relative timelocks — and there is no other pre-signed transaction on the coin.
- The **hop budget** is finite per depth level, not per coin: 36 state decrements per epoch × 16
  epochs = **576 whole-coin transfers** before a rollover adds a depth level — off-chain, through
  `rollover_auto`, by hand today. `needs_renewal(k)` and `needs_rollover(m)` (`lib/src/tesr.rs`) are
  the scheduling predicates.
- A coin's off-chain life is bounded by **renewals and rollover only**; the on-chain cadence is the
  cooperative re-anchor at that cap. There is no epoch spent by the wall clock, no `interval` spent
  per hop, and nothing for `deadline_safety_due` to defend — it runs every tick and finds no subject.

**What used to be here.** The retired shape rested a carrier and a sub-coin on the absolute-locktime
backup anchored at the root deposit, so a *received* one had to reach the chain before the earliest
stale ancestor backup matured, and this section tabulated the extension options (re-anchor,
self-split, materialize the branch) against `exit_deadline_block = H_deposit_root + initlock` — a
bound that was exact for a never-transferred parent and **too late by `k·interval`** for one
transferred `k` times before its split, with `auto_exit_margin_blocks_for(k_max = 14, …)` absorbing
an *assumed* span (**B6**). None of that describes a coin minted under the rule: there is no
deposit-anchored deadline, nothing to convey about ancestors' locktimes, and the `k_max` term
survives only inside a margin that bounds a legacy `branch-` coin. The one option that keeps its
meaning is the **re-anchor** — `refresh` / `refresh_sponsored`, one SE-co-signed 112-vB transaction
into a fresh aggregate, user-paid or sponsor-rebated (`sdk30`, `sdk38`) — and it is reached at the
renewal/rollover cap, not on a date.

**The block-space ledger, per payment.** Because essentially every payment is a split, the leaf lane
is the only lane that describes a real user ([PARTIAL-PAYMENT-ECONOMICS.md
§1.3](../spec/PARTIAL-PAYMENT-ECONOMICS.md); ordinary on-chain comparison ~154 vB for a 1-in-2-out
payment):

| leaf lane, per payment | block space | against ~154 vB on chain |
|---|---:|---|
| spent onward off-chain | **0 vB** | this is the product |
| swept and settled | **~105 vB** | **1.47× better — the cap for a leaf settled on its own, i.e. without a close** |
| walked out unilaterally | **250 – 2,719 vB** | **worse than on-chain** |
| **shipped default** | **418 vB** | 2.7× worse |

State both sides or the number is marketing: for the population that actually exists, the shipped
default settles a payment for **more** block space than doing it on chain. Read the swept row with
its own status: the sweep's **decision** is built and sited where REQ-49 puts it — `sweep_at_claim`
on `SdkConfig`, read inside `claim()`, over the predicate `mercurylib::sweep::may_absorb` /
`should_settle` — but the **swap itself** is not, so the flag ships `false` and enabling it is a
hard error by name. `combine_leaves` (`clients/libs/rust/src/combine.rs`) has no caller outside
`sdk83`, and the cooperative child exit every §3 number rests on is unverified.

*What used to be claimed here, and is false.* This paragraph said the **discharge round** would
change the row by an order of magnitude and was "design, not built, its SE enforcement point
empty". Neither half holds. The round as a round — the R0–R9 sequence, round eligibility, the
operator float — was **deleted from the design** (SPEC §5.4.7; ci-guard
`deny_round_shaped_mechanisms` keeps its shapes out, REQ-81: a close is a root owner's decision,
never a calendar's). What replaced it, the owner-triggered **close**, is built on both sides: the
client has `collapse_obligations` / `collapse_first` / `collapse_grant` / `request_collapse`, and
the enclave enforces REQ-56 (`lockbox/include/registry.h`, `db_manager.h`'s
`freeze_root_and_store_collapse_sig`, the `/collapse_grant` route in `lockbox/src/server.cpp`),
refusing any `C` that does not pay every unreleased frontier leaf its full funding value to its own
exit key. `sdk94` drives the accept path end to end. What does not exist is a scheduler, so the
figures above remain the one-leaf-at-a-time numbers. The satoshi ledger is a different and larger
quantity: every pre-signed tier permanently burns `committed_fee(3.0) + 240` = **615 sat**, a leaf's
own two tiers burn 1,230, and a combine that spends `SP.out[j]` directly never broadcasts those tiers
at all.

## 6. Real-world situations

### 6.1 Receiver goes offline for N days

**Nothing *triggers*, and nothing else runs.** There is no CSV maturity to sleep through, because no
CSV clock has started, and there is no calendar to sleep through, because the coin has none. One
thing the offline period does cost:

- **reaction time** — if a past owner triggers the coin while you are away, someone has to run
  `defend_ladders()` (or a delegated tower has to) within the notice window: ≥ 288 blocks (~2 days)
  before *any* hostile transaction is final, and ≥ 36 blocks of head start at each tier after that.
  Since the alarm is a public on-chain event and the bundle is keyless, running two or three
  independent towers reduces this to a liveness question about towers. This is residual **R-2**, and
  it is the whole of TRUST-MODEL **B4**: a laddered coin has exactly one clock, the reactive one,
  and a keyless delegate's coverage of it is complete — every exported entry, root or leaf, carries
  a trigger on the watched `F` and `deadline_block: u32::MAX`, so the delegate watches the *event*
  of `F` being spent and nothing else exists to watch. (The second half B4 used to carry — "safe
  against `min(L_k)` only until it approaches, and no keyless tower covers that" — is RETIRED: there
  is no `min(L_k)`.)

The retired shape's exposure — a sub-coin or received carrier whose root deadline, anchored at the
root deposit, could pass while its locktime-free branch sat unbroadcast — no longer exists for any
coin minted under the rule. A coin that still carries legacy `branch-` rows keeps its
deposit-anchored deadline and `auto_exit_due`'s legacy loop; for it alone, `ExitBranchConflict`
means a *different* transaction is spending the branch root, and rebroadcasts of the identical
branch tx are tolerated.

### 6.2 SE goes down permanently — the day it happens, and a year later

The SE's death removes the *cooperative* paths only; every unilateral path is pre-signed and needs
nobody — with one onboarding boundary.

**The onboarding window.** `T`, `X_0` and `S_0` are co-signed once, when the wallet first sees your
funding tx in the mempool (`check_deposit`, `clients/libs/rust/src/coin_status.rs`, or `claim()`'s
establish pass in the same tick). Between broadcasting the funding tx and that co-sign you have
**no** unilateral path at all — there is no flat backup signed ahead of the ladder — and an SE that
dies inside that window strands the deposit in the 2-of-2 permanently.

How the wallet reports that depends on the lane, and the difference is worth knowing before you
fund. Under `LadderAtSight::Plain` a deposit whose ladder cannot be established is **not booked**:
`check_deposit` rolls the coin back to `INITIALISED` and returns the failure, so "visible on chain
yet still `INITIALISED`, no `LadderEstablished`" is the signal to stop funding. Under
`LadderAtSight::Defer` — the SDK's own lane — `update_coins_ex` books and persists the coin
`IN_MEMPOOL` **before** the establish pass runs, so a failure leaves a booked coin with no exit
material; the signal there is a `LadderSkipped{reason}` event and a `ladderskip-` row
(`ladder_skip_reason` / `flat_only_coins`), never the coin's status. Either way the coin is not
lost: cooperative withdrawal still works. (One deliberate exception on the `Plain` lane: a
`single_use` coin is booked with no ladder by design, because the SE refuses any second co-sign on
it.) (TRUST-MODEL **B5**; `sdk16` covers the fresh-user onboarding path.)

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

Note what is *not* available under a dead SE: the cooperative de-trigger, the cooperative
re-anchor, and renewal or rollover. So a hostile trigger costs you a per-tier race rather than a
chosen settlement, and a coin at its renewal/rollover cap can only leave by walking out — there is
no date by which it must, only a budget it cannot extend. The correlated case (dead SE *and* mass
grief) is residual **R-1**; it is never confiscation. **Outcome:** funds recovered on-chain; the only
variable is how long you wait, never whether you win.

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
need the SE.

Each tier is nVersion=3 (TRUC) and carries a **committed fee** at 3 sat/vB drawn from
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
quantifying it against mainnet fee history is open problem **O-2**, not done. There is no second
shape with a different answer: a stranded tier stays valid forever and is rebroadcast for free, and
nothing hostile matures while it waits.

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

**There is no other move.** A past owner used to have one: wait for `min(L_k)` and let a retained
flat backup become valid on its own, which nothing in the tier tree could out-race. No such backup
exists — the only spends of `F` a past owner holds are copies of the un-timelocked `T`, which the
current owner can always broadcast first, and superseded states, which lose the CSV race. Every
hostile move therefore begins with the public alarm above (`tb05`, re-derived to that defence,
pending run).

### 6.6 The high-velocity merchant coin

A merchant coin gets 36 hops per epoch and 16 epochs — **576 whole-coin transfers per depth level**
— and when a level exhausts it rolls over **off-chain** for zero on-chain bytes (`rollover_auto`, a
library call the merchant's wallet invokes by hand today; nothing on the transfer path does it yet).
`sdk43` is the standing proof that renewal and rollover are unbounded and free. Elapsed time
contributes nothing to the hop budget, and there is no second budget it contributes to: the honest
merchant budget has **one** line, hop count against 576 per level, and the re-anchor is reached when
the depth cap is — not on a calendar, and not after 99 hops of a flat chain that no longer exists.

What a high-velocity operator should also budget for is **exit weight**: every depth level adds 2
pre-signed txs and 720 blocks to a contingent unilateral exit. Neither is visible to the payer.

### 6.7 Long-hold cold storage

A self-deposited, never-transferred coin has: no counterparty holding stale state; no party other
than the owner able to start its CSV clock at all (copies of `T` travel only with ownership
history); and the SE alone cannot spend a 2-of-2 it holds one share of. It sits on nothing else —
no epoch, no calendar — and can sit untouched indefinitely; the only thing that ever brings it back
on chain is its owner's choice (a cooperative re-anchor at 112 vB, or an exit).

A **received** coin is different in one respect: its previous owners hold `T` and stale states, so
it carries the perpetual alarm-driven watching duty of **R-2**. Cold storage means nobody is watching
— so for a received coin, cold storage means delegating the *reactive* half to towers that are
watching, and that half is the whole duty: a keyless tower covers everything there is to cover.

The remaining reasons a statechain coin is an imperfect vault are operational: cooperative paths depend
on SE liveness (and any `epoch_deadline`, §6.8); pre-signed tier fees are frozen at signing time and
drift against the fee market, with owner-funded bumping the only rescue; and the exit material is not
seed-derivable, so the *real* long-hold risk is losing `wallet.db` and the recovery bundle (§6.9,
TRUST-MODEL **B7**). A coloured carrier idles exactly like a plain coin: `sdk32` is the standing
record of a received token idled past every horizon with nothing lost, and a received coloured child
has no root deadline to materialize before (`sdk34`, re-derived to the event-driven defence, pending
run).

### 6.8 Epoch-bounded coins (compliance / limited mandate)

An optional per-coin `epoch_deadline` (unix seconds, set at deposit) makes the SE refuse **new**
co-signatures once its clock passes the deadline (410, ERR-2; `RGB_E2E=7`, re-derived over
`single_use` deposits, pending run). This is a *server-side* gate on `sign/first`, and it is the
only thing in the system that is a date. Unlike round expiry there is no sweep: unilateral exit never
needs the SE, so the pre-signed tier chain lives on. Use it to hard-bound circulation: a custodial
mandate ending on a date, a compliance-scoped instrument, a bounded delegation. Note the interaction
with the ladder: past the epoch the SE also refuses renewal, rollover **and the cooperative
re-anchor**, so the coin's off-chain life ends at whatever renewal/rollover capacity it has left and
it must then be exited — unilaterally, needing nobody, whenever its owner chooses. Nobody, including
the SE, can confiscate it.

### 6.9 Receiving as a fresh user with zero on-chain footprint

`sdk16`: a brand-new wallet with no UTXOs, no deposits and no chain history receives off-chain and is a
first-class owner. Its exit material is entirely local — the tier bundle (`tesr-<id>` for a whole
coin, `ctesr-<id>` for a received split child, `spinetip-<id>` for a change tip) with its trigger,
extension, current state and per-tier CSV schedule. That bundle is a complete, SE-independent exit
containing no key material, which is what makes it delegable (`sdk45`).

The corollary applies to **every** coin: **a mnemonic alone does not restore a wallet.** The seed
rebuilds the key hierarchy but not the per-coin exit material — statechain ids and the tier chain
(plus, for a coin that predates the rule, its `branch-*`/`parents-*` rows) — which lives only in the
wallet database and which the blind SE cannot re-serve after a claim (TRUST-MODEL **B7**).
`export_recovery_bundle` snapshots all of it; `sdk81` covers recovery of an interrupted in-ladder
split from its journal. Token wallets additionally need the entire `rgb_data_dir` (including its own
plaintext RGB seed), which the recovery bundle deliberately does not embed.

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

**A sub-economic piece.** A prior owner of an *ancestor* holds no matured spend of `F` to void a
piece with — the 112-vB backup that used to do it at zero marginal cost no longer exists. What they
hold is the root's un-timelocked `T`, and broadcasting it does not void anything: the piece's chain
*descends* from `T`, so the payee (or their tower) simply walks it out — at the walk's cost, which
for a small piece exceeds its value. That walk cost is the economic reason `min_child_value` exists
and the reason a piece received and immediately cashed out should never have been an off-chain
split.

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
`exit_deadline_blind`). For a laddered **root** `backup_vbytes` is the tier walk's signed vsize — a
`ctesr-` child and a `spinetip-` tip report `0`, because the lookup is a `tesr-` row; read their
walk from `tesr_exit_vbytes(d)` instead —
`wait_blocks` is 0 while idle, and `exit_deadline_block: None` with `exit_deadline_blind: None` is
the answer for **every** such coin — "laddered, event-driven". Only a legacy `branch-` coin can
report a `Some` deadline, and only for it does `exit_deadline_blind: Some(reason)` mean "I could not
tell", which `deadline_is_unknown()` names. `unilateral_exit` returns per-coin
`ExitStatus{complete, wait_blocks}` and is idempotently re-callable: it advances the tier chain one
maturity at a time, so "call it again next block" is the whole protocol. Events:

| Event | Means |
|---|---|
| `LadderEstablished` | the deposit was laddered at first sight of `F`, still in the mempool — it is now exitable, and transferable once it confirms |
| `DepositConfirmed` | funding reached the confirmation target; nothing about the exit material changed |
| `LadderSkipped{reason}` | the establish pass could not ladder this coin; `LadderSkipReason` names why (`RgbCarrier`, `PlainLadderOverCarrier`, `AttestationIdentityUnpinned`, `CoordinatorUnavailable`, …). The coin has **no exit material** until a retry succeeds; the reason is diagnostic, never a licence |
| `TransferClaimed` / `TokenTransferClaimed` / `TransferCancelled` / `BalanceUpdate` | ordinary bookkeeping |
| `LadderDefended{tiers_broadcast}` | someone triggered a coin of yours and your pass raced its tiers — watch it through |
| `WatchtowerBlind{pass, detail}` | a defence pass could not read what it needed. **Not** "nothing was due" |
| `CoinRefreshed` | a coin was re-anchored — **re-export your recovery bundle** (new statechain id, new exit material) |
| `ExitBranchConflict` / `ExitDeadlineApproaching` / `TokenCarrierMaterialized` | legacy `branch-` coins only: a different tx is spending the branch root; `auto_exit_due` force-exited or materialized one against its deposit-anchored deadline. A laddered coin never emits them |
| `LeafExitForced` | **no longer emitted** — its producer, the leaf near-deadline loop, is deleted. The variant survives on the enum so match arms compile |

Deposit-time cost surfaces as `SdkError::TokenPaymentRequired{token_id, deposit_address, fee_sats}`.

**What a user must do, and when.**

| Trigger | Deadline | Action |
|---|---|---|
| Holding a coin, nothing happening | none | nothing. Nothing on the coin ages, received or not |
| `LadderDefended` fires, or you see `F` spent | within the tier head starts (≥ 288 blocks total notice, 36 per tier) | let `defend_ladders()` keep running; if the SE is up, take the de-trigger instead of racing |
| Going offline with a received coin | none — but the reactive duty needs *someone* awake | delegate the keyless watch bundle to one or more towers; that covers everything there is to cover |
| A coin at its renewal/rollover cap | none — a budget, not a date | renew or roll over (library calls, by hand today), or `refresh` / `refresh_sponsored` to re-anchor |
| `WatchtowerBlind` | immediately | fix the backend. A blind pass is not a quiet one |
| SE unreachable and you want out | none — any time | `unilateral_exit`, re-call each block until `complete` |
| `CoinRefreshed` (any cause) | promptly | re-export the recovery bundle |
| A coin recorded `LadderSkipped` | none, but it has no exit until the reason clears | wait for the retry (transient reasons), re-fund above the coloured floor, or pin an identity. `PlainLadderOverCarrier` has **no** remedy — `colored_reanchor` refuses a plain ladder by name — and neither does a sub-floor carrier |

**Re-anchoring, in one line.** `refresh(coin)` spends the coin's outpoint into a fresh aggregate with
one SE-co-signed 112-vB transaction, and the coin comes back with a brand-new funding outpoint, a
brand-new ladder (established at first sight of the new funding tx), and `k = 0` prior owners. It
kills every retained trigger copy and superseded state rooted at the old outpoint, and it returns a
deep coin to depth 0. It resets no calendar, because there is none. It is cooperative — if the SE is
gone, exit instead.

Two fee models: **user-pays** (`refresh` — coin = amount − fee) or **operator-pays**
(`refresh_sponsored` — a funded operator rebates the fee off-chain). Why a rebate rather than the
operator adding an input? Because the re-anchor is **single-input by construction**: the blind SE
co-signs exactly one input and holds no funds or chain view, so nobody can co-fund the transaction. A
sponsor paying from a laddered coin rebates via an in-ladder split, whose child must clear
`min_child_value`, so the rebate is `max(fee_sats + DUST_LIMIT, min_child_value)` and the operator
absorbs the round-up, leaving the user ≥ whole. `sdk30` is the happy path (both fee models); `sdk38`
pins what a *broke* sponsor does — it errors cleanly and the user keeps the refreshed coin.

**Auto-refresh, honestly.** `SdkConfig::auto_refresh` is on by default and `auto_refresh_due(margin)`
still keys off `coin.locktime`: it re-anchors any confirmed non-carrier coin whose `locktime` headroom
has fallen to `auto_refresh_margin_blocks` (144). No laddered coin has a `locktime`, so on a laddered
wallet the pass — as `transfer`'s pre-spend hook and as the routine background pass — finds nothing
due and returns `Ok(vec![])`; the flags are inert, kept so the cost of a future on-demand re-anchor
would appear as a payment fee rather than a balance shrinking in the background. Two things still
hold. First, an unreadable carrier set is an `Err`, never a quiet empty pass. Second,
`deadline_safety_due` runs regardless of any flag and, like the hook, has no laddered subject; it
reports every coin it could not defend rather than returning a clean `Ok` (ci-guard
`deny_uncovered_carrier_deadline` pins the shape of the pass), and a carrier that has no ladder is
not "undefended on a calendar" — it has no exit material at all until it is coloured, which the
wallet reports as `LadderSkipped`.

**What a watchtower must watch** (per coin): the funding outpoint `F`, and nothing else. A spend of
it is the alarm — run `watch_pass` per block from there, broadcasting each tier as its CSV matures.
Idle coins need no polling beyond the subscription, and multiple towers compose idempotently
(`sdk45`). Every exported entry — root, adopted child, spine tip — carries a `WatchTrigger` on `F`
and `deadline_block: u32::MAX`, with `backup_tx: None`, so a keyless delegate's coverage of a
laddered coin is complete. A keyless tower must **not** try to be package-aware (it has no funding
input, so its package would be 1-parent-0-child); a funded tower **must** be.

Scope that "every" honestly: **no entry the exporter can now produce carries a `backup_tx` or a
finite deadline at all.** The height-driven arm is deleted, so `export_watch_bundle` emits entries
for laddered coins only — an un-laddered coin and a legacy `branch-` coin are both skipped, and
`flat_only_coins` lists only coins a `claim()` pass recorded a skip reason for, so a coin can leave
the bundle without appearing in that listing. The advice a previous draft gave here — that an
offline-capable tower should cache `initlock` and the root deposit height for a `branch-` coin — is
obsolete: there is no entry for it to compute a height for.

Bundles are **snapshots**: re-export after any operation that mints or replaces coins.

**Time-to-money, per flow:**

| Flow | On-chain txs | Wait |
|---|---|---|
| Deposit → exitable | 1 (your funding tx) | none — the ladder exists from the first mempool sighting |
| Deposit → spendable | the same tx | confirmation target + SE registration |
| Off-chain receive (whole coin or split child) | 0 | seconds (API round-trips + validation queries) |
| Off-chain onward send of a received child | 0 | seconds — `sdk60` does two hops with the funding outpoint never spent |
| Cooperative exit | 1 per coin (~111 vB) | ~1 conf |
| Cooperative re-anchor | 1 (112 vB) | ~1 conf |
| De-trigger after a hostile trigger | 1 (125 vB) | ~1 conf, no CSV wait |
| Unilateral exit, laddered root coin | 3 (T+X+S = 375 vB) + 0–3 fee children in a spike | `E_m + Δ_k` sequential — 2,160 blocks + confirmations ≈ 15 d fresh, −36 per hop |
| Unilateral exit, laddered depth-`d` child | `3 + 2d` (`293·d + 375` vB) | `720·d + 2,160 + (3 + 2d)` blocks — depth-1 ≈ 20 d, the mainnet cap of depth 8 ≈ 55 d |
| Legacy token materialization (`branch-` rows that predate the rule) | branch only (2d+1 txs) | now — the allocation settles on the resting output |

The latency line deserves emphasis, because it is the real price of relative timelocks: **a unilateral
exit is slow, and it gets slower with depth.** Cooperative exit, when the SE is alive, is one
transaction and one confirmation at any depth — and that is the path essentially every user takes. The
unilateral chain is the guarantee that makes the cooperative path safe to prefer, not the path itself.

**Sharp edges, honestly.**

- **No unconditional no-watch window** on the ladder (**R-2**): a received coin must be watched —
  keyless and delegable, alarm-driven with ≥ 1 day of notice — and that reactive duty is the only
  one (**B4**, one clock).
- **A coin with no ladder has no exit and no second lane** (**B12**): a carrier that cannot be
  coloured, a plain ladder over a carrier, and every SDK deposit on an unpinned network are faults to
  repair, reported but not exitable until repaired.
- **Renewal, rollover and the coloured re-anchor are not on the transfer path**: library calls and
  a manual call. A coin at its cap is refused with the remedy named, not renewed in place.
- **The conveyance window is a wall clock, not ownership** (§3, `sdk91`); REQ-61's owner latch is
  **design, not built**.
- **Nothing schedules a close** (SPEC §5.4): the discharge *round* is deleted from the design, and
  the owner-triggered close that replaced it is built end to end (client `collapse_grant` /
  `request_collapse`, the enclave's REQ-56 predicate, `sdk94`) — but only an owner starts one, so
  the §5 block-space figures are the one-leaf-at-a-time numbers.
- **Spike-time bumping is owner-funded and node-gated** (**R-4**): the code is wired, but no suite test
  exercises it and a keyless tower structurally cannot; fee bumping ships with no fee source.
- **Deep-DAG unilateral latency** (**R-5**): the mainnet cap of depth 8 is ~55 days; the geometric
  schedule that would shorten it is default off (**O-4**).
- **The census's trust premises** (**O-1**): `se_num_sigs` is earned by the pinned-identity
  attestation (**P3**), but the sid ↔ aggregate-key binding (**P1**) is still coordinator-supplied and
  unattested, and a malicious enclave can attest anything (**B11/CO-1**).
- **No RBF on any pre-signed tx.** (**B6**, un-conveyed ancestor locktimes, and **B2**, ancestor-id
  substitution on the branch lane, are RETIRED with the material they described.)
- **Recovery bundle is not seed-derivable** for any coin (**B7**); token wallets also need
  `rgb_data_dir`.

## 8. FAQ

**Does an idle coin have a deadline?** No. Nothing in the tier tree matures until someone broadcasts
`T`, so an idle coin's exit chain is byte-identical after any amount of waiting and costs 0 vB of
rent — and there is nothing else on the coin: no flat backup, no absolute locktime, `coin.locktime`
is `None`. That is INV-27, unconditionally. (Until 2026-09-06 the answer had a second sentence about
a retained flat chain and its `min(L_k)`; that chain is gone. `sdk86`, re-derived, measures a
*received* coin over two hops and 300 idle blocks — pending run.)

**Do I lose anything if I do nothing for a year?** No. Nothing is forfeited by timeout — no output
ever pays the operator — and nothing anyone else holds becomes valid on a date. The one duty is
reactive: if a past owner triggers your coin, you or a tower must respond inside the notice window,
and a keyless tower can do all of it. What a year *does* consume is nothing; what hops consume is the
renewal/rollover budget, and when that runs out the coin is re-anchored or exited at its owner's
convenience.

**Can the SE steal my coin?** Not alone: the coin is a 2-of-2 and the SE never holds your key share.
Colluding with a *previous owner* it can fresh-sign a competing spend and force a symmetric first-seen
race against your own un-timelocked trigger (§6.3, **B1**) — the trust floor shared with every
statechain design. It cannot forge terminal state through the API (monotonic), and misbehaviour on
structural nodes is publicly queryable.

**Can the SE freeze my funds?** It can refuse to co-sign (or die, or be legally compelled), which kills
the *cooperative* paths only: transfers, renewal, rollover, cooperative withdraw, de-trigger, and
re-anchor. Unilateral exit is pre-signed and SE-independent; worst case is the tier wait. A frozen
coin has no date to beat — it exits when its owner decides, and its only lost capability is
extension. The one boundary case is the onboarding window — the guarantee begins when the SE
co-signs the trigger at first sight of the funding tx (§6.2).

**What if I lose my wallet database?** That is loss of funds, and **no** statechain coin restores from
the mnemonic alone (**B7**). Back up with `export_recovery_bundle` and re-export after every transfer,
claim, split, child re-transfer, **or refresh**. Token wallets must additionally copy the whole
`rgb_data_dir`.

**Can two people be handed the same coin?** The enclave consumes each secnonce atomically (one
signature per nonce — **INV-23**, `sdk12`), the coordinator refuses every co-sign while a transfer of
that coin is open, and refuses to re-address an open transfer to a different recipient. A *malicious*
SE could still try; what a second "owner" cannot satisfy without the SE visibly double-signing is the
receiver's census — `se_num_sigs` must equal `tiers + superseded` against an enclave-attested count,
with the flat term pinned to 0 by an empty `backup_transactions`, so a hidden extra co-signed state
shows up as a mismatch (`sdk46`, `sdk54`, `sdk58`, `sdk60`).

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
and unbounded (`sdk43`), and amounts are exact rather than denominated. One honest caveat:
"consensus-dead" is *race-conditional*. (The second caveat that used to sit here — the flat chain's
calendar as a maintenance duty — is gone with the chain.)

**Is there any coin that is not laddered, then?** Not as a lane — but as a fault, yes, and on an
unpinned network it is not only carriers. An RGB carrier must never be given a *plain* ladder (a
plain tier spend destroys the allocation), and the *coloured* one that carries it needs a pinned
enclave identity to establish, which a network with no provisioned enclave does not have — so on
such a network a carrier has **no exit material**, is not conveyable, and is reported as
`LadderSkipped{RgbCarrier}` every pass until a later pass can colour it. On the same network the
SDK's establish pass ladders **nothing at all**, plain deposits included, because it calls
`get_statechain_info` and that call needs an identity to verify the count attestation against
(`LadderSkipReason::AttestationIdentityUnpinned`) — so an SDK wallet's plain deposit is booked with
no unilateral exit either, and cooperative withdrawal is its only route out. The absolute-locktime
backup that used to serve both those coins no longer exists (**B12**). And "a sub-coin
over un-broadcast funding" is not on any list: that coin still has no confirmed prevout for a
trigger to spend (a v3 tier cannot relay over an unconfirmed parent) — **B0** is permanent — but its
ladder hangs off `SP.out[j]` instead, which is what makes an in-ladder child and a spine tip
laddered coins with un-broadcast funding rather than a contradiction in terms.

**What happened to the locktime-0 branch transactions?** They belonged to the retired coloured
split/combine lane, where a branch had to beat every deposit-anchored stale backup unconditionally
(**INV-4**). Nothing mints them any more — `register_split_subcoins_n` and
`register_combine_subcoins` refuse — and a coin that still carries `branch-` rows is a coin that
predates the rule.

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
a keyless one. Cooperative exits pay normal fees at live rates.

**What if the mempool purges my pre-signed tx?** Nothing is lost: pre-signed transactions never expire
and rebroadcast is free. `exit_pass` and `watch_pass` are idempotent and incremental, so the next call
re-broadcasts what is missing.

**Is there any scenario where an honest, online user loses funds?** Within the model, one: the SE
colludes with a past owner and wins the first-seen race against the online user (§6.3) — and the
default wallet does not bump, so "wins the race" is not merely theoretical. Plus one boundary case with
no adversary at all: an SE that dies before co-signing the trigger strands the deposit in the 2-of-2.
Every other adversary — stale broadcasters, triggers, griefers, dead SEs, fee spikes — loses to an
online user mechanically. Offline is where the qualifiers pile up (§6.1, §6.7).

**What happens when the hop budget runs out?** A refusal that names the remedy. At 36 state
decrements the extension is renewed off-chain (`renew_auto`); at `m = 15` the coin rolls over to a
fresh level, also off-chain (`rollover_auto`); past the depth cap it is re-anchored (`refresh`, one
112-vB transaction). `sdk43` runs the whole sequence through those library calls. None of them is
invoked on the transfer path yet — renewal is by hand today — so a transfer that would breach the
floor is refused rather than renewed in place. There is no second budget that runs out visibly on a
calendar.

**Does splitting extend my coin's life?** It neither extends nor shortens it, because there is no
life to extend: an in-ladder split gives each child its own extension and state tiers and costs the
split node its terminality, and the child inherits **no calendar** from its parent — neither has
one. What the child inherits is the length of the walk back to `F`, which the split-depth cap bounds
against the fixed `initlock` window.

**Can a previous owner do anything at all before broadcasting the trigger?** No. Their states and
extensions spend outputs that do not exist on-chain, and the SE will not co-sign for them — key
rotation plus the secnonce consume plus the pending-transfer lock mean the SE answers the current
owner. So their only move begins with `T`, which is public. (They used to also hold a flat backup at
`L_j > min(L_k)` that needed no trigger and simply became valid when its height arrived; no such
backup exists.)

**What if my watchtower dies?** You inherit its duty on your next wake: check that no funding
outpoint was spent hostilely, and if one was, run `defend_ladders()` immediately. Exposure is limited
to coins triggered during the outage — nothing else can have happened. Towers are keyless and
idempotent, so run more than one; together they cover everything there is to cover.

## 9. Comparison recap: over-time behaviour

| | **Ours (TES-R)** | **Spark** | **Ark / Second** | **Absolute ladder** |
|---|---|---|---|---|
| Exit-chain ageing | **None** — relative CSV on un-broadcast txs; the tier tree never ages, and there is no other chain | Relative ladder, decrementing per hop; unbounded *if* renewed | Round expiry (~weeks), hard | Absolute, one horizon |
| Calendar deadline | **None.** `coin.locktime` is `None` for life; nothing anyone holds matures on a date | Operator-renewed | Round expiry | Same as the exit chain |
| Idle on-chain footprint | **0 vB/yr**; one ~112-vB re-anchor at the renewal/rollover cap, reached by hops, never by time | 0 | Refresh per round or lose funds | 5,840 vB/coin-yr |
| Renewal | **Off-chain and unbounded** — lower-CSV extension re-sign, then off-chain self-split rollover (`sdk43`), as library calls (by hand today); on-chain only for the re-anchor at the cap | Operator-group churn | Mandatory per-round refresh participation | On-chain re-anchor per coin per horizon |
| What invalidates old state | **Consensus** — lower CSV wins the trigger output, old epochs' parents unconfirmable — plus SE refusal and the receiver's attested census | Operator honest key deletion (1-of-n trust) | Round expiry | Absolute locktime ordering |
| Operator dies | All exits pre-signed; wait `E_m + Δ_k` (≈ 15 d for a root, ~55 d at the depth cap), funds whole; no date by which it must happen | Unilateral path exists; timelock race | Exit window critical; miss it → server sweeps | Wait ≤ `initlock` |
| Stale state over time | Inert until a **public** trigger, then ≥ 288 blocks of notice and a ≥ 36-block head start per tier — and nothing else, ever | Timelock race; key-deletion honest-1-of-n | Dies at expiry (the same knife that threatens users) | Timelock race |
| Missed-liveness outcome | Raceable after a public trigger + ≥ 1 day notice; **never confiscated by design**, and never losable to a date | Safe if renewed; trust-dependent | **Confiscation** — funds sweep to the server | Raceable after maturity |
| Operator misbehaviour visibility | Terminal state enclave-attested and publicly queryable per node | Not queryable per node | Round tree is public | None |
| Offline requirement | **Alarm-driven for triggers** (keyless, delegable to N towers), and that is the whole requirement | Online for renewals, forever | Online every round, forever | Online after maturity |

Further reading: the ladder in [PROTOCOL.md](../spec/PROTOCOL.md) and
[CHILDREN.md](../spec/CHILDREN.md); Lightning over the ladder in
[LIGHTNING.md](../spec/LIGHTNING.md); what a payment costs in
[PARTIAL-PAYMENT-ECONOMICS.md](../spec/PARTIAL-PAYMENT-ECONOMICS.md); the normative requirements in
[SPEC.md](../spec/SPEC.md) (REQ-33/34/35/36/38/61, INV-19/20/23/27/28, ERR-1/2/3/7; INV-4, INV-5 and
INV-25 describe the retired branch and flat-backup material); the trust map in
[TRUST-MODEL.md](../spec/TRUST-MODEL.md) (B1–B12, with B2, B6 and the calendar half of B4 RETIRED);
the short comparison in [invalidation.md](invalidation.md); partial amounts in
[granularity-deep-dive.md](granularity-deep-dive.md); exit mechanics in [exits.md](exits.md).

Test evidence cited on this page, partitioned by what the rule actually did to each file (`git show
--stat 9ddc4bb -- clients/tests/rust/src/` is the authority):

- **Untouched by the rule** — `sdk04`, `sdk16`, `sdk38`, `sdk51`, `sdk81`, `sdk89`, `sdk90`,
  `sdk91`, `sdk94`.
- **Re-derived under the rule, pending run — not evidence until run** — `sdk12`, `sdk15`, `sdk17`,
  `sdk30`, `sdk32`, `sdk34`, `sdk39`, `sdk40`, `sdk41`, `sdk42`, `sdk43`, `sdk44`, `sdk45`,
  `sdk46`, `sdk47`, `sdk48`, `sdk50`, `sdk54`, `sdk55`, `sdk58`, `sdk59`, `sdk60`, `sdk70`,
  `sdk71`, `sdk72`, `sdk74`, `sdk76`, `sdk77`, `sdk79`, `sdk80`, `sdk82`, `sdk84`, `sdk86`,
  `sdk87`, `sdk88`, `tb05`, `RGB_E2E=7`, and the concurrency chaos test `chaos22`
  (`clients/tests/rust/src/chaos22_concurrent_users.rs`, oracle in `chaos22_oracle.rs`, cheats
  re-derived to capture ladder state rather than a backup).
- **Deleted** — `sdk73` and `sdk78` (files gone, dispatch arms removed from `main.rs`), and the
  branch-lane `RGB_E2E=1–3, 5, 6, 8–10`. Nothing on this page may cite them.

The node-gated fee-bump tests (`live_p2a_package_rescue.rs`, `live_tower_float.rs`) skip without a
Bitcoin Core RPC endpoint.
