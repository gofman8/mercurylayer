# Old-state invalidation — the deep dive

How this system makes yesterday's owner unable to spend today's coin, what that machinery does
over days and weeks, and what it feels like to hold, receive, and exit a coin. This page is the
long-form explainer; the short comparison is [invalidation.md](invalidation.md), the shipped
ladder is specified in [PROTOCOL.md](../PROTOCOL.md) (TES-R) and
[CHILDREN.md](../CHILDREN.md) (first-class split children), the normative requirements for the
un-laddered coin shape live in INVALIDATION-SPEC (retired 2026-08-15), fee/size tables in
invalidation-economics (retired 2026-08-15), and exit mechanics in
[exits.md](exits.md). Audience: developers, integrators, and researchers who have not read the
code. Every number below comes from the code paths and tests cited; where behaviour is an open
item we say so rather than round it off — see AUDIT-2026-07 (retired 2026-08-15) and
PROTOCOL.md §6's residual-risk list. Who trusts whom — and what is verified instead of trusted —
is mapped party-by-party in [TRUST-MODEL.md](../TRUST-MODEL.md).

## One protocol, two coin shapes

There is exactly **one protocol**. `claim()` establishes a TES-R ladder — trigger `T` → extension
`X_m` → state `S_k`, all **relative-CSV** and all **un-broadcast** — for every fresh confirmed
**root** coin, unconditionally. The `deposit_protocol_version` field and the
`UTEXO_PROTOCOL_DEFAULT` escape hatch that could opt a deposit out of it are **deleted**, and no
test pins an older lane. But not every coin is laddered, and that is a design decision, not a
leftover:

- **LADDERED** — every plain BTC deposit. Old state is outranked by **relative** timelock ordering
  (each transfer co-signs a state one δ lower), backed by the receiver's disclosure census and a
  keyless watchtower. An idle laddered coin **never ages**: it has no calendar deadline and costs
  **0 vB** of on-chain rent, forever.
- **UN-LADDERED** — an **RGB carrier** is deliberately never laddered, because a plain tier spend
  would destroy its allocation (terminal-freeze, [PROTOCOL.md §5.10](../PROTOCOL.md); `sdk52`), and
  a **split sub-coin** whose funding output is still un-broadcast has no on-chain outpoint to root
  a trigger [B0]. These coins keep the signed-once **absolute-locktime** backup and transfer by
  backup-chain handover. They keep a real calendar deadline with them. This shape is **load-bearing
  for RGB tokens** — current, not deprecated (`sdk52`, `sdk39`, `sdk32`, `sdk34`).

Almost every mechanic below exists in both shapes but in a different key. Each one is labelled.

Terminology follows the
[INVALIDATION-SPEC (retired 2026-08-15) §0 table](../INVALIDATION-SPEC (retired 2026-08-15)#0-scope-terminology-relationship-to-specmd);
the words used most: a **flat coin**'s funding output is on-chain, a **sub-coin**'s funding tx is
pre-signed but un-broadcast (*materializing* the branch broadcasts it, turning the sub-coin flat),
and a **child** is the piece minted by an in-ladder split.

Contents: [the problem](#1-the-problem-from-first-principles)
· [why the deadline went away](#1b-why-the-deadline-went-away--and-where-one-still-lives)
· [four layers](#2-the-four-defence-layers)
· [walkthroughs](#3-lifecycle-walkthroughs) · [over time](#4-over-time-what-a-coin-actually-costs-to-hold)
· [real-world situations](#5-real-world-situations) · [UX](#6-the-ux-perspective) · [FAQ](#7-faq)
· [recap](#8-comparison-recap-over-time-behaviour).

---

## 1. The problem, from first principles

An off-chain transfer changes who owns a UTXO without touching the chain. The chain therefore
cannot referee: at any moment there may be several *mutually exclusive*, *individually valid*
descriptions of who owns the coin — the current one, and every state the coin passed through on
the way. Each past owner once held (and may have kept) a pre-signed transaction that pays the
coin to *them*. If nothing distinguishes old state from new state, the first past owner to reach
the chain wins, and off-chain "ownership" means nothing. Old-state invalidation is the set of
mechanisms that make the newest state win — cryptographically, economically, or temporally —
against every stale copy in every past owner's backup folder.

The design space is small and every deployed L2 picks from it: **expiry** (old state simply dies
after a window — Ark/Second's round expiry; cheap, but missing the window forfeits funds to the
operator), **revocation** (handing over a secret makes old state punishable — Lightning; strong,
but requires per-counterparty state and constant watching), **decrementing locks** (every new
state carries a lower timelock, so the newest matures first and wins any honest race — Spark's
relative ladder, SuperScalar's nSequence counters, and this system's own pre-TES-R absolute ladder;
simple and trustless), and **operator refusal** (a semi-trusted co-signer simply refuses to sign
conflicting or expired state — every statechain; instant and race-free, but only as strong as the
operator's honesty).

This system takes decrementing locks as the trustless floor and layers operator refusal,
receiver-side verification, and watching on top — four layers, each covering the failure mode of
the one below. What changed with TES-R is *which kind* of decrementing lock: **relative** (BIP-68/
BIP-112 CSV) instead of **absolute** (nLockTime). That one substitution is why a coin can now sit
still forever without a deadline, and §1b is the whole argument.

## 1b. Why the deadline went away — and where one still lives

The single most-asked question about the old design was *why does a ~7-day timeout exist at all?*
The answer to that question is also the answer to why it no longer does. Here is the chain of
reasoning, step by step, with nothing assumed:

1. **Bitcoin cannot revoke a signature.** When a coin changes owner off-chain, every previous
   owner keeps their old pre-signed transaction — cryptographically valid forever. No Bitcoin
   opcode deletes, expires, or punishes an old signed transaction. (Lightning fakes revocation
   with penalty keys; that requires per-counterparty state and constant watching, and grows the
   collusion surface — rejected here, see §1.)
2. **So old states can only be *outranked*, never destroyed.** The only tool Bitcoin gives us to
   rank mutually-exclusive pre-signed transactions in time is the timelock, and ranking requires
   strictly decreasing timelocks: each handover must give the new owner a lock that matures
   strictly *before* anything the sender kept.
3. **With an *absolute* lock, a decreasing sequence needs a finite starting point.** A first backup
   cannot lock at infinity, because the depositor too must be able to exit unilaterally in bounded
   time. That finite start was `initlock` — and it is where every property people called "the 7
   days" came from: the unilateral-exit wait ceiling, the hop budget, the off-chain lifetime, the
   receiver's materialization deadline.
4. **The fatal part is that an absolute lock ticks while un-broadcast.** The clock runs on the
   calendar whether or not anyone is attacking, so the defence has to be renewed on the calendar:
   one on-chain re-anchor per coin per horizon, **whether or not the coin ever moves**. Measured:
   112 vB per coin per ~1,000 blocks ≈ **5,840 vB per coin-year of pure idle rent**; one million
   coins ≈ 11% of all Bitcoin block space; ten million is physically impossible
   ([PROTOCOL.md §2](../PROTOCOL.md)). No amount of delegation fixes that — it changes who *pays*,
   not how much chain space is *burned*.
5. **A *relative* lock does not tick until its parent confirms** (BIP-112). Put the ordering on
   relative locks and the clock starts on **attack**, not on deposit. That is TES-R:

   ```
   F   on-chain funding UTXO (P2TR key-path, aggregate A = KeyAgg(P_user, P_SE))
   └─ T    TRIGGER    v3/TRUC, NO timelock, signed ONCE at deposit, never re-signed
      │                out[0] → the coin's own tweaked aggregate   out[1] → 240-sat P2A anchor
      ├─ X_0 … X_m   EXTENSIONS   mutually exclusive spends of T.out[0]
      │                X_m: input nSequence = relative CSV E_m = E0 − m·δE
      │                re-signed OFF-CHAIN at each renewal; a lower CSV replaces the old one
      └─ (on X_m.out[0])  S_0 … S_k   STATES / SP splits / CB combines
                       S_k: input nSequence = relative CSV Δ_k = D0 − k·δ
                       one δ LOWER at every transfer — the new owner matures first
   ```

   All three tiers are pre-signed and **un-broadcast**. `T` carries no timelock at all, and every
   CSV below it counts only from the confirmation of its parent. Therefore **nothing anywhere
   matures until somebody broadcasts `T` on-chain**. An idle coin — and an entire idle split DAG —
   never ages. There is no calendar deadline and **0 vB of idle rent**
   (`sdk40` PART 1 proves BIP-68 is enforced by real consensus and that un-broadcast material does
   not age; `sdk43` runs a coin through unbounded off-chain renewal and rollover with the funding
   outpoint untouched throughout).
6. **The ordering property survives intact.** A transfer co-signs a fresh state one δ lower than
   the one it replaces (replace-by-lower-timelock, Decker–Wattenhofer at a single dedicated tier),
   so the new owner's state always matures **first**; the state it replaces is disclosed to the
   receiver as *superseded* and counted (§2, layer 3). Renewal does the same thing horizontally at
   the extension tier: `X_{m+1}` strictly undercuts every older extension in the race for
   `T.out[0]`, so every pre-renewal state hangs on a parent that can now never confirm (`sdk40`
   PART 2 kills a stale ladder outright at the consensus level).
7. **What replaces "the 7 days".** Three numbers, none of them a calendar:
   - **the alarm** — the only way to start any clock is to broadcast `T`, which spends the funding
     outpoint `F` and is therefore **publicly visible on-chain**. A watchtower subscribes to one
     outpoint per coin and does nothing until that happens;
   - **the notice** — after `T` confirms, the newest extension needs `E_m` confirmations and the
     newest state another `Δ_k` before *anything* is final. That is ≥ 288 blocks (~2 days) of
     defender notice in the shipped schedule, and never less than 144 blocks (~1 day) for any one
     tier;
   - **the head start** — δ = 36 blocks (~6 h) per hop at the state tier, δE = 36 at the extension
     tier. That is the margin by which the honest owner's transaction outruns the newest stale
     rival, and it must absorb a plausible reorg plus watchtower reaction time.
8. **What it costs, honestly.** Three things. (a) **No unconditional no-watch window**: the old
   design gave a received coin a period in which *the chain itself* rejected every stale backup and
   nobody had to be awake; TES-R replaces that with perpetual — but alarm-driven, keyless, delegable
   — watching (PROTOCOL.md R-2). (b) **Longer worst-case unilateral latency**: a fresh flat coin's
   unilateral exit walks `T → X → S` and waits `E_m + Δ_k` sequentially — worst 2,160 blocks
   (≈ 15 days), decreasing 36 blocks per hop; deeper for sub-coins (§5.2, PROTOCOL.md §5.9).
   (c) **Invalidation is race-conditional, not axiomatic**: "consensus-dead" means the newest
   extension must win a ≥ 36-block-edge race after a public trigger. Strictly stronger than the old
   design's post-window position, and categorically stronger than a key-deletion promise — but a
   race, not a theorem.
9. **Where a calendar deadline still lives.** On the **un-laddered** shape. An RGB carrier must
   never be laddered (rule 1 of the terminal-freeze design: a plain tier spend destroys the
   allocation), and a sub-coin over un-broadcast funding cannot root a trigger. Those coins keep the
   signed-once absolute-locktime backup, so a *received* one still owes exactly one materialization
   before the deposit-anchored deadline `H_deposit + initlock`. That machinery — `exit_deadline_block`,
   `auto_exit_due`, the margin arithmetic — is retained and default-on for precisely this shape
   (§4, §5.1; `sdk34` materializes a carrier before its deadline, `sdk32` documents what the
   residual clawback window looks like if nobody ever acts).

**Is the trigger itself dangerous?** Only someone holding a copy of `T` can start a coin's clock,
and copies of `T` travel with the coin: the owner has one, and so does every *previous* owner. A
self-deposited coin that has never been transferred therefore has **no one who can trigger it**.
When a past owner (or a griefer) does trigger, the answer is not a race but a **cooperative
de-trigger**: `T.out[0]` pays the coin's own aggregate, so the owner and the SE simply key-path-spend
it into a fresh funding output and rebuild the ladder — one ~111-vB transaction with no timelock,
confirmed unopposed inside the ≥ 144-block window during which no adversary transaction is even
valid. The griefer pays ~276 vB to cost the victim ~111 vB (damage:cost ≈ 0.4 — griefing is
economically losing), and the victim keeps their coin off-chain (`sdk40` PART 2). The de-trigger
needs the SE, so it is a cost shield, never a safety dependency — the unilateral tree always exists
without anyone.

## 2. The four defence layers

**Laddered parameters** (`TesrParams::mainnet()` in `lib/src/tesr.rs`, served via `/info/config`
and published in the SE's signed nostr record so a receiver can detect a per-victim parameter
split; the arithmetic is pinned by `sdk44`):

| Parameter | Value | Meaning |
|---|---|---|
| `D0` / `δ` / `D_floor` | 1,440 / **36** / 144 blocks | state tier: 36 hops per epoch, ~6 h head start per hop |
| `E0` / `δE` / `E_floor` | 720 / **36** / 144 blocks | extension tier: 16 usable epochs, forced rollover at `m = 15` |
| Hop budget per depth level | 36 × 16 = **576 transfers** | between depth increments (§4) |
| Worst fresh unilateral wait | `E0 + D0` = 2,160 blocks ≈ **15 days** | decreasing 36 blocks per hop/renewal |
| Committed fee rate | 2 sat/vB per tier | plus a 240-sat P2A anchor on every tier |

A `TesrParams::regtest()` preset (24/6/6, 12/3/3, `m_max` 2) exists only so a full lifecycle fits
inside a test's mining budget. δ = 36 rather than a shorter value because both economics reviews
flagged the head start as the single parameter everything stands on, and mainnet has sustained
> 4 h full-block spikes; the budget sensitivity is stated plainly in PROTOCOL.md §5.2
(δ = 24 → 1,350 hops/level, **36 → 576**, 72 → 162, 144 → 45). Because rollover is off-chain,
a conservative δ trades exit weight only — never chain rent.

**Un-laddered parameters** (the absolute ladder; clients read the pair from `GET /info/config` as
`initlock`/`interval` and MUST NOT hard-code them — IVL-REQ-1):

| Profile | `initlock` | `interval` | Hop capacity | Wall-clock horizon | Exclusive window per hop |
|---|---|---|---|---|---|
| Deployed (`server/Settings.toml`) | 1,000 | 10 | 100 | ≈ 6.9 days | 10 blocks ≈ 100 min |
| Code defaults (`server_config.rs`) | 10,000 | 100 | 100 | ≈ 69 days | 100 blocks ≈ 17 h |

---

**Layer 1 — timelock ordering.** One idea, two mechanisms.

*Laddered (`lib/src/tesr.rs`).* Every owner holds the whole pre-signed tier chain. A transfer
co-signs a new state at `Δ_{k+1} = Δ_k − 36`; a renewal co-signs a new extension at
`E_{m+1} = E_m − 36`. Because every lock is relative and every tier is un-broadcast, none of them
counts down while the coin rests. When someone does broadcast `T`, the tiers become a strict
ordering: the current owner's state matures ≥ 36 blocks (~6 h) before the newest stale one, and a
stale *epoch*'s states hang on an extension that can never confirm at all. Evidence: `sdk40`
(consensus enforcement and stale-ladder death), `sdk41` (after Alice pays Bob, Bob's lower-CSV
state wins the exit race and Alice cannot claw back), `sdk51` (a hostile trigger, defended
end-to-end by a watchtower pass), `sdk50` (the full unilateral walk).

*Un-laddered (`lib/src/transaction.rs`, `calculate_block_height`).* Every owner holds one
pre-signed **backup transaction** spending the coin's 2-of-2 funding output to their own address,
with an **absolute** nLockTime anchored at the deposit: the depositor's backup locks at
`H + initlock`, where `H` is the chain tip when the SE first co-signs the deposit backup — at
deposit *detection*, in practice ≈ the confirmation height (IVL-INV-10 keeps the small
co-sign→confirmation gap explicit) — and each transfer hands the new owner a backup locking
`interval` blocks *lower*. After `k` hops the current owner holds `L_k = H + initlock − k·interval`,
the lowest locktime in existence for that coin. While the tip is below `L_k`, *no* pre-signed
transaction is final: the chain rejects them all and the only spend path is a fresh SE
co-signature. From `L_k` to `L_{k−1}` the current owner enjoys an **exclusive exit window** of
`interval` blocks in which their backup is the only final transaction on Earth for this coin. After
that, stale backups mature one by one (newest first) and honesty degrades to a first-seen broadcast
race — one the current owner has already had a head start in.

**Layer 2 — SE hard refusal** (`server/src/endpoints/sign.rs`). The statechain entity refuses to
co-sign at all when:

- a `single_use` coin already has one finalized signature (HTTP 410, ERR-1);
- a coin's `epoch_deadline` (unix seconds) has passed (410, ERR-2 — §5.8);
- a coin's `sig_budget` is exhausted (410, ERR-3 — "terminal node");
- **a transfer of the coin is currently open** (409 Conflict — the *pending-transfer lock*).

All of these **fail closed**: any database error yields 503 and no signature (audit [1]). The
budget is monotonic — `set_spend_budget` takes `min(existing, finalized + remaining)`, so a
terminal node can never be un-terminated (INV-24) — and terminal status is **publicly auditable**
via `GET /statechain/spend_budget/<id>`. Independently, the enclave consumes each signing nonce
atomically, so the SE cannot be tricked into signing two different messages under one nonce
(single-active-state, INV-23/ERR-12, proven by `sdk12`).

The pending-transfer lock is new and load-bearing (see [CHILDREN.md](../CHILDREN.md)): once
`/transfer/sender` opens a transfer, the SE refuses every further co-sign on that statechain id
until the receiver completes it or it times out, and refuses to re-address an open transfer to a
different recipient. It closes the window in which a still-owner sender could co-sign a *lower*-CSV
rival after the receiver had already checked everything. It is releasable, not monotonic, so it
never fights the budget clamp. It is only safe because the sender's own legitimate pre-signs were
re-ordered to happen **before** the transfer is opened.

What terminality is used for now is narrower and sharper than it once was. An **in-ladder split
terminalizes the parent** (the split consumes the parent's last co-signature, so after a split even
the legitimate owner cannot get the parent co-signed again), and an **RGB carrier's ancestry is
terminal-frozen** so no colored anchor can ever be re-signed out from under an allocation. The
split **child is deliberately *not* terminalized** — the census closes any pre-conveyance rival and
the SE key handover closes every later one. The single exception is the **Lightning-latched piece**,
which sits unclaimed past the pending lock's window while the SSP settles on its own schedule, so it
is terminalized instead — a permanent lockout ([LIGHTNING.md](../LIGHTNING.md), PROTOCOL.md §5.12).

**Layer 3 — receiver-side verification** (`clients/libs/rust/src/tesr.rs`,
`clients/libs/rust/src/transfer_receiver.rs`). A receiver trusts neither sender nor SE blindly.

*Laddered — the R′ census.* The receiver rebuilds the whole tier structure from public data:
funding `F` is on-chain, unspent and pays the aggregate `A`; `T` spends `F` and carries no timelock;
each tier output pays `A` plus the public tagged tweak; the conveyed extension's nSequence is
exactly `E0 − m·δE` and the new state's exactly `D0 − (k+1)·δ` against the SE's **publicly served
counters**; and — the linchpin — **the SE's signature count equals the exact expected tree size**.
Any hidden extra co-signed state or extension shows up as a count mismatch. For a multi-hop child
the census generalizes to `se_num_sigs == flat_backups + Σ conveyed_tiers` — where `flat_backups` is
the count of signed-once backup transactions conveyed with the coin (the un-laddered shape's chain;
a laddered root coin still carries exactly one, co-signed by its deposit at claim, so the term is
never assumed zero) — summed across the ancestor chain rooted at the on-chain funding output. Each hop discloses exactly one superseded
state, so an undisclosed rival cannot hide. Evidence: `sdk46` (the census against the
*real* SE counter — it accepts the true count and rejects a hidden extra signature), `sdk47` (R′
across a transfer), `sdk54` (adversarial `verify_bundle`), `sdk58` (11 adversarial in-ladder-split
cases, all REJECT), `sdk60` and `sdk17` (the N-hop child census).

*Un-laddered — branch validation.* The receiver checks the backup ladder decrements and signature
count (REQ-16), then runs `validate_branch` (root on-chain, unspent, confirmed; every branch tx
locktime `≤ tip` — INV-4, audit [11]; value conservation Σout ≤ Σin per hop — INV-25; rejection of
any non-tree branch that consumes an outpoint more than once; full script/signature verification)
and `verify_terminal_parents` (the sender must name at least one terminal ancestor per structural
input the branch consumes — so a multi-input combine names all N — INV-20, and each must report
`terminal: true` at the SE, ERR-7). Blind-SE caveat
([SPEC.md §14](../SPEC.md#14-known-limitations-adversarial-review), TRUST-MODEL B2): ancestor *ids*
are not cryptographically bound to branch outpoints, so the count check defeats omission, not
substitution; the compensating control is that the receiver holds the full branch and can
materialize immediately. Laddered coins are structurally exempt from B2 — their ancestor chain is
key-derived from the on-chain funding output, so there is no id to substitute. Adversarial
coverage: `sdk55`.

**Layer 4 — watching.** Two duties, two shapes.

*Laddered — event-driven, no deadline.* `defend_ladders()` runs one `watch_pass` per coin. If the
funding outpoint `F` is unspent, it is a **no-op** — an un-broadcast coin never ages, so there is
nothing to defend and no routine renewal to perform. If someone has triggered the coin, the pass
races the owner's tiers, broadcasting each as its relative timelock matures; because the adopted
current state carries the strictly-lowest CSV (enforced at adoption by `verify_bundle`), it matures
first and the funds land at the owner's own key. It emits `WalletEvent::LadderDefended` per pass and
is idempotent and incremental — call it once per block from your own loop. The watch bundle carries
**zero key material**, so the duty is fully delegable, and a second independent tower is idempotent
(both asserted in `sdk45`). What is **not** implemented: package-aware broadcast. `watch_pass` and
`exit_pass` walk the tiers with per-tx broadcast and attach no P2A fee child, so spike-time
fee-bumping is specified but not shipped (PROTOCOL.md R-4, §5.4 below).

*Un-laddered — calendar-driven.* Structural branch transactions are **locktime-zero** (INV-4,
`lib/src/transaction.rs`): broadcastable *now*, they always beat any deposit-anchored stale
backup — provided they reach the chain before the earliest stale ancestor backup matures.
`estimate_exit_cost` surfaces that bound as `exit_deadline_block = H_deposit_root + initlock`
(`rust-sdk/src/wallet.rs`, audit [10] fix; it returns `None` for a coin with no branch), and
`auto_exit_due(margin_blocks)` acts on it: it force-exits plain off-chain sub-coins and
**materializes** received token carriers (branch only — a plain sweep would destroy the allocation),
emitting `ExitDeadlineApproaching` / `TokenCarrierMaterialized`. It runs from the background watcher
every poll by default (`SdkConfig::auto_exit`, margin `auto_exit_margin_blocks` = 288). Meanwhile
`unilateral_exit` broadcasts branch-first, reports remaining `wait_blocks` instead of failing, and
raises `WalletEvent::ExitBranchConflict` if someone is racing the branch in the mempool (REQ-25).
The deadline's residual imprecision is quantified in §4.

## 3. Lifecycle walkthroughs

### 3a. A laddered coin, deposited and transferred three times

Alice deposits; her funding tx confirms and `claim()` establishes the ladder, emitting
`LadderEstablished`. She then pays Bob, Bob pays Carol, Carol pays Dave — all off-chain, minutes
apart, none of them touching the chain. Nothing is broadcast; nothing has started counting.

| State | Holder | Relative CSV on `X_0.out[0]` | Status after hop 3 |
|---|---|---|---|
| `S_0` (deposit state) | Alice | 1,440 | superseded, disclosed |
| `S_1` | Bob | 1,404 | superseded, disclosed |
| `S_2` | Carol | 1,368 | superseded, disclosed |
| `S_3` | Dave | **1,332** | current — lowest |

All four states are mutually-exclusive spends of the same output, and all four are inert. The
funding outpoint `F` is untouched, the trigger `T` is un-broadcast, and the extension `X_0` (CSV
`E_0 = 720`) is un-broadcast too. The chain contains exactly one transaction for this coin: Alice's
original deposit. **Nothing about this picture changes with time.** Wait a day, a month, or three
years, and the table is identical.

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

(Every relative lock counts from its own parent's confirmation: `E_0 = 720` from `T`, then each
state's `Δ` from `X_0`.)

At each boundary:

- **before `T` is even broadcast** — indefinitely, at zero cost, with no deadline and no watching
  action required. There is no transaction anyone could send that the chain would accept.
- **`T` confirms** — the alarm. `defend_ladders()` sees `F` spent and switches from no-op to active.
  The preferred response is not a race at all: Dave and the SE **cooperatively de-trigger**,
  key-path-spending `T.out[0]` into a fresh funding output inside the 720-block window during which
  no pre-signed extension is valid. Bob has bought Dave a ~111-vB re-anchor and bought himself
  nothing.
- **`T` + 720** — if the SE is unavailable and the de-trigger cannot happen, `X_0` is broadcast.
  Note that Bob has to wait exactly as long: the extension is *shared*, it is not per-owner.
- **`X_0` + 1,332** — Dave's `S_3` is spendable, and it is the only spendable state on Earth for
  this coin for the next 36 blocks. Bob's `S_1` cannot confirm until `X_0` + 1,404. Dave (or his
  tower, keylessly) broadcasts and the coin settles at Dave's own key.

What each hop actually consumes is a *decrement*, not lifetime: the current owner's edge over the
previous one stays a constant 36 blocks no matter how many hops have happened, while the number of
decrements left before `D_floor` shrinks by one. When the next decrement would breach the floor, the
SDK renews the extension off-chain and the state tier starts again at `D0` (§4). The calendar plays
no part anywhere in this walkthrough. Evidence: `sdk41`
(exactly this race, run for real), `sdk40` (the consensus properties underneath it), `sdk51` (the
watchtower response), `sdk42` (the whole lifecycle including persistence and reload).

### 3b. A partial payment: the in-ladder split

Alice holds a laddered 50,000-sat coin and owes Bob exactly 20,000 sats. She cannot hand over a
whole coin, so she splits — and the shape of that split is the single most safety-critical detail
in the system.

1. The SDK checks admission **before touching anything**: each child must clear
   `min_child_value(fee_rate, dust)` = `2·(committed_fee + 240) + 330` = **1,306 sats at 2 sat/vB**.
   That is not a dust check — a child is not a bare output, it is a coin that must fund its **own**
   extension and state tier (each burning a committed fee plus a 240-sat P2A anchor) and still clear
   dust at the end. Both 20,000 and the change clear it comfortably.
2. The SDK sets the parent's spend budget to `finalized + 1`, then co-signs the **split state `SP`**.
   Crucially, `SP` spends **`X_m.out[0]`** — it is a *state tier*, a **descendant of the trigger**,
   at nSequence `Δ_{k+1}` (one decrement below the state Alice retains). It is **not** a rival spend
   of the funding outpoint `F`. That is the whole game: a past owner's retained no-timelock trigger
   has nothing to race, because the split does not compete for `F`. `SP` pays two exact resting
   outputs (Σout = Σin − committed fee) plus its P2A anchor, and is **un-broadcast** like everything
   else.
3. `establish_child` hangs each child's own extension + state tiers off its resting output — no
   trigger needed, because `SP` is itself un-broadcast, so nothing below it ticks until `SP`
   confirms.
4. The split consumed the parent's last co-signature: the parent is **terminal** at the SE. No later
   withdraw, transfer, or fresh state on it can ever be signed, for anyone;
   `GET /statechain/spend_budget/<parent>` shows `terminal: true` to the world.
5. Alice conveys the child bundle **together with the key-handover material** (`x1` from
   `get_new_x1`, `t1`/`transfer_signature`, and the ancestor chain `F → T → X_m → SP` so Bob can
   validate over un-broadcast funding). She does **not** terminalize the child.
6. Bob claims: `verify_child_bundle` (parent `F` on-chain via the ancestor chain, exact-equality
   census), then he **completes the key handover** — the SE rotates its share so that
   `A_child` is *invariant* (`sender_share + SE_old == receiver_share + SE_new`), which is exactly
   what keeps the pre-signed child exit chain valid, and re-points `auth` to Bob. Alice is now
   **permanently locked out**.

Stale-state inventory after the split: Alice's superseded state, disclosed and counted, at a CSV
36 blocks *above* the one that funds Bob's child. Nothing is on-chain, nothing has a deadline, and
the funding outpoint is still unspent.

**The child is first-class, not an exit-only claim.** Bob can pay it onward off-chain — whole via
`child_retransfer`, or split again via `child_in_ladder_pay` (a depth-2 ancestors chain). Each hop
costs exactly **one co-signature** and discloses exactly **one superseded state**, which the next
receiver's census counts and proves out-raced. `sdk60` runs alice → bob → carol with the funding
outpoint **unspent throughout** — only Carol's exit ever touches the chain; `sdk17` runs a partial
second hop. Attack coverage for the split itself: `sdk58` (11 adversarial cases, all REJECT) and
`sdk59` (the end-to-end payment).

*A defect worth knowing about, because it shows where the sharp edge is (D1, fixed).* The admission
guard originally used the old backup-fee floor (442 sats). A piece in the 442..1,306 window
therefore terminalized the parent and **then** failed, stranding a perfectly good coin as
unilateral-exit-only. The guard now takes `max(min_split_output, min_child_value)` and refuses **up
front**. No funds were ever at risk; the cooperative path was.

#### The same payment on the un-laddered shape

Colored (RGB) splits and any split of a sub-coin whose funding is un-broadcast still travel the
older machinery, and this is where the locktime-0 branch lives. Alice deposits 50,000 sats at
**H = 100,000** (backup `L₀` = 101,000, never transferred). At height **100,300** she splits:

1. Parent budget set to `finalized + 1`, then the **split tx** is co-signed: one input (the deposit
   outpoint), two outputs — 20,000 to a fresh 2-of-2 for Bob's piece, 29,500 change to a fresh 2-of-2
   for Alice (fee reserve `(50,000/100).clamp(300, 2000) = 500` sats stays behind as the split tx's
   miner fee). The split tx has **locktime 0** and is *not broadcast*.
2. The parent is terminal, publicly.
3. Each sub-coin gets a **fresh first backup** at split time: locktime `100,300 + 1,000 = 101,300`.
   Depth does not consume ladder.
4. Bob verifies branch linkage root-first; the root input is on-chain, unspent, confirmed; the split
   tx's locktime is 0 ≤ tip; no value created; scripts verify; ≥ 1 terminal ancestor named and
   reporting `terminal: true`.

Stale-state inventory: exactly one item — Alice's parent deposit backup at **101,000**. It is
locktimed; the branch is not. Bob's `exit_deadline_block` is `H + initlock = 101,000`, and here it
is **exact** — up to the small deposit co-sign→confirmation gap (IVL-INV-10) — because the parent
was never transferred before the split. If Bob ever distrusts anything, he broadcasts the 155-vB
split tx (fee already committed) and waits out his own fresh ladder.

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

### 3c. Depth: off-chain rollover, child chains, and the un-laddered tree

**Laddered: depth is bought off-chain and bounded by policy.** When the next state's CSV would fall
below `D_floor`, the SDK renews inside `transfer()`: two blind co-signs mint `X_{m+1}` (CSV
`E0 − (m+1)·δE`) and the transfer's own state `S'_0` on top of it — **zero on-chain bytes**, and the
SE refuses the renewal unless the counter is genuinely within 2δ of the floor, which kills both the
replay grief and the arbitrary-epoch-burn lever. When the extension tier itself exhausts (`m = 15`),
the SDK performs an off-chain **self-split rollover**: a 1-in-1-out `SP_roll` consuming the current
state slot, whose child resting output hosts fresh extension + state tiers and a fresh 576-hop
budget. Cost: zero on-chain, +2 pre-signed txs (~248 vB) of *contingent* exit weight, +1 depth level.
`sdk43` drives renew → rollover → renew past epoch exhaustion, unattended, with the funding outpoint
untouched, and then exits unilaterally through the whole deep chain.

Depth costs exit *latency*, not lifetime: a depth-`d` sub-coin's unilateral exit is `3 + 2d`
transactions ≈ 124·(3+2d) vB, with a worst-case wait ≤ `(d+1)·2,160` blocks (depth-3 ≈ 60 days;
cooperative exit remains instant at any depth). The default SDK policy caps depth at 3: the next
transfer past the cap triggers a **solo compaction** — exactly one re-anchor, 112 vB,
non-interactive — rebearing the coin at depth 0 and priced into that transfer's fee. Net budget
between chain touches: 576 × 4 ≈ **2,300 transfers per 112 vB**, and a user who tolerates depth can
raise the cap and touch the chain never. (An optional per-level geometrically shrinking `E0`/`D0`
schedule would bound the total worst wait below ~30 days at any depth, trading per-level hop budget;
it is a shipped dial, default off — PROTOCOL.md open problem O-4.)

**Un-laddered: depth is a tree, and the deadline is the minimum over its ancestors.** Same deposit,
**H = 100,000**, splits at heights 100,050 (parent → A + change), 100,120 (A → B + change), 100,200
(B → leaves), nothing transferred in between:

| Node | Created | Terminal at SE? | Its (now stale) backup locktime | Held by |
|---|---|---|---|---|
| Root parent (deposit) | 100,000 | yes (split 1) | 101,000 | original owner |
| A (split-1 piece) | 100,050 | yes (split 2) | 101,050 | whoever split A |
| B (split-2 piece) | 100,120 | yes (split 3) | 101,120 | whoever split B |
| Leaves (split-3 outputs) | 100,200 | no — live coins | 101,200 (current, not stale) | current owners |

Branch to exit a leaf: 3 pre-signed locktime-0 txs (~155 vB each, fees pre-committed) + the 112-vB
leaf backup ≈ 577 vB total.

The earliest hostile maturity across *all* stale ancestors is `min(101,000; 101,050; 101,120) =`
**101,000** — the root's deposit backup. Because every fresh sub-ladder anchors at its own later
split height, *and nothing here was transferred between splits*, the root deadline is the minimum:
**one number, `H_deposit + initlock`, bounds the entire tree**, however deep and however bushy. That
"nothing transferred in between" is load-bearing: an intermediate ancestor transferred `k` times
*before* its split retains a backup at its own anchor `+ initlock − k·interval`, which undercuts the
root's deposit backup as soon as `k·interval` exceeds the split-height gap — the true deadline is the
minimum over **every** ancestor's retained backups, not the root's alone (IVL-INV-10; audit
[10]/[17]). Everything below it lives or dies by whether its branch reaches the chain before that
minimum — quantified next. A colored (token) tree is exactly this shape; `sdk39` materializes a
token piece two colored splits deep with its allocation intact.

<a id="4-over-time-the-ladder-as-a-consumable-budget"></a>

## 4. Over time: what a coin actually costs to hold

**Laddered: nothing, and there is no clock to run out.** This is the single biggest change in the
system and it is worth stating without hedging. A laddered coin that nobody touches:

- consumes **0 vB per year** of block space, forever;
- has **no deadline** — no expiry, no floor, no horizon, no "act before" height;
- requires **no renewal traffic**, no refresh, no on-chain touch of any kind;
- exposes its holder to nothing, because no pre-signed transaction anywhere in the system can become
  valid until someone spends `F` in public and then waits out ≥ 288 blocks of relative timelocks.

What *is* finite is the **hop budget**, and it is finite per depth level, not per coin: 36 state
decrements per epoch × 16 epochs = **576 transfers** before the SDK adds a depth level by rolling
over — off-chain, automatically, inside `transfer()`. The only thing that ever costs chain space is
the optional depth cap: one 112-vB solo compaction per ~2,300 transfers ≈ **0.05 vB per transfer**,
paid inside the transfer that triggers it. Footprint scales with *activity*, and only activity.

**What "the coin is dying" looks like on a laddered coin:** it doesn't. There is no number trending
toward zero that a wallet needs to alarm on. `needs_renewal(k)` and `needs_rollover(m)` are internal
scheduling predicates the SDK consults during a transfer; a user never sees them and never acts on
them.

**Un-laddered: the calendar is real, and this is where the old arithmetic still applies.** An RGB
carrier and a sub-coin over un-broadcast funding both rest on an absolute-locktime backup anchored
at the root deposit. A *received* one must reach the chain before the earliest stale ancestor backup
matures. The extension options, with real costs:

| Option | What it extends | What it does NOT extend | Cost |
|---|---|---|---|
| **Refresh (re-anchor)** — `refresh` / `refresh_sponsored` | **Everything** — the coin moves to a brand-new funding outpoint with a fresh ladder; the old outpoint is spent so all old backups die | — | **1 on-chain tx** (~112 vB SE-co-signed spend → fresh aggregate) + a deposit-token slot. Fee **user-paid** (coin = amount − fee) or **operator-paid** (a funded sponsor rebates off-chain, keeping your total whole). Cooperative (needs the SE). `sdk30` |
| Self-split (split to yourself) | The *leaf* ladder: the new sub-coin anchors at `H_split + initlock`, fresh 100-hop budget | The **root deadline** `H_deposit + initlock` — unchanged, still bounds the whole tree | One SE co-sign; 300–2,000 sats fee reserve locked into the branch; +155 vB of future exit weight |
| Materialize the branch | Puts the sub-coin's funding on-chain; the coin becomes flat and its **own** ladder governs — the root deadline no longer applies | Its own ladder (already ticking since the split) | Branch miner fees (pre-committed reserves; N×~155 vB confirm now) |
| Cooperative withdraw + redeposit | Everything | — | 2 on-chain txs — **refresh supersedes this** (same result in one tx) |

**The deadline, exactly.** `exit_deadline_block = H_deposit_root + initlock` (deposit-anchored,
audit [10]) is a *safe, early* bound over ancestor maturities in one case and **too late** in
another:

- **Exact** — up to the deposit co-sign→confirmation gap (IVL-INV-10) — when the split parent was
  never transferred: its only stale backup is the deposit backup at `H + initlock`.
- **Too late by `k·interval`** when the parent was transferred `k` times before the split: the
  splitter's own retained backup matures at `H + initlock − k·interval`. Example, deployed profile:
  parent transferred **20 times** then split — the true deadline is `H + 800`, but the wallet reports
  `H + 1,000`: **overstated by 200 blocks ≈ 33 hours**. A receiver who timed an exit to the reported
  number could be clawed back inside that gap. In a deep tree the same understatement applies per
  ancestor: the true deadline is the minimum over **every** ancestor's retained backups, not just the
  immediate parent's (IVL-INV-10, §3c).

This is **audit item [17]**, and its status is: *scoped and half-closed*. It has **no laddered
analogue at all** — a laddered coin reports no `exit_deadline_block`, because there is no absolute
deadline to be late about — so what remains is a residual of the un-laddered shape only
(TRUST-MODEL B6). The shipped half: `UtexoWallet::auto_exit_due(margin_blocks)` force-exits any
owned off-chain plain sub-coin and materializes token carriers within `margin_blocks` of the
deadline, emitting `ExitDeadlineApproaching` / `TokenCarrierMaterialized`; the background watcher
runs it every poll by default. The still-open half: the transfer message does not convey ancestor
backup locktimes, so the receiver cannot compute the true minimum locally — the deadline
`auto_exit_due` acts on is the deposit-anchored upper bound. An **online** receiver is always safe
(the branch is locktime-free — broadcast it and the race is over), but an owner offline past the
*true* deadline has no defence. So: either broadcast the branch promptly on receipt, or run
`auto_exit_due` with `margin_blocks ≥ M·interval + slack` for an *assumed* upper bound `M` on any
ancestor's pre-split hop count — `k` itself is exactly what a receiver cannot observe. **Practical
defaults**: the SDK ships `auto_exit_margin_blocks = 288` (~2 days), which on the deployed 1,000/10
profile covers `M ≤ 14` pre-split hops plus a day of confirmation/congestion/reorg slack; a tower on
the 10,000/100 defaults profile covering the same `M` would need `≥ 1,544`. If no bound on `M` is
justifiable, fall back to eager broadcast (normative: IVL-REQ-16 in
INVALIDATION-SPEC (retired 2026-08-15)).

## 5. Real-world situations

### 5.1 Receiver goes offline for N days

**Laddered coin (the ordinary case): nothing happens, for any N.** There is no deadline to miss and
no maturity to sleep through, because no clock has started. What the offline period *does* cost is
**reaction time**: if a past owner triggers the coin while you are away, someone has to run
`defend_ladders()` (or a delegated tower has to) within the notice window — ≥ 288 blocks (~2 days)
before *any* hostile transaction is final, and ≥ 36 blocks of head start at each tier after that. So
the honest statement is: an offline laddered holder is safe unless (a) somebody triggers, **and**
(b) no tower of theirs wakes up for ~2 days, **and** (c) the trigger came from a party actually
holding a stale state. Since the alarm is a public on-chain event and the bundle is keyless, running
two or three independent towers reduces this to a liveness question about towers, not about you.
This is PROTOCOL.md's residual **R-2**, accepted deliberately as the price of deleting the rent: a
downgrade in kind from an unconditional no-watch window, in exchange for zero idle footprint.

**Un-laddered sub-coin or received carrier: the calendar is real.** The danger height is the root
deadline (§4), anchored at the *root deposit*, not at the moment of receipt — a sub-coin whose root
was deposited 5 days ago has ~2 days of margin left under the deployed profile no matter when you
received it. Offline within the margin: nothing happens; locktimes hold everyone off. Offline past
the *true* deadline (up to `k·interval` earlier than the reported one — audit [17]): a stale ancestor
backup becomes final and the sender-side holder can claw back the shared root while your
locktime-free branch sits unbroadcast. **Outcome:** safe if N days < remaining root margin; at risk
beyond it. If you plan to be offline, broadcast the branch first (it costs only the pre-committed
reserves), or leave `auto_exit` on with a wallet that stays alive, or don't hold un-laddered
sub-coins. One benign wrinkle: any co-descendant of the same tree (say, the splitter holding the
change sub-coin) can broadcast the *shared* branch txs at any time — that materializes your funding
output without your involvement and only improves your position. It is not a conflict:
`ExitBranchConflict` means a *different* transaction spending the same input; rebroadcasts of the
identical branch tx are tolerated (IVL-REQ-13).

### 5.2 SE goes down permanently — the day it happens, and a year later

The SE's death removes the *cooperative* paths only; every unilateral path is pre-signed and needs
nobody — with one onboarding boundary.

**The onboarding window.** The trigger `T` is signed once, when the SE's watcher first sees your
funding tx (`clients/libs/rust/src/coin_status.rs`), and the same is true of the first backup on the
un-laddered shape. Between broadcasting the funding tx and that co-sign you have **no** unilateral
path at all: an SE that dies inside that window strands the deposit in the 2-of-2 permanently. Fund
only after deposit init succeeds, and treat a missing `LadderEstablished` / `DepositConfirmed` as a
reason to stop funding. (TRUST-MODEL B5; `sdk16` covers the fresh-user onboarding path.)

**Once the ladder exists**, an SE that never returns costs you latency, never funds — and the
latency does **not** grow with how long the SE has been dead, because nothing has been ageing.
`unilateral_exit` runs `exit_pass`, which is idempotent and incremental: it broadcasts `T`, reports
`complete: false` with the blocks remaining until the next tier matures, and advances one tier per
call as the chain moves. Call it once per block (or let the background loop do it). Total wait:
`E_m + Δ_k` sequentially — worst 2,160 blocks (≈ 15 days) for a never-transferred coin, decreasing
36 blocks per hop and per renewal, and 3 + 2d transactions for a depth-`d` sub-coin. `sdk50` drives
the whole walk end-to-end; `sdk45` drives it from a **keyless** watch bundle, which is the same path
a delegated tower takes on behalf of an owner who never wakes up. Note what is *not* available: the
cooperative de-trigger needs the SE, so under a dead SE a hostile trigger costs you a per-tier race
rather than a 111-vB re-anchor. That correlated case — dead SE *and* mass grief — is PROTOCOL.md's
worst case, residual **R-1**; it is never confiscation, and the attacker still burns ~2.5× the
defender per coin.

**Un-laddered coins** under a dead SE: the branch broadcasts *immediately* (locktime 0), and only
the leaf backup waits its own fresh ladder — the `wait_blocks` figure `unilateral_exit` reports.
**Outcome, both shapes:** funds fully recovered on-chain; the only variable is how long you wait,
never whether you win.

### 5.3 SE compromised or colluding with a previous owner

The trust floor, demonstrated by `sdk15`: a malicious SE can co-sign a *fresh* transaction for a
previous owner, and a fresh signature carries no timelock at all — the ordering machinery, which
only ranks *pre-signed* state, gives no advantage against it. This is **B1**, the statechain trust
unit, and TES-R leaves it byte-identical: the same attack, the same mitigations, the same
requirement that the enclave really did delete the old share.

What constrains the collusion is unchanged and worth restating. The SE alone can do nothing — the
coin is a 2-of-2 and the SE never holds your share; freeze ≠ seize. The *API* refuses to raise a
budget (`set_spend_budget` clamps to `min()`, INV-24), so un-terminating a node requires the operator
to rewrite its own database — the clamp is application code, not cryptography, and a compromised SE
controls both the table and the public endpoint that reads it. What that subversion cannot do is
**hide**: anyone who previously recorded `terminal: true` for a node catches the flip, and a fresh
co-signature spending a terminal/single-use/expired node is publicly attributable misbehaviour. The
refusal layer turns SE misconduct on structural nodes from silent into **auditable**, not impossible.

One thing genuinely improved. Post-hack theft against a *watched* laddered coin now requires a public
trigger plus ≥ 144 blocks of on-chain notice before anything of the attacker's is valid, instead of a
minutes-scale mempool race at a silent calendar maturity. Coins received before the hack and left
untouched are unconditionally safe either way (the hacked SE holds only the post-rotation share, and
the owner's partial is required for every spend path). **Outcome:** same residual trust unit as
vanilla Mercury or full-collusion Spark; watch, and exit at the first sign of anomaly.

### 5.4 Fee spike 10× during a unilateral exit

Every pre-signed transaction has a fee decided at signing time; **there is no RBF** — re-signing
would need the SE. Two answers, one per shape.

**Laddered.** Each tier tx is nVersion=3 (TRUC) and carries two things: a **committed fee** targeted
at ~2 sat/vB drawn from the coin, so the base case relays and confirms standalone, and a **240-sat
P2A anchor** (`OP_1 0x4e73`, anyone-can-spend) so *any* party — owner, keyless tower, operator — can
attach a live-rate fee child (~152 vB) during a spike. TRUC's 1P1C topology plus sibling eviction
gives pinning resistance: each tier confirms before the next is even valid, so there are no long
vulnerable chains for a third party to pin. The committed fee is a **floor, not the whole fee**, so
the stranded-reserve pathology of the old design does not return in force. **The honest gap:**
`watch_pass` and `exit_pass` broadcast tier by tier with plain `transaction_broadcast_raw` and attach
**no** P2A child today. The anchor is present in every pre-signed tier and standard-relayed
(`sdk40` proves the SE blind-signs the v3 + CSV + P2A shape), but the rescue path is not exercised by
any test, and spike-time fee bumping is a specification rather than shipped code (PROTOCOL.md §5.13,
residual **R-4**). Note also that a fee spike outlasting the 36-block head start converts the CSV
edge into a pure fee race — δ is a dial, and quantifying it against mainnet fee history is open
problem O-2.

**Un-laddered.** Branch txs carry the split's pre-committed reserve (300–2,000 sats; at 155 vB that
is ~2–13 sat/vB), so a 10× spike can strand them in the mempool — but stranded is not lost: the
transaction stays valid forever, and once the leaf backup's locktime matures, the backup — which pays
to *your own address* — can be spent by a high-fee child that CPFPs the entire ancestor package onto
the chain. The sharp edge is timing: the branch must land before the earliest hostile ancestor
maturity, which is why exiting *early*, not at the deadline, is the rule.

**Outcome:** exits confirm slower and may cost a CPFP child (un-laddered) or a P2A child (laddered,
manual today) at spike rates. A laddered exit has no deadline to beat, only tiers to wait out; an
un-laddered exit is safe provided the branch lands before the earliest hostile maturity.

### 5.5 A previous owner broadcasts stale state

**Laddered — the attack is loud, slow, and answerable.** A past owner cannot broadcast their stale
state directly; it spends `X_m.out[0]`, which does not exist on-chain. Their only opening move is
`T`, which spends `F` in public. Four phases:

1. **`T` in the mempool / confirmed.** Every tower watching that one outpoint sees it. Nothing else
   is valid yet. Preferred response: **cooperative de-trigger** — the owner and SE key-path-spend
   `T.out[0]` into a fresh funding output, unopposed, inside the ≥ 144-block window. The coin stays
   off-chain with a fresh ladder and the attacker has paid ~276 vB to cost the victim ~111 vB
   (`sdk40` PART 2 runs exactly this: after the de-trigger, `X′` can never confirm, even past `E`
   blocks).
2. **`T` + `E_m`.** If the SE is unreachable, the owner's watchtower broadcasts the newest extension.
   Every extension is a rival spend of the same output `T.out[0]`, and the newest carries the
   *lowest* CSV, so it matures ≥ 36 blocks before any older one; once it confirms, every older
   extension's prevout is gone and every state hanging on an old epoch can never confirm at all. An
   old-epoch attacker loses here outright.
3. **`X` + `Δ_k`.** The owner's current state, carrying the strictly-lowest CSV (enforced at adoption
   by `verify_bundle`), matures 36 blocks before the newest stale one and settles the coin at the
   owner's key. `sdk51` runs this end-to-end against a real hostile trigger; `sdk41` proves the payer
   cannot claw back after paying.
4. **Never a first-seen free-for-all.** There is no phase in which two matured rivals sit in the
   mempool together on equal terms, provided the defender acted within their head start. If the
   defender sleeps through the whole notice window *and* the head start, it becomes a fee race — that
   is the accepted R-2/R-4 residual, not a design property.

**Un-laddered — three phases, all tested.** While `tip < stale locktime`: the network **rejects the
transaction as non-final** — the broadcast achieves nothing and the honest owner needs to do nothing.
At the honest owner's own maturity: the **exclusive window** — for `interval` blocks (10 ≈ 100 min
deployed; 100 ≈ 17 h defaults) the honest backup is the only final tx, so a broadcast that *confirms*
inside it is uncontestable. Broadcast early in the window at a fee that buys a confirmation inside
`interval` blocks, and note that `interval` is also the margin IVL-REQ-3 sizes against
reorg-plus-reaction. After the stale state matures too: a first-seen race a watchtower wins by
detecting the hostile mempool entry and broadcasting immediately; for sub-coins the locktime-0 branch
outruns any locktimed backup at any time before maturity, and a mempool conflict surfaces as
`ExitBranchConflict` rather than a silent failure. **Outcome:** an honest owner whose exit confirms
within the exclusive window always wins; one who is merely *watching* wins the ensuing first-seen
race with high probability; one who is offline past the window is gambling.

### 5.6 The hot-potato merchant coin

Under the old absolute ladder this was a genuine cliff: velocity and calendar burned the same
1,000-block budget, and a coin hopped 90 times in an afternoon died ~100 blocks after *deposit*.
That cliff is gone.

**Laddered.** A merchant coin gets 36 hops per epoch and 16 epochs — **576 transfers per depth
level** — and when a level exhausts, `transfer()` rolls it over **off-chain**, unattended, for zero
on-chain bytes. Elapsed time contributes nothing: 576 hops in an hour and 576 hops over a year cost
exactly the same. The only chain contact is the optional depth cap (default 3): one 112-vB solo
compaction per ~2,300 transfers, priced into the transfer that triggers it, ≈ 0.05 vB per transfer.
`sdk43` is the standing proof that renewal and rollover are unbounded and free.

What a high-velocity operator should budget for instead is **exit weight**, not lifetime: every depth
level adds 2 pre-signed txs to a contingent unilateral exit and up to 2,160 blocks to its worst-case
latency. (Each hop also lowers the current state's CSV by 36, which *shortens* the exit wait and
consumes one of the epoch's 36 decrements; renewal resets it to `D0`.) Neither is a cliff, and
neither is visible to the payer.

**Un-laddered.** The old arithmetic still applies to carriers and freed sub-coins: remaining life
`= initlock − k·interval − (tip − H)`, receive-side validation hard-rejects any handover whose
backup locktime is at or below the tip (`LocktimeTooLow`, `lib/src/transfer/receiver.rs`), and at the
floor the coin must be re-anchored (`refresh`) or cooperatively withdrawn. **Outcome:** a laddered
merchant coin needs no rotation at all; an un-laddered one must be re-anchored well before its floor,
and high-velocity token flows should budget hops *and* elapsed blocks like they budget fees.

### 5.7 Long-hold cold storage

The old rule — *never treat a statechain coin as cold storage beyond the horizon* — was a
consequence of the calendar, and for a **laddered** coin there is no longer a horizon to be beyond.
The precise position:

A self-deposited, never-transferred laddered coin has: no calendar deadline; no idle rent; no
counterparty holding stale state; and — because copies of `T` travel only with the coin's ownership
history — **no party other than the owner who is able to start its clock at all**. The SE alone
cannot spend a 2-of-2 it holds one share of, and the B1 fresh-sign attack of §5.3 needs a *past
owner* contributing the other share, which a never-transferred coin does not have. Such a coin can
sit indefinitely.

A **received** laddered coin is different in one respect only: its previous owners hold `T` and stale
states, so it carries the perpetual (alarm-driven, keyless, delegable) watching duty of R-2. Cold
storage means nobody is watching — so for a received coin, cold storage means delegating to towers
that *are* watching, not to nobody.

The remaining reasons a statechain coin is an imperfect vault are operational, not protocol: the
cooperative paths depend on SE liveness (and any epoch deadline, §5.8); pre-signed tier fees are
frozen at signing time and drift against the fee market, with the P2A rescue path specified but not
yet exercised (§5.4); and the exit material is not seed-derivable, so the *real* long-hold risk is
losing `wallet.db` and the recovery bundle (§5.9, TRUST-MODEL B7). **Un-laddered** carriers keep the
old rule outright: they age on the calendar and must be materialized before their root deadline
(`sdk32` is the standing record of a received token idled past every horizon; `sdk34` shows the
watchtower doing it for you). **Outcome:** a never-transferred laddered coin is a legitimate
long-hold instrument; a received one is, provided towers are running; a carrier is not.

### 5.8 Epoch-bounded coins (compliance / limited mandate)

An optional per-coin `epoch_deadline` (unix seconds, set at deposit) makes the SE refuse **new**
co-signatures once its clock passes the deadline (410, ERR-2; `RGB_E2E=7`). Unlike Ark's round expiry
there is no sweep: unilateral exit never needs the SE, so the pre-signed exit path — the tier chain
on a laddered coin, the branch plus backup on an un-laddered one — lives forever. Use it to
hard-bound circulation: a custodial mandate ending on a date, a compliance-scoped instrument, a
bounded delegation. Note the interaction with a laddered coin: past the epoch the SE will also refuse
renewal and rollover, so the coin is frozen at its current epoch and must eventually be exited rather
than kept off-chain indefinitely. **Outcome:** at the epoch the coin simply stops moving off-chain;
the holder exits unilaterally (or withdrew earlier); nobody, including the SE, can confiscate it.

### 5.9 Receiving as a fresh user with zero on-chain footprint

`sdk16`: a brand-new wallet with no UTXOs, no deposits, and no chain history receives off-chain and
is a first-class owner. Its exit material is entirely local — for a laddered coin, the tier bundle
(`tesr-<id>` for a whole coin, `ctesr-<id>` for a received split child) with its trigger, extension,
current state and per-tier CSV schedule; for an un-laddered coin, the pre-signed backup ladder plus
the branch rows (`branch-<id>`, root-first) and the ancestor list (`parents-<id>`). Either bundle is
a complete, SE-independent exit, and neither contains key material of the SE's, which is what makes
it delegable to a tower (`sdk45`).

Note the corollary, which applies to **every** coin shape: **a mnemonic alone does not restore a
wallet.** The seed rebuilds the key hierarchy but not the per-coin exit material — statechain ids,
the tier chain, the backup ladder, the `branch-*`/`parents-*` rows — which lives only in the wallet
database and which the blind SE cannot re-serve after a claim (review **H3**, TRUST-MODEL B7). A
sub-coin restored without its branch at least fails loudly: exit raises an explicit "restore the
recovery bundle" error (audit [20]) rather than failing opaquely. Token wallets additionally need the
entire `rgb_data_dir`, which the recovery bundle deliberately does not embed. **Outcome:** full
self-custody from the first second, zero on-chain onboarding cost — but the recovery bundle, not just
the seed, is what must be backed up.

### 5.10 The griefing cases

**A replayed owner-auth signature (audit [15], closed).** Owner authentication was historically a
static signature over `sha256(statechain_id)`, replayable across owner endpoints by anyone who
observed one request (a logging proxy, a TLS terminator, an SSP in the path). The worst replay:
`set_spend_budget` with `remaining = 0`, which — because the budget is monotonic and irreversible —
bricks the coin to **unilateral-exit-only**. What the attacker never got: the funds. The exit path
needs no SE, so the owner exits on-chain and keeps everything; the loss was the off-chain utility of
the coin plus exit fees plus the wait. **Closed in audit UPDATE 3:** the two irreversible endpoints
(`set_spend_budget`, `withdraw/complete`) now demand a single-use, endpoint-bound challenge — a
5-minute SE nonce (`GET /auth/challenge/<sid>`) signed as `sha256(nonce ‖ endpoint)` and atomically
consumed — so a captured signature can no longer be replayed or redirected. The lower-harm
`transfer`/`sign` endpoints deliberately keep the static auth (harm bounded by the coin protocol, the
pending-transfer lock, and the enclave's nonce consume).

**A hostile trigger (the ladder's own griefing case).** Anyone holding a copy of `T` — i.e. any past
owner — can broadcast it to force the victim a cost. The response is the cooperative de-trigger:
~111 vB, one confirmation, coin fully restored and still off-chain. The attacker pays ~276 vB
(`T` plus a fee child) to cost the victim ~111 vB, a damage:cost ratio of ≈ 0.4 — economically
losing, and fee-attributable on-chain even though `T` itself is anonymous. The residual is
*saturation*: ~1M simultaneous triggers would demand ~111M vB of co-op responses inside a ~144-block
grace ≈ 77% of block space for a day — strained but survivable, with the attacker burning ~2.5× more;
beyond that the response degrades to a prioritized fee auction on the highest-value coins. If the SE
is *simultaneously* dead the de-trigger is unavailable and every coin fights the per-tier races of
§5.5. That correlated scenario is PROTOCOL.md residual **R-1**, and the mass-grief prioritization
policy is open work (O-6), not shipped code.

**Outcome, both cases:** griefing costs the victim fees and inconvenience and costs the griefer more;
neither case can take funds.

## 6. The UX perspective

**What the wallet surfaces.** `estimate_exit_cost(coin)` returns
`{branch_txs, branch_vbytes, backup_vbytes, total_vbytes, wait_blocks, exit_deadline_block}` with
`fee_sats_at(rate)` for live fee math — note that `exit_deadline_block` is `None` for a laddered coin
and for any flat coin, and is populated only for a coin with an un-broadcast exit branch.
`unilateral_exit` returns per-coin `ExitStatus{complete, wait_blocks}` and is idempotently
re-callable: on a laddered coin it advances the tier chain one maturity at a time, so "call it again
next block" is the whole protocol. `defend_ladders()` is the laddered watchtower pass; schedule it
per block. Events:

| Event | Means |
|---|---|
| `DepositConfirmed` | funding reached the confirmation target |
| `LadderEstablished` | a fresh confirmed deposit was laddered by `claim()` — it is now transferable |
| `TransferClaimed` / `TokenTransferClaimed` / `BalanceUpdate` | ordinary bookkeeping |
| `LadderDefended{tiers_broadcast}` | someone triggered a coin of yours and your pass raced its tiers — watch it through |
| `ExitBranchConflict` | a *different* tx is spending your branch root: someone is racing your exit. Fee-bump/alert; do not assume the exit landed |
| `ExitDeadlineApproaching` | an un-laddered sub-coin is inside its margin; `auto_exit_due` broadcast its branch |
| `TokenCarrierMaterialized` | a received carrier was materialized before its deadline (branch only) |
| `CoinRefreshed` | a coin was re-anchored — **re-export your recovery bundle** (new statechain id, new exit material) |

Deposit-time cost surfaces as `SdkError::TokenPaymentRequired{token_id, deposit_address, fee_sats}` —
pay and retry.

**What a user must do, and when.** Usually: **nothing** — and on a laddered coin that is now
literally true for as long as they hold it. The exhaustive list of moments that require action:

| Trigger | Deadline | Action |
|---|---|---|
| Holding a laddered coin, nothing happening | **none — there is no deadline** | nothing. It does not age, expire, or cost anything |
| `LadderDefended` fires, or you see `F` spent | within the tier head starts (≥ 288 blocks total notice, 36 blocks per tier) | let `defend_ladders()` keep running; if the SE is up, take the cooperative de-trigger instead of racing |
| Holding an un-laddered sub-coin or a received carrier, going offline | before `exit_deadline_block` minus an assumed `M·interval` margin (IVL-REQ-16; [17]) | broadcast the branch first, or leave `auto_exit` on / delegate a tower running `auto_exit_due` |
| `ExitDeadlineApproaching` / `TokenCarrierMaterialized` | within your margin | the branch is already broadcast — confirm it lands |
| `ExitBranchConflict` | immediately | CPFP/alert/re-attempt; treat the exit as contested |
| An un-laddered coin nearing its ladder floor | before it floors out | `refresh` (user-pays) or `refresh_sponsored` (operator rebates off-chain) |
| `TokenPaymentRequired` on deposit | before depositing | pay `fee_sats`, retry |
| SE unreachable and you want out | none — any time | `unilateral_exit`, re-call each block until `complete` |
| `CoinRefreshed` (any cause) | promptly | re-export the recovery bundle |

**Refresh, in one line.** `refresh(coin)` **re-anchors** a coin: one SE-co-signed on-chain tx
(~112 vB) spends its outpoint into a fresh aggregate, and the coin comes back with a brand-new
funding outpoint and a brand-new ladder. That is what it is — a re-anchor primitive, verified by
`sdk30` — and what it is *for* differs by shape. On the **un-laddered** shape it is the lifetime
reset: the old outpoint is spent, so every old backup dies and the root deadline moves. On the
**laddered** shape there is no deadline to reset; the same primitive is the optional **solo
compaction** that returns a deep coin to depth 0 under the default depth-3 policy, priced into the
transfer that triggers it (§3c). Either way it is cooperative — if the SE is gone, exit instead.

Two fee models: **user-pays** (`refresh` — coin = amount − fee) or **operator-pays**
(`refresh_sponsored` — a funded operator rebates the fee off-chain). Why a rebate and not the
operator adding an input to the refresh tx? Because the re-anchor is **single-input by
construction**: the blind SE co-signs exactly one input (its own 2-of-2) and holds no funds or chain
view, so nobody can co-fund the transaction — reimbursement must ride the off-chain rail. The rebate
is `max(fee + DUST_LIMIT, min_child_value)`: 330 sats is the P2TR dust floor (smaller than legacy's
546), the sub-dust 112-sat fee cannot be sent exactly, and after defect **D2** — a sponsored refresh
that sized its rebate into the dead 442..1,306 window and so failed *after* the user had already paid
the on-chain fee — the floor also clears `min_child_value`. The operator absorbs the round-up,
leaving the user ≥ whole. `sdk38` pins what a *broke* sponsor does (errors cleanly, bounded loss).

**Auto-refresh, honestly.** `SdkConfig::auto_refresh` is on by default and keys off a coin's absolute
backup locktime: `auto_refresh_due(margin)` re-anchors any confirmed non-carrier coin whose backup
headroom has fallen to `auto_refresh_margin_blocks` (default 144 ≈ 1 day), and `transfer` runs it as
a pre-spend hook so an aging **un-laddered** coin never becomes un-transferable mid-payment. Two
things to know. First, routine **background** re-anchoring is default-**off**
(`background_auto_refresh = false`): re-anchor cost is folded into `transfer` and paid on demand as
part of the payment fee, so a running wallet never silently shrinks a balance. Second, when the
pre-spend hook is the one that catches an aging coin, the transfer **waits** for the re-anchor to
confirm — a bounded ~2-minute poll, after which it returns an explicit "will succeed once it
confirms — retry shortly" error rather than hanging. Carriers are excluded (a plain re-anchor would
destroy the allocation — the watchtower materializes them instead; see
[tokens.md](tokens.md#exits-with-tokens)), and a coin too small to cover the ~112-sat fee above dust
is deliberately **skipped**, not failed — rescue it by combining. On a laddered coin none of this is
a lifetime mechanism: there is no floor to approach and the ladder does the invalidation work.

**If they do nothing:** a laddered coin is untouchable by anyone until someone publicly triggers it,
and then untouchable for another ≥ 288 blocks; the duty is to react, not to anticipate. An
un-laddered sub-coin or received carrier is exposed from its root deadline onward. Doing nothing is
safe indefinitely for the first, and safe precisely up to that height for the second.

**What a watchtower must watch** (per coin):

1. **Laddered:** the funding outpoint `F`. A spend of it is the alarm — run `watch_pass` per block
   from there, broadcasting each tier as its CSV matures and attaching P2A fee children if the
   operator implements them (not shipped — §5.4). Idle coins need no polling beyond the subscription,
   and multiple towers compose idempotently (`sdk45`).
2. **Un-laddered:** (a) any mempool/chain spend of the funding outpoint that is not the owner's own
   tx → race immediately with the branch (sub-coin) or matured backup (flat); (b) tip vs the owner's
   backup locktime → broadcast at maturity, inside the exclusive window; (c) tip vs
   `exit_deadline_block` minus a safety margin (the assumed `M·interval` bound) → force-broadcast the
   branch, which is exactly `auto_exit_due(margin_blocks)`; (d) persistence of already-broadcast
   low-fee exit txs → rebroadcast if purged, CPFP once the backup is spendable. An offline-capable
   tower must cache `initlock` and the root deposit height — never derive the deadline as
   `leaf_locktime + interval` (that formula is the bug audit [10] fixed).

The normative version of these rules is **IVL-REQ-16** in
INVALIDATION-SPEC (retired 2026-08-15); the bundle format is PROTOCOL.md §5.13.

**Time-to-money, per flow:**

| Flow | On-chain txs | Wait |
|---|---|---|
| Deposit → spendable | 1 (your funding tx) | confirmation target + SE registration + `claim()` laddering |
| Off-chain receive (whole coin or split child) | 0 | seconds (API round-trips + validation queries) |
| Off-chain onward send of a received child | 0 | seconds — `sdk60` does two hops with the funding outpoint never spent |
| Cooperative exit | 1 per coin (~111 vB) | ~1 conf |
| Unilateral exit, laddered flat coin | 3 (T+X+S ≈ 372 vB) + 0–3 P2A children in a spike | `E_m + Δ_k` sequential — worst 2,160 blocks ≈ 15 d fresh, −36 per hop |
| Unilateral exit, laddered depth-`d` sub-coin | 3 + 2d (≈ 124 vB each) | ≤ `(d+1)·2,160` blocks; depth-3 ≈ 60 d |
| Unilateral exit, un-laddered sub-coin | N branch (~155 vB each) + 1 backup | branch: now; backup: its own fresh ladder |
| Token materialization | branch only (2d+1 txs) | now — no final state needed; the allocation settles on the resting output |

The latency line deserves emphasis, because it is the real price of relative timelocks: **a
unilateral exit is slow, and it gets slower with depth.** Cooperative exit, when the SE is alive, is
one transaction and one confirmation at any depth — and that is the path essentially every user
takes. The unilateral chain is the guarantee that makes the cooperative path safe to prefer, not the
path itself.

**Sharp edges, honestly, as of 2026-07.** Post remediation UPDATE 3 all 11 HIGH findings and all
griefing/DoS MEDIUMs (including the [15] brick, §5.10) are fixed and verified; mainnet remains gated
on the SGX enclave rebuild, a re-audit, and a third-party audit. Still open:

- **No unconditional no-watch window** on the ladder (R-2): a received coin must be watched — keyless
  and delegable, alarm-driven with ≥ 1 day of notice, but watched.
- **Spike-time fee bumping is specified, not shipped** (R-4): every tier carries its P2A anchor, but
  `watch_pass`/`exit_pass` attach no fee child and no test exercises the rescue.
- **Deep-DAG unilateral latency** (R-5): depth-3 worst ≈ 60 days; the geometric-schedule dial that
  would cap it near 30 days is default off (O-4).
- **The enclave counter machine `{level, m, k}` is safety-relevant** for receiver verification and
  wants an INV-23/24-grade formal spec before ship (O-1).
- **Un-laddered residuals stay exactly as written**: the conveyed-locktimes half of [17] (the reported
  deadline can be `k·interval` too late, absorbed by the `auto_exit_due` margin), no RBF on any
  pre-signed tx, and ancestor-id substitution (B2) on the branch lane.
- **Recovery bundle is not seed-derivable** for any coin, laddered or not (H3, B7); token wallets also
  need `rgb_data_dir`.
- **Plain-BTC combine is a lib-level primitive only**, so fragmentation from repeated splits still
  costs one withdraw tx per coin ("combine-then-withdraw" batching is a future optimization).

## 7. FAQ

**Is there still a ~7-day timeout?** No — not on a laddered coin, which is every plain deposit. The
timeout existed because *absolute* locktimes tick while un-broadcast, so a decreasing sequence needed
a finite start and the defence had to be renewed on the calendar. TES-R uses *relative* CSV locks on
un-broadcast transactions, and the top of the chain (`T`) has no timelock at all, so nothing counts
down until somebody spends the funding outpoint in public. An idle coin never ages, costs 0 vB, and
has no deadline (§1b; `sdk40` PART 1, `sdk43`). A calendar deadline survives on the **un-laddered**
shape — RGB carriers and sub-coins over un-broadcast funding — where a received coin still owes one
materialization before `H_deposit + initlock` (§4).

**Do I lose anything if I do nothing for a year?** On a laddered coin, nothing at all: no forfeiture,
no expiry, no rent, no degradation — this was never Ark-style expiry and now there is not even a
window to lapse. The one duty that survives is *reactive*: if a past owner triggers your coin, you or
a tower must respond inside the notice window (§5.1). On an un-laddered coin the old answer holds:
nothing is forfeited either, but a floored coin can no longer be *transferred* (the receiver's claim
fails with `LocktimeTooLow`) while remaining always *exitable* — cooperatively at any moment,
unilaterally since maturity — and a received carrier idled past its root deadline is exposed to a
clawback race, which is exactly what `auto_exit_due` materializes away (`sdk32`, `sdk34`).

**Can the SE steal my coin?** Not alone: the coin is a 2-of-2 and the SE never holds your key share.
Colluding with a *previous owner* it can fresh-sign a competing spend and force a broadcast race
(§5.3, B1) — the trust floor shared with every statechain design. It cannot forge terminal state
(monotonic), and misbehaviour on structural nodes is publicly queryable.

**Can the SE freeze my funds?** It can refuse to co-sign (or die, or be legally compelled), which
kills the *cooperative* paths only: transfers, renewal, rollover, cooperative withdraw, cooperative
de-trigger. Unilateral exit is pre-signed and SE-independent; worst case is the tier wait. Freeze ≠
seize: at most it converts your coin into an on-chain exit ticket. The one exception is the
onboarding window — the guarantee begins when the SE co-signs the trigger at deposit detection, and
an SE that refuses or dies *before* that strands the funding in the 2-of-2 (§5.2), so fund only after
deposit init succeeds.

**What if I lose my wallet database?** That is loss of funds, and **no** statechain coin restores
from the mnemonic alone (review H3, TRUST-MODEL B7): the seed rebuilds the key hierarchy but not the
per-coin exit material — statechain ids, the tier chain (`tesr-*`, `ctesr-*`), the backup ladder, the
`branch-*`/`parents-*` rows — which lives only in the wallet database and which the blind SE cannot
re-serve after a claim. Back up with `export_recovery_bundle` and re-export after every transfer,
claim, split, child re-transfer, **or refresh** (a refresh replaces the coin: new statechain id, new
exit material — treat `CoinRefreshed` as a re-export trigger). Token wallets must additionally copy
the whole `rgb_data_dir`, which the bundle deliberately does not embed. An un-laddered sub-coin
missing its branch rows at least fails loudly: exit raises an explicit "restore the recovery bundle"
error (audit [20]).

**Can two people be handed the same coin?** The SE binds one challenge per signing nonce atomically
(nonce reuse with a different message → 409; INV-23, `sdk12`), refuses conflicting co-signs, and now
refuses *any* co-sign while a transfer of that coin is open (the pending-transfer lock). A
*malicious* SE could still try; what a second "owner" cannot satisfy without the SE visibly
double-signing is the receiver's census — `se_num_sigs` must equal the exact expected tree size, so a
hidden extra co-signed state shows up as a count mismatch (`sdk46`, `sdk54`, `sdk58`, `sdk60`).

**Why does the split spend the extension rather than the funding output?** Because a split that spent
`F` would be a *rival* of the trigger, and a past owner's retained `T` — which has no timelock at all
— would win that race, voiding the payee's coin while the ladder paid the splitter the full parent
value. That is the split-versus-trigger theft vector that twice reverted the protocol default
(tracked in the codebase as `[B1]` — not the same B1 as TRUST-MODEL's SE-collusion boundary). An
**in-ladder split** is a state tier `SP` spending `X_m.out[0]`: a **descendant** of `T`, never a
rival for `F`, so a retained trigger has nothing to race (§3b; `sdk58`'s 11 adversarial cases,
`sdk59`).

**Is a received partial payment a real coin, or an exit-only claim?** A real coin. The claim completes
the standard SE key handover: `A_child` is invariant across the share rotation — which is precisely
what keeps the child's pre-signed exit chain valid — and the sender's auth is rotated out, so the
sender is permanently locked out. The child can be paid onward off-chain, whole (`child_retransfer`)
or split again (`child_in_ladder_pay`), one co-signature and one disclosed superseded state per hop,
counted by the next receiver's N-hop census. `sdk60` (alice → bob → carol, funding outpoint unspent
throughout) and `sdk17` (partial second hop). The child is deliberately **not** terminalized; the
census closes any pre-conveyance rival and the handover closes every later one.

**Why relative locks, when Spark also uses them?** Both use relative CSV, but the invalidation
*authority* differs. Spark's old state dies by **operator key deletion** — an honest-1-of-n trust
assumption — and its leaves need `renew_leaf` churn with the operator group to stay alive. Here, old
state dies at the **consensus** level: a renewal mints an extension that strictly undercuts every
older one in the race for `T.out[0]`, so every pre-renewal state hangs on a parent that can never
confirm, and the enclave's single-active-state refusal is a second, independent layer on top rather
than the whole mechanism. Renewal and rollover are also fully off-chain here and unbounded
(`sdk43`), and amounts are exact rather than denominated. The honest caveat: "consensus-dead" is
*race-conditional* — the newest extension must win a ≥ 36-block-edge race after a public trigger.
Stronger than a key-deletion promise; still a race, not an axiom.

**Why absolute locktimes on the un-laddered shape, then?** Because that shape exists precisely where
a relative-CSV tier chain cannot go. An RGB carrier must never be laddered (rule 1 of terminal-freeze:
a plain tier spend destroys the allocation), and a sub-coin over un-broadcast funding has no confirmed
prevout for a trigger to spend, and a v3 tier cannot relay over an unconfirmed v2 parent. The
signed-once absolute backup is what those coins can carry, and it works — at the price of the calendar
deadline they bring with them.

**Why locktime 0 on un-laddered split transactions?** Because the branch must beat every
deposit-anchored stale backup unconditionally. Any nonzero branch locktime can end up *above* an aged
parent's backup, letting the stale state mature first and win (the arithmetic bug behind audit H5). A
height-0 branch is broadcastable now and sits below every backup by construction (INV-4); receivers
reject any branch tx with locktime > tip (audit [11]).

**What does "terminal" mean — can it be undone?** Terminal = the SE will never co-sign this statechain
again (`finalized ≥ sig_budget`). `set_spend_budget` is `min()`-monotonic (INV-24), so no request *via
the API* — from the owner or an attacker — raises it. An operator subverting its own database could
(the clamp is server application code, §5.3), but the flip contradicts every `terminal: true` answer
the public `GET /statechain/spend_budget/<id>` endpoint served before — observers who record those
answers hold the receipt. Honest-server permanent by design; compromised-server detectable. Note what
is terminalized today: the **parent** of any split, the ancestry of any RGB anchor, and the
**Lightning-latched piece** — but *not* an ordinary split child.

**Who pays exit fees?** On the ladder, every tier carries a committed fee (~2 sat/vB) drawn from the
coin itself at signing time, plus a 240-sat P2A anchor that lets *anyone* — including a tower or the
operator — top it up at live rates. On the un-laddered shape the *splitter* pre-pays each branch tx's
fee (the 300–2,000-sat reserve deducted from change at split time) and the *exiter* pays the backup's
fixed pre-signed fee plus any CPFP top-up. Cooperative exits pay normal on-chain fees at live rates.

**What if the mempool purges my pre-signed tx?** Nothing is lost: pre-signed transactions never expire
and rebroadcast is free. A purge only delays; `exit_pass` and `watch_pass` are idempotent and
incremental, so the next call simply re-broadcasts what is missing (§5.4).

**Is there any scenario where an honest, online user loses funds?** Within the model, one: the SE
colludes with a past owner *and wins the broadcast race* against the online user (§5.3) — online users
with a working fee bump win that race in practice, but it is a race, not a proof. Plus one boundary
case with no adversary at all: an SE that dies before co-signing the trigger strands the deposit in
the 2-of-2 (§5.2's onboarding window). Every other adversary — stale broadcasters, triggers, griefers,
dead SEs, fee spikes — loses to an online user mechanically. Offline is where the qualifiers pile up
(§5.1, §5.7).

**What happens when the hop budget runs out?** Nothing visible. At 36 state decrements the SDK renews
the extension off-chain inside the next `transfer()`; at `m = 15` it rolls over to a fresh level, also
off-chain; at depth 3 the default policy folds one 112-vB re-anchor into that transfer's fee. `sdk43`
runs the whole sequence unattended. The old cliff — a coin that could no longer be handed on at all —
exists only on the un-laddered shape, where the 100th hop's locktime reaches the deposit anchor and
receive-side validation rejects it (`LocktimeTooLow`).

**Does splitting extend my coin's life?** The question no longer has a laddered meaning: there is no
lifetime to extend. An in-ladder split gives each child its own extension and state tiers and costs
the parent its terminality. On the un-laddered shape the old answer holds: the *leaf's* ladder resets
(fresh `initlock` at split height) but the *tree's* root deadline `H_deposit + initlock` never moves;
only materialization or refresh resets the wall clock (§4).

**Can a previous owner do anything at all before broadcasting the trigger?** No. Their states spend an
output that does not exist on-chain, their extension the same, and the SE will not co-sign for them —
key rotation plus single-active-state plus the pending-transfer lock mean the SE only answers the
current owner. Their move begins with `T`, which is public, and every honest response begins there
too.

**What if my watchtower dies?** You inherit its duties on your next wake: check that no funding
outpoint was spent hostilely (laddered — and if one was, run `defend_ladders()` immediately), and
check the margin-adjusted deadline on any un-laddered sub-coin or carrier. Exposure is limited to
coins that were *triggered* during the outage, or whose root deadline passed during it — not to
everything you hold. Towers are keyless and idempotent, so the real answer is to run more than one.

## 8. Comparison recap: over-time behaviour

| | **Ours (TES-R, laddered)** | **Ours (un-laddered shape)** | **Spark** | **Ark / Second** | **Mercury (vanilla)** |
|---|---|---|---|---|---|
| Lifetime bound | **None** — relative CSV on un-broadcast txs; an idle coin never ages | Absolute: `initlock` from deposit (6.9 d / 69 d), 100 hops | Relative ladder 2000, −100/hop; unbounded *if* renewed | Round expiry (~weeks), hard | Absolute ladder, one horizon, no refusal layer |
| Idle on-chain footprint | **0 vB/yr** | 0 vB/yr, but a received coin owes one materialization before its root deadline | 0 | Refresh per round or lose funds | 5,840 vB/coin-yr |
| Renewal | **Off-chain and unbounded** — lower-CSV extension re-sign, then off-chain self-split rollover (`sdk43`); optional 112-vB compaction per ~2,300 transfers | On-chain `refresh` re-anchor (1 tx), or self-split (leaf only) / materialization | `renew_leaf` churn at ≤ 300 blocks, needs the operator group | Mandatory per-round refresh participation | None |
| What invalidates old state | **Consensus** — lower CSV wins the trigger output, old epochs' parents unconfirmable — plus enclave refusal and the receiver census | Consensus (absolute locktime ordering) + enclave refusal | Operator honest key deletion (1-of-n trust) | Round expiry | Absolute locktime ordering only |
| Operator dies | All exits pre-signed; wait `E_m + Δ_k` (≤ 15 d flat, deeper for sub-coins), funds whole | Branch broadcasts now; leaf backup waits its ladder | Unilateral path exists; timelock race | Exit window critical; miss it → server sweeps | Wait ≤ `initlock` |
| Stale state over time | Inert until a **public** trigger, then ≥ 288 blocks of notice and a ≥ 36-block head start per tier | Non-final → exclusive window → watched race; branches beat ancestors before the root deadline | Timelock race; key-deletion honest-1-of-n | Dies at expiry (the same knife that threatens users) | Timelock race only |
| Missed-liveness outcome | Raceable only after a public trigger + ≥ 1 day notice; **never** confiscated | Raceable after the deadline; never confiscated | Safe if renewed; trust-dependent | **Confiscation** — funds sweep to the server | Raceable after maturity |
| Operator misbehaviour visibility | Terminal state + tier counters publicly auditable per node | Terminal state publicly auditable per node | Not queryable per node | Round tree is public | None |
| Offline requirement | **Perpetual but alarm-driven** — nothing until someone triggers, then react within the head start; keyless, delegable to N towers | Online (or watched) before the root deadline | Online for renewals, forever | Online every round, forever | Online after maturity |

Further reading: the shipped ladder in [PROTOCOL.md](../PROTOCOL.md) and
[CHILDREN.md](../CHILDREN.md); Lightning over the ladder in [LIGHTNING.md](../LIGHTNING.md);
normative un-laddered requirements in INVALIDATION-SPEC (retired 2026-08-15); fee/size
tables and feerate scenarios in
invalidation-economics (retired 2026-08-15); the short comparison in
[invalidation.md](invalidation.md); partial amounts in
[granularity-deep-dive.md](granularity-deep-dive.md); exit mechanics in [exits.md](exits.md);
system spec [SPEC.md](../SPEC.md) (REQ-16/18/25, INV-4/20/23/24/25, ERR-1/2/3/7/12); trust map in
[TRUST-MODEL.md](../TRUST-MODEL.md); audit trail in AUDIT-2026-07 (retired 2026-08-15).

Test evidence cited on this page: `sdk12`, `sdk15`, `sdk16`, `sdk17`, `sdk30`, `sdk32`, `sdk34`,
`sdk38`, `sdk39`, `sdk40`, `sdk41`, `sdk42`, `sdk43`, `sdk44`, `sdk45`, `sdk46`, `sdk47`, `sdk50`,
`sdk51`, `sdk52`, `sdk54`, `sdk55`, `sdk58`, `sdk59`, `sdk60`, plus `RGB_E2E=7` (epoch deadline).
The live dispatch range is `SDK_E2E=1..68` with gaps where tests were retired, plus the concurrency
chaos test `SDK_E2E=22` (`clients/tests/rust/src/main.rs`).
