# Old-state invalidation — the deep dive

How this system makes yesterday's owner unable to spend today's coin, what that machinery does
over days and weeks, and what it feels like to hold, receive, and exit a coin. This page is the
long-form explainer; the short comparison is [invalidation.md](invalidation.md), the normative
requirements live in [INVALIDATION-SPEC.md](../INVALIDATION-SPEC.md), fee/size tables in
[invalidation-economics.md](../research/invalidation-economics.md), and exit mechanics in
[exits.md](exits.md). Audience: developers, integrators, and researchers who have not read the
code. Every number below comes from the code paths cited; where behaviour is an open audit item
we say so rather than round it off — see [AUDIT-2026-07.md](../AUDIT-2026-07.md), especially the
open item [17]. Terminology follows the
[INVALIDATION-SPEC.md §0 table](../INVALIDATION-SPEC.md#0-scope-terminology-relationship-to-specmd);
the two words used most: a **flat coin**'s funding output is on-chain, a **sub-coin**'s funding tx
is pre-signed but un-broadcast (*materializing* the branch broadcasts it, turning the sub-coin flat).

Contents: [the problem](#1-the-problem-from-first-principles) · [four layers](#2-the-four-defence-layers)
· [walkthroughs](#3-lifecycle-walkthroughs) · [over time](#4-over-time-the-ladder-as-a-consumable-budget)
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
state carries a lower timelock, so the newest matures first and wins any honest race — Mercury's
absolute ladder, Spark's relative one, SuperScalar's nSequence counters; simple and trustless,
but finite: the ladder is a budget that runs out), and **operator refusal** (a semi-trusted
co-signer simply refuses to sign conflicting or expired state — every statechain; instant and
race-free, but only as strong as the operator's honesty). This system takes decrementing locks
as the trustless floor and layers operator refusal, receiver-side verification, and
deadline-bounded watching on top — four layers, each covering the failure mode of the one below.

## 2. The four defence layers

Two configuration profiles appear throughout. Both allow **100 hops** (`initlock / interval`);
they differ in wall-clock horizon (at ~144 blocks/day):

| Profile | `initlock` | `interval` | Hop capacity | Wall-clock horizon | Exclusive window per hop |
|---|---|---|---|---|---|
| Deployed (`server/Settings.toml`) | 1,000 | 10 | 100 | ≈ 6.9 days | 10 blocks ≈ 100 min |
| Code defaults (`server_config.rs`) | 10,000 | 100 | 100 | ≈ 69 days | 100 blocks ≈ 17 h |

Clients read the pair from `GET /info/config` as `initlock`/`interval`.

**Layer 1 — the decrementing timelock ladder** (`lib/src/transaction.rs`,
`calculate_block_height`). Every owner holds one pre-signed *backup transaction* spending the
coin's 2-of-2 funding output to their own address, with an **absolute** nLockTime anchored at the
deposit: the depositor's backup locks at `H + initlock`, where `H` is the chain tip when the SE
first co-signs the deposit backup — at deposit *detection*, in practice ≈ the confirmation height
(IVL-INV-10 keeps the small co-sign→confirmation gap explicit) — and each transfer hands the new
owner a backup locking `interval` blocks *lower*. After `k` hops the
current owner holds `L_k = H + initlock − k·interval` — the lowest locktime in existence for that
coin. While the tip is below `L_k`, *no* pre-signed transaction is final: the chain rejects them
all and the only spend path is a fresh SE co-signature. From `L_k` to `L_{k−1}` the current owner
enjoys an **exclusive exit window** of `interval` blocks in which their backup is the only final
transaction on Earth for this coin. After that, stale backups mature one by one (newest first)
and honesty degrades to a first-seen broadcast race — one the current owner has already had a
head start in. E2E `SDK_E2E=13` demonstrates a stale-state clawback being defeated by the ladder;
`SDK_E2E=14` quantifies the watchtower race once the window lapses.

**Layer 2 — SE hard refusal** (`server/src/endpoints/sign.rs:57-126`). The statechain entity
refuses to co-sign at all when: a `single_use` coin already has one finalized signature (HTTP
410, ERR-1); a coin's `epoch_deadline` (unix seconds) has passed (410, ERR-2); or a coin's
`sig_budget` is exhausted (410, ERR-3 — "terminal node"). All three guards **fail closed**: any
database error yields 503 and no signature (audit [1]). The budget is monotonic — `set_spend_budget`
takes `min(existing, finalized + remaining)`, so a terminal node can never be un-terminated
(INV-24). Independently, the enclave consumes each signing nonce atomically, so the SE cannot be
tricked into signing two different messages under one nonce (single-active-state, INV-23/ERR-12,
proven by `SDK_E2E=12`). Crucially, the SDK sets the parent's budget to *exactly one more
signature* immediately **before** co-signing a split (`rust-sdk/src/transfer.rs:349-355`), so the
split itself consumes the budget: after a split, even the legitimate owner cannot get the parent
co-signed again. Terminal status is **publicly auditable** via `GET /statechain/spend_budget/<id>`.

**Layer 3 — receiver-side verification** (`clients/libs/rust/src/transfer_receiver.rs`). A
receiver trusts neither sender nor SE blindly. On receive it checks the backup ladder decrements
and signature count (REQ-16); for off-chain sub-coins it additionally runs `validate_branch`
(root on-chain, unspent, confirmed; every branch tx locktime `≤ tip` — INV-4, audit [11]; value
conservation Σout ≤ Σin per hop — INV-25; full script/signature verification) and
`verify_terminal_parents` (the sender must name at least as many structural ancestors as the
branch has hops — INV-20 — and each must report `terminal: true` at the SE, ERR-7). Blind-SE
caveat ([SPEC.md §14](../SPEC.md#14-known-limitations-adversarial-review)): ancestor *ids* are
not cryptographically bound to branch
outpoints, so the count check defeats omission, not substitution; the compensating control is
that the receiver holds the full branch and can exit immediately.

**Layer 4 — exit branches, deadlines, and watching**. Off-chain splits/combines produce
pre-signed, **locktime-zero** branch transactions (INV-4, `lib/src/transaction.rs:381-394`):
broadcastable *now*, they always beat any deposit-anchored stale backup — provided they reach the
chain before the earliest stale ancestor backup matures. `estimate_exit_cost` surfaces that bound
as `exit_deadline_block = H_deposit_root + initlock` (`rust-sdk/src/wallet.rs:634-666`, audit
[10] fix), and `unilateral_exit` broadcasts branch-first, reports remaining `wait_blocks` instead
of failing, and raises `WalletEvent::ExitBranchConflict` if someone is racing the branch in the
mempool (REQ-25). The deadline's residual imprecision is quantified in §4.

## 3. Lifecycle walkthroughs

All three use the deployed profile: `initlock = 1,000`, `interval = 10`.

### 3a. A flat coin, deposited and transferred three times

Alice deposits; her funding tx confirms at height **H = 100,000**. She then pays Bob, Bob pays
Carol, Carol pays Dave — all off-chain, minutes apart. Backup locktimes are deposit-anchored, so
*when* the transfers happen doesn't move them:

| State | Holder | Backup locktime | Status after hop 3 |
|---|---|---|---|
| qt=0 (deposit backup) | Alice | 101,000 | stale |
| qt=1 | Bob | 100,990 | stale |
| qt=2 | Carol | 100,980 | stale |
| qt=3 | Dave | **100,970** | current — lowest |

```
height   100,000                              100,970    100,980    100,990    101,000
         |                                     |          |          |          |
         | deposit confirms (H)                | L3=Dave  | L2=Carol | L1=Bob   | L0=Alice
         |<——— no pre-signed tx is final ————→|<— Dave —→|<——— broadcast races open ———→
         |     (SE co-sign is the only         exclusive   newest-first: each stale backup
         |      spend path; ~6.7 days)         window,     matures 10 blocks after the next
         |                                     10 blocks
```

At each boundary:

- **tip < 100,970** (~6.7 days): nobody — not Dave, not any past owner — can broadcast anything.
  The coin moves only by SE co-signature, and the SE will only co-sign for Dave
  (single-active-state).
- **100,970 ≤ tip < 100,980**: Dave's exclusive window. His backup is the only final transaction
  for this outpoint; a broadcast that *confirms* here cannot be contested — so broadcast early in
  the window, at a fee that buys a confirmation within its 10 blocks (§5.5). This is the honest
  unilateral-exit path if the SE has vanished.
- **tip ≥ 100,980**: Carol's stale backup becomes final too; then Bob's, then Alice's, each 10
  blocks later. First-seen wins now. Dave (or his watchtower) still wins any race he actually
  shows up for — `SDK_E2E=14` measures this — but sleeping through it is how funds are lost.

Every off-chain hop costs the coin 10 blocks (~100 min) of remaining life; the block clock costs
it 144/day regardless. Dave should spend, withdraw, or exit well before 100,970.

### 3b. A payment via off-chain split

Alice deposits 50,000 sats at **H = 100,000** (backup L₀ = 101,000, never transferred). At height
**100,300** she owes Bob exactly 20,000 sats. She splits:

1. The SDK sets the parent's spend budget to `finalized + 1` — then immediately co-signs the
   **split tx**: one input (the deposit outpoint), two outputs — 20,000 to a fresh 2-of-2 for
   Bob's piece, 29,500 change to a fresh 2-of-2 for Alice (fee reserve
   `(50,000/100).clamp(300, 2000) = 500` sats stays behind as the split tx's miner fee). The
   split tx has **locktime 0** and is *not broadcast*.
2. The split consumed the parent's last co-signature: the parent is now **terminal** at the SE —
   no later withdraw, transfer, or fresh backup on it can ever be signed, for anyone
   (`SDK_E2E=8`). `GET /statechain/spend_budget/<parent>` shows `terminal: true` to the world.
3. Each sub-coin gets a **fresh first backup** created at split time: locktime
   `100,300 + 1,000 = 101,300`. Depth does not consume ladder (`rust-sdk/src/transfer.rs:438-449`).
4. Bob receives the piece off-chain and verifies: branch linkage root-first; the root input
   (Alice's deposit outpoint) is on-chain, unspent, confirmed; the split tx's locktime is 0 ≤ tip;
   no value created; scripts verify; the sender named ≥ 1 terminal ancestor; the parent reports
   `terminal: true`.

Stale-state inventory after the split: exactly one item — Alice's parent deposit backup at
**101,000**. It is locktimed; the branch is not. Bob's `exit_deadline_block` is
`H + initlock = 101,000`, and here it is **exact** — up to the small deposit
co-sign→confirmation gap (IVL-INV-10) — because the parent was never transferred
before the split. If Bob ever distrusts anything, he broadcasts the 155-vB split tx (fee already
committed) and waits out his own fresh ladder.

```
height   100,000       100,300                101,000      101,300
         |             |                      |            |
         | deposit H   | split (locktime 0,   | parent's   | sub-coins' fresh
         |             | un-broadcast)        | stale bkup | backups mature
         |             |                      | matures    |
         |<———— branch is broadcastable anywhere here ————→|
                        Bob MUST have the branch on-chain before 101,000
```

### 3c. A depth-3 tree

Same deposit, **H = 100,000**. Splits at heights 100,050 (parent → A + change), 100,120
(A → B + change), 100,200 (B → leaves), nothing transferred in between. Full stale-state
inventory:

| Node | Created | Terminal at SE? | Its (now stale) backup locktime | Held by |
|---|---|---|---|---|
| Root parent (deposit) | 100,000 | yes (split 1) | 101,000 | original owner |
| A (split-1 piece) | 100,050 | yes (split 2) | 101,050 | whoever split A |
| B (split-2 piece) | 100,120 | yes (split 3) | 101,120 | whoever split B |
| Leaves (split-3 outputs) | 100,200 | no — live coins | 101,200 (current, not stale) | current owners |

Branch to exit a leaf: 3 pre-signed locktime-0 txs (~155 vB each, fees pre-committed) + the
112-vB leaf backup ≈ 577 vB total.

The earliest hostile maturity across *all* stale ancestors is `min(101,000; 101,050; 101,120) =`
**101,000** — the root's deposit backup. Because every fresh sub-ladder anchors at its own later
split height, *and nothing here was transferred between splits*, the root deadline is the
minimum: **one number, `H_deposit + initlock`, bounds the entire tree**, however deep and however
bushy. That "nothing transferred in between" is load-bearing: an intermediate ancestor transferred
`k` times *before* its split retains a backup at its own anchor `+ initlock − k·interval`, which
undercuts the root's deposit backup as soon as `k·interval` exceeds the split-height gap — the
true deadline is the minimum over **every** ancestor's retained backups, not the root's alone
(IVL-INV-10; audit [10]/[17]). Everything below it lives or dies by whether its branch reaches
the chain before that minimum — quantified next.

## 4. Over time: the ladder as a consumable budget

A coin's off-chain lifetime is a single budget of `initlock` blocks, drawn down by two clocks at
once: **each hop instantly burns `interval` blocks** (the handover decrement) and **each block of
real time burns one block** (the tip rising toward the deposit-anchored ladder). After `k` hops
the current owner's backup matures `initlock − k·interval` blocks after deposit — whichever
combination of velocity and calendar reaches zero first, wins. A coin that sits still for 6.9
days (deployed profile) and a coin hopped 99 times in its first hour exhaust the same
1,000-block budget — but at very different heights: the idle coin dies at `H + 1,000`, while the
hot one's floor has dropped to `H + 10` and meets the tip well within its second hour. Hops pull
the death height *down*; they do not merely pre-spend calendar.

**What "the coin is dying" looks like:** `wait_blocks` (from `estimate_exit_cost`) trends to zero
— dropping 1 per block and 10 per receive. When it hits zero the exclusive window opens; 10
blocks later the first stale backup is final and every further block adds one more past owner to
the set of people who could race. There is no alarm bell in the protocol other than this number;
wallets should treat a low `wait_blocks` as "spend, withdraw, or exit now".

**There is no renewal.** No endpoint refreshes a ladder (verified: no renew/refresh path exists
in the code). This is deliberate — Spark's `renew_leaf` churn is what we traded away — so the
only lifetime-extension options are real operations with real costs:

| Option | What it extends | What it does NOT extend | Cost |
|---|---|---|---|
| Self-split (split to yourself) | The *leaf* ladder: new sub-coin anchors at `H_split + initlock`, fresh 100-hop budget | The **root deadline** `H_deposit + initlock` — unchanged, still bounds the whole tree | One SE co-sign; 300–2,000 sats fee reserve locked into the branch; +155 vB of future exit weight |
| Materialize the branch | Puts the sub-coin's funding on-chain; the coin becomes flat and its **own** ladder (`H_split + initlock`) now governs — the root deadline no longer applies | Its own ladder (already ticking since the split) | Branch miner fees (pre-committed reserves; N×~155 vB confirm now) |
| Cooperative withdraw + redeposit | Everything — a brand-new coin, fresh `initlock`, fresh hop budget | — | 2 on-chain txs (withdraw ~111–140 vB + new funding ~111–154 vB; the [economics doc](../research/invalidation-economics.md)'s TCO prices the pair at 294 vB) + a new deposit token (`SdkError::TokenPaymentRequired` if the SE charges) |

**The deadline, exactly.** `exit_deadline_block = H_deposit_root + initlock` (deposit-anchored,
audit [10]) is a *safe, early* bound over ancestor maturities in one case and **too late** in
another:

- **Exact** — up to the small deposit co-sign→confirmation gap (IVL-INV-10) — when the split
  parent was never transferred: its only stale backup is the deposit backup at `H + initlock`.
- **Too late by `k·interval`** when the parent was transferred `k` times before the split: the
  splitter's own retained backup matures at `H + initlock − k·interval`. Example, deployed
  profile: parent transferred **20 times** then split — the true deadline is `H + 800`, but the
  wallet reports `H + 1,000`: **overstated by 200 blocks ≈ 33 hours**. A receiver who timed an
  exit to the reported number could be clawed back inside that gap. In a deep tree the same
  understatement applies per ancestor: the true deadline is the minimum over **every** ancestor's
  retained backups, not just the immediate parent's (IVL-INV-10, §3c).

This is **audit item [17]**, now half-closed. The shipped half: remediation batch 5 added
`SparkWallet::auto_exit_due(margin_blocks)` — a watchtower pass that force-exits any owned
off-chain sub-coin within `margin_blocks` of its deadline, emitting
`WalletEvent::ExitDeadlineApproaching`; apps call it on an interval (e.g. alongside `claim()`).
The still-open half: the transfer message does not convey ancestor backup locktimes, so the
receiver cannot compute the true minimum locally — the deadline `auto_exit_due` acts on is the
deposit-anchored upper bound. An **online** receiver is always safe — the branch is
locktime-free, broadcast it and the race is over — but an owner offline past the *true* deadline
has no defence. So: either broadcast the branch promptly on receipt, or run `auto_exit_due` with
`margin_blocks = M·interval` for an *assumed* upper bound `M` on any ancestor's pre-split hop
count — `k` itself is exactly what a receiver cannot observe. If no bound on `M` is justifiable,
fall back to eager broadcast (normative: IVL-REQ-16 in
[INVALIDATION-SPEC.md](../INVALIDATION-SPEC.md)).

## 5. Real-world situations

### 5.1 Receiver goes offline for N days holding a sub-coin

The danger height is the root deadline (§4), which is anchored at the *root deposit*, not at the
moment of receipt: a sub-coin whose root was deposited 5 days ago has ~2 days of margin left
under the deployed profile, regardless of when you received it. Offline within the margin:
nothing happens — locktimes hold everyone off. Offline past the *true* deadline (which can be up
to `k·interval` earlier than the reported one — audit [17]): a stale ancestor backup becomes
final and the sender-side holder can claw back the shared root while your locktime-free branch
sits unbroadcast. **Outcome:** safe if N days < remaining root margin; at risk beyond it. If you
plan to be offline, broadcast the branch first (it costs only the pre-committed reserves) or
don't hold sub-coins. One benign wrinkle: any co-descendant of the same tree (say, the splitter
holding the change sub-coin) can broadcast the *shared* branch txs at any time — that
materializes your funding output without your involvement and only improves your position. It is
not a conflict: `ExitBranchConflict` means a *different* transaction spending the same input;
rebroadcasts of the identical branch tx are tolerated (IVL-REQ-13).

### 5.2 SE goes down permanently — day 1 vs day 6 (deployed profile)

The SE's death removes the *cooperative* paths only; every unilateral path is pre-signed and
needs nobody — with one onboarding boundary. The first backup (tx1) is co-signed when the SE's
watcher first sees your funding tx (`clients/libs/rust/src/coin_status.rs:75-79`), so between
broadcasting the funding and that co-sign you have **no** unilateral path at all: an SE that dies
in that window strands the deposit in the 2-of-2 permanently. Fund only after deposit init
succeeds, and treat a missing first backup / `DepositConfirmed` as a reason to stop funding.
(`single_use` coins skip the ladder backup entirely — their branch is their only exit.) Once tx1
exists: Day 1, a fresh never-transferred flat coin's backup has ~856 blocks (~5.9 days) of
`wait_blocks` left (1,000 minus the ~144 blocks day 1 already burned); you call
`unilateral_exit`, it reports `complete: false, wait_blocks: 856`, you re-call after the wait and
the backup confirms — you were first in line the whole time because your locktime is the lowest.
Day 6: same, with ~136 blocks (~23 h) left. Sub-coins: the branch broadcasts *immediately*
either day; only the leaf backup waits its own fresh ladder. **Outcome:** funds fully recovered
on-chain in ≤ `initlock` blocks in both cases; the only difference day 1 vs day 6 is how long
you wait, never whether you win.

### 5.3 SE compromised or colluding with a previous owner

The trust floor, demonstrated by `SDK_E2E=15`: a malicious SE can co-sign a *fresh* transaction
for a previous owner, and a fresh signature carries a *current* locktime — the ladder, which only
orders *pre-signed* stale state, gives no advantage against it. The contest is a plain first-seen
broadcast race. What constrains the collusion: the *API* refuses to raise a budget
(`set_spend_budget` clamps to `min()`, INV-24), so un-terminating a node requires the operator to
rewrite its own database — the clamp is application code, not cryptography, and a compromised SE
controls both the table and the public `GET /statechain/spend_budget` endpoint that reads it.
What that subversion cannot do is hide: anyone who previously recorded `terminal: true` for a
node catches the flip, and a fresh co-signature spending a terminal/single-use/expired node is
publicly attributable misbehaviour — the refusal layer turns SE misconduct on structural nodes
from silent into **auditable**, not impossible. And the receiver's locktime-free branch still
wins any broadcast race it actually enters. What is protected: an online holder
who broadcasts first keeps the funds. What is not: an offline holder against SE+past-owner
collusion. **Outcome:** same residual trust unit as vanilla Mercury or full-collusion Spark;
watch, and exit at the first sign of anomaly.

### 5.4 Fee spike 10× during a unilateral exit

Every pre-signed transaction has a fixed fee decided at signing time; **there is no RBF** —
re-signing would need the SE. Branch txs carry the split's pre-committed reserve (300–2,000
sats; at 155 vB that is ~2–13 sat/vB), so a 10× spike can strand them in the mempool — but
stranded is not lost: the transaction stays valid forever, and once the leaf backup's locktime
matures, the backup — which pays to *your own address* — can be spent by a high-fee child that
CPFPs the entire ancestor package onto the chain. The sharp edge is timing: during a spike, the
race window after stale backups mature is decided by fee, and `SDK_E2E=14` quantifies exactly
that watchtower-vs-clawback fee race. **Outcome:** exit confirms slower and costs you a CPFP
child at spike rates; funds are safe provided the package (or at least the branch) lands before
the earliest hostile maturity — which is why exiting *early*, not at the deadline, is the rule.

### 5.5 A previous owner broadcasts stale state

Three phases, all tested. While `tip < stale locktime`: the network **rejects the transaction as
non-final** — the broadcast achieves nothing and the honest owner needs to do nothing
(`SDK_E2E=13`). At the honest owner's own maturity: the **exclusive window** — for `interval`
blocks (10 ≈ 100 min deployed; 100 ≈ 17 h defaults) the honest backup is the only final tx, so a
broadcast that *confirms* inside it is uncontestable; this window is the structural insight of
the whole ladder. Its guarantee is conditional on landing within it: broadcast early in the
window at a fee that buys a confirmation inside `interval` blocks, and note that `interval` is
also the margin IVL-REQ-3 sizes against reorg-plus-reaction — a reorg across the window boundary
eats into the same head start. After the stale state matures too: a first-seen race that a
watchtower wins by detecting the hostile mempool entry and broadcasting immediately
(`SDK_E2E=14`); for sub-coins, the locktime-0 branch outruns any locktimed backup at any time
before maturity, and a mempool conflict on the branch surfaces as
`WalletEvent::ExitBranchConflict` rather than a silent failure. **Outcome:** an honest owner
whose exit confirms within the exclusive window always wins; one who is merely *watching* wins
the ensuing first-seen race with high probability — `SDK_E2E=14` quantifies the fee race, and
where full-RBF relay prevails the matured-vs-matured contest comes down to fee (§5.4); one who
is offline past the window is gambling.

### 5.6 The hot-potato merchant coin approaching 100 hops

Velocity and calendar burn the **same** budget simultaneously: remaining life
`= initlock − k·interval − (tip − H)`. 90 hops in the first few hours leave the ladder floor at
`H + 100` — the coin dies ≈ 100 blocks (~17 hours) after *deposit*, not after the last hop.
Stretch the same 90 hops over a full day and they cannot even complete: the day's ~144 blocks
overtake the descending floor around hop 85, because receive-side validation hard-rejects any
handover whose backup locktime is at or below the tip (`LocktimeTooLow`,
`lib/src/transfer/receiver.rs`) — the potato stops moving mid-stream. Each receive re-checks the
decrement, and `wait_blocks` makes the shrinkage visible at every hop; at the floor there is no
exclusive window left to hand over — the coin is at end-of-life and must be cooperatively
withdrawn or redeposited (a split does *not* help here beyond the leaf ladder: the root deadline
stands). **Outcome:** the merchant loses nothing but must rotate the coin — one on-chain
withdraw — well before the floor; high-velocity flows should budget hops *and* elapsed blocks
like they budget fees.

### 5.7 Long-hold cold storage

**Statechain coins MUST NOT be treated as cold storage beyond the horizon** (6.9 days deployed,
69 days defaults). A *received* coin has previous owners holding stale backups that mature 10
blocks after yours: cold storage means nobody watches the race, and past the horizon the race is
live — and the §5.3 collusion (SE + a past owner's key share) is live against it too. A
self-deposited, never-transferred coin is different in kind: no counterparty holds stale state,
and the SE *alone* cannot spend a 2-of-2 it holds only one share of (the §5.3 fresh-sign attack
needs a past owner contributing the other share; see the FAQ). Past the horizon such a coin's
own matured backup is a benefit, not an exposure. The reasons it is still a poor vault are
operational: the cooperative paths depend on SE liveness (and any epoch deadline, §5.8), the
pre-signed backup carries a fee frozen at signing time that drifts stale against the fee market
(no RBF, CPFP only — §5.4), and nothing is gained over plain on-chain custody in exchange for
those dependencies. **Outcome:** for holdings longer than the horizon, cooperatively withdraw to
plain on-chain custody; the statechain is a spending layer, not a vault.

### 5.8 Epoch-bounded coins (compliance / limited mandate)

An optional per-coin `epoch_deadline` (unix seconds, set at deposit) makes the SE refuse **new**
co-signatures once its clock passes the deadline (410, ERR-2; `RGB_E2E=7`). Unlike Ark's round
expiry there is no sweep: unilateral exit never needs the SE, so the pre-signed exit path lives
forever. Use it to hard-bound circulation — a custodial mandate ending on a date, a
compliance-scoped instrument, a bounded delegation. **Outcome:** at the epoch the coin simply
stops moving off-chain; the holder exits unilaterally (or withdrew earlier); nobody, including
the SE, can confiscate it.

### 5.9 Receiving as a fresh user with zero on-chain footprint

`SDK_E2E=16`: a brand-new wallet with no UTXOs, no deposits, and no chain history receives a
sub-coin off-chain and is a first-class owner. Its exit material, all local: the key share, the
latest pre-signed leaf backup, the full branch rows (`branch-<id>`, root-first), and the ancestor
list (`parents-<id>`). That bundle is a complete, SE-independent exit: broadcast branch, wait
leaf ladder, broadcast backup. Note the corollary: **a mnemonic alone does not restore it** —
the branch rows are not derivable from a seed, and a sub-coin restored without them is explicitly
rejected at exit time with a "restore the recovery bundle" error (audit [20]) rather than failing
opaquely. **Outcome:** full self-custody from the first second, zero on-chain onboarding cost —
but the recovery bundle, not just the seed, is what must be backed up.

### 5.10 The griefing case: a replayed owner-auth signature (audit [15], closed)

Owner authentication was historically a static signature over `sha256(statechain_id)`,
replayable across owner endpoints by anyone who observed one request (a logging proxy, a TLS
terminator, an SSP in the path). The worst replay: `set_spend_budget` with `remaining = 0`,
which — because the budget is monotonic and irreversible — bricks the coin to
**unilateral-exit-only**. What the attacker never got: the funds. The ladder and branch need no
SE, so the owner exits on-chain and keeps everything; the loss was the off-chain utility of the
coin plus exit fees plus the `wait_blocks` wait. **Closed in audit UPDATE 3:** the two
irreversible endpoints (`set_spend_budget`, `withdraw/complete`) now demand a single-use,
endpoint-bound challenge — a 5-minute SE nonce (`GET /auth/challenge/<sid>`) signed as
`sha256(nonce ‖ endpoint)` and atomically consumed — so a captured signature can no longer be
replayed or redirected. The lower-harm `transfer`/`sign` endpoints deliberately keep the static
auth (harm bounded by the coin protocol and the enclave's nonce consume). **Outcome:** the brick
attack is no longer possible; the wait-cost arithmetic below survives as the price of any
*voluntary* forced-unilateral situation (e.g. SE failure).

## 6. The UX perspective

**What the wallet surfaces.** `estimate_exit_cost(coin)` returns
`{branch_txs, branch_vbytes, backup_vbytes, total_vbytes, wait_blocks, exit_deadline_block}` with
`fee_sats_at(rate)` for live fee math; `unilateral_exit` returns per-coin
`ExitStatus{complete, wait_blocks}` and is idempotently re-callable. Events:
`WalletEvent::DepositConfirmed` (funding reached target depth), `TransferClaimed`,
`BalanceUpdate`, `TokenTransferClaimed`, and `ExitBranchConflict` (someone is racing your exit
branch — fee-bump/alert, do not assume the exit landed). Deposit-time cost surfaces as
`SdkError::TokenPaymentRequired{token_id, deposit_address, fee_sats}` — pay and retry.

**What a user must do, and when.** Usually: **nothing**. The exhaustive list of moments that
require action:

| Trigger | Deadline | Action |
|---|---|---|
| `wait_blocks` approaching 0 on a held coin | before your own locktime `L_k` | spend, coop-withdraw, or exit |
| Holding a sub-coin, going offline | before `exit_deadline_block` minus an assumed `M·interval` margin (IVL-REQ-16; [17]) | broadcast the branch first, or have a watchtower run `auto_exit_due(M·interval)` |
| `ExitDeadlineApproaching` event | within your margin | `auto_exit_due` already broadcast the branch — confirm it lands |
| `ExitBranchConflict` event | immediately | CPFP/alert/re-attempt; treat the exit as contested |
| `TokenPaymentRequired` on deposit | before depositing | pay `fee_sats`, retry |
| SE unreachable and you want out | none — any time | `unilateral_exit`, re-call after `wait_blocks` |

**If they do nothing:** a flat coin is untouchable by anyone until the *current owner's* locktime
(≈ horizon minus `k·interval/144` days), then exclusively theirs for `interval` blocks, then
exposed to whichever past owner shows up first. A sub-coin is additionally exposed from the root
deadline onward. Doing nothing is safe precisely up to those heights and a gamble after them.

**What a watchtower must watch** (per coin): (1) any mempool/chain spend of the funding outpoint
that is not the owner's own tx → race immediately with the branch (sub-coin) or matured backup
(flat); (2) tip vs the owner's backup locktime → broadcast at maturity, inside the exclusive
window; (3) tip vs `exit_deadline_block` minus a safety margin (the assumed-`M·interval` bound)
→ force-broadcast the branch — shipped as `auto_exit_due(margin_blocks)`, which does exactly this
and emits `ExitDeadlineApproaching` (audit [17], batch 5); call it on an interval;
(4) persistence of already-broadcast low-fee exit txs → rebroadcast if purged,
CPFP when the backup is spendable. An offline-capable watchtower must cache `initlock` and the
root deposit height — never derive the deadline as `leaf_locktime + interval` (that formula is
the bug [10] fixed). The normative version of these rules is **IVL-REQ-16** in
[INVALIDATION-SPEC.md](../INVALIDATION-SPEC.md).

**Time-to-money, per flow** (deployed profile):

| Flow | On-chain txs | Wait |
|---|---|---|
| Deposit → spendable | 1 (your funding tx) | confirmation target (~1 conf + SE registration) |
| Off-chain receive | 0 | seconds (API round-trips + electrum/SE validation queries) |
| Cooperative exit | 1 per coin (~111–140 vB) | ~1 conf (~10 min–hours by feerate) |
| Unilateral exit, flat coin | 1 (112 vB backup) | `wait_blocks` ≤ initlock (≈ 6.9 d fresh, less by 10 blocks per past hop) + 1 conf |
| Unilateral exit, sub-coin | N branch (~155 vB each) + 1 backup | branch: now; backup: its own fresh ladder |

The fresh-ladder consequence deserves emphasis: **unilaterally exiting a sub-coin you *just*
received waits ≈ `initlock`** — the leaf backup was anchored at the split height, so its ladder
is nearly full (~7 days deployed). The branch confirms immediately (securing the funds against
every ancestor), but the money reaches your plain address only after the leaf wait. Coop exit,
when the SE is alive, is always the fast path.

**Sharp edges, honestly, as of 2026-07** (post remediation UPDATE 3: all 11 HIGH findings and
all griefing/DoS MEDIUMs — incl. the [15] brick, §5.10 — fixed + verified; mainnet still gated on
the SGX enclave rebuild, a re-audit, and a third-party audit): still open is the
conveyed-locktimes half of [17] — the reported deadline can be `k·interval` too late, which the
`auto_exit_due` margin must absorb; no RBF on any pre-signed tx (CPFP only); recovery
bundle (backup ladder + branch rows) is not seed-derivable — for any coin, flat included (H3);
fresh-sub-coin unilateral exits wait ~`initlock`; plain-BTC combine is a lib-level primitive
only, so fragmentation from repeated splits currently costs one withdraw tx per coin
("combine-then-withdraw" batching is a future optimization, not a shipped path).

## 7. FAQ

**Can the SE steal my coin?** Not alone: the coin is a 2-of-2 and the SE never holds your key
share. Colluding with a *previous owner* it can fresh-sign a competing spend and force a
broadcast race (§5.3) — the trust floor shared with every statechain design. It cannot forge
terminal state (monotonic), and misbehaviour on structural nodes is publicly queryable.

**Can the SE freeze my funds?** It can refuse to co-sign (or die, or be legally compelled),
which kills the *cooperative* paths only. Unilateral exit is pre-signed and SE-independent;
worst case is the `wait_blocks` wait. Freeze ≠ seize: at most it converts your coin into an
on-chain exit ticket. The one exception is the onboarding window: the unilateral guarantee
begins when the SE co-signs the first backup at deposit detection — an SE that refuses or dies
*before* that strands the funding in the 2-of-2 (§5.2), so fund only after deposit init succeeds.

**What if I lose the branch rows?** Losing them is one instance of losing `wallet.db`, and **no**
statechain coin — flat or sub — restores from the mnemonic alone (review H3,
`rust-sdk/src/wallet.rs`): the seed rebuilds the key hierarchy but not the per-coin exit material
— statechain ids, the pre-signed backup ladder, the `branch-*`/`parents-*` rows — which lives
only in the wallet database and which the blind SE cannot re-serve after a claim. Losing
`wallet.db` with only the mnemonic is total loss of every off-chain coin. A sub-coin missing its
branch rows at least fails loudly: exit raises an explicit "restore the recovery bundle" error
(audit [20]). Back up the bundle (`export_recovery_bundle`) for everything and re-export after
every transfer, claim, or split; the branch rows are just the sub-coin-specific part of it (§5.9).

**Can two people be handed the same coin?** The SE binds one challenge per signing nonce
atomically (nonce reuse with a different message → 409; INV-23, `SDK_E2E=12`) and refuses
conflicting co-signs, so it cannot be walked into signing two states. A *malicious* SE could
try; the receiver-side checks (ladder decrement, `num_sigs`, terminal ancestors, branch
validation) are what a second "owner" cannot satisfy without the SE visibly double-signing.

**Why locktime 0 on split transactions?** Because the branch must beat every deposit-anchored
stale backup unconditionally. Any nonzero branch locktime can end up *above* an aged parent's
backup, letting the stale state mature first and win (the arithmetic bug behind audit H5). A
height-0 branch is broadcastable now and sits below every backup by construction (INV-4);
receivers reject any branch tx with locktime > tip (audit [11]).

**Why absolute locktimes, not relative (like Spark)?** Spark's relative CSV ladder starts each
leaf's clock at its parent's *confirmation*, so leaves keep a full window post-broadcast — but
the ladder still decrements per transfer (2000 initial, −100/hop) and demands `renew_leaf` churn
(operator-assisted, mandatory at ≤300 blocks — constants per
[protocol-notes.md](../research/protocol-notes.md)). Our absolute ladder is computable at
signing time — every deadline is
a fixed height, ideal for watchtowers — needs zero renewal traffic, and splits mint *fresh*
sub-ladders so depth never consumes lifetime. The trade: our coins age in wall-clock time even
when idle; Spark's age per hop but rot without renewal service. We chose predictable expiry over
operator-dependent immortality.

**What does "terminal" mean — can it be undone?** Terminal = the SE will never co-sign this
statechain again (`finalized ≥ sig_budget`). `set_sig_budget` is `min()`-monotonic (INV-24), so
no request *via the API* — from the owner or an attacker — raises it. An operator subverting its
own database could (the clamp is server application code, §5.3), but the flip contradicts every
`terminal: true` answer the public `GET /statechain/spend_budget/<id>` endpoint served before —
observers who record those answers hold the receipt. Honest-server permanent by design;
compromised-server detectable, with the receiver's branch as the backstop.

**Who pays exit fees?** The *splitter* pre-pays each branch tx's fee (the 300–2,000-sat reserve
deducted from change at split time); the *exiter* pays the backup's fixed pre-signed fee and any
CPFP top-up at exit-time rates. Cooperative exits pay normal on-chain fees at live rates.

**What if the mempool purges my pre-signed tx?** Nothing is lost: pre-signed transactions never
expire and rebroadcast is free. A purge only delays; the watchtower's job includes rebroadcasting
and, once the backup is spendable, CPFPing the package (§5.4).

**Is there any scenario where an honest, online user loses funds?** Within the model, one: the SE
colludes with a past owner *and wins the broadcast race* against the online user (§5.3) — online
users with a working fee bump win that race in practice, but it is a race, not a proof. Plus one
boundary case with no adversary at all: an SE that dies before co-signing the first backup
strands the deposit in the 2-of-2 (§5.2's onboarding window). Every other adversary — stale
broadcasters, dead SEs, fee spikes, griefers — loses to an online user mechanically. Offline is
where the qualifiers pile up (§5.1, §5.7, [17]).

**What happens at hop 100?** It cannot complete. The 100th backup's locktime is exactly the
deposit anchor `H` — at or below every possible tip — and receive-side validation **rejects**
(not merely warns about) any incoming ladder whose lowest locktime is ≤ tip: the claim fails
with a validation error (`LocktimeTooLow`, `lib/src/transfer/receiver.rs`). In practice the last
acceptable hop comes earlier still, since every elapsed block eats the same floor (§5.6). The
coin must be cooperatively withdrawn or redeposited; its hop capacity is spent even if its
calendar isn't.

**Does splitting extend my coin's life?** The *leaf's* hop budget and ladder, yes — fresh
`initlock` at split height. The *tree's* off-chain lifetime, no: the root deadline
`H_deposit + initlock` never moves. Only branch materialization or withdraw+redeposit reset the
wall clock (§4).

**Can a previous owner do anything at all before their locktime?** No. Their backup is non-final
(the network rejects it), and the SE will not co-sign for them — key rotation plus
single-active-state means the SE only answers the current owner. Their move exists only after
their locktime matures, which the current owner's earlier window pre-empts.

**What if my watchtower dies?** You inherit its duties on your next wake: check no funding
outpoint was spent hostilely, check `wait_blocks` and the (margin-adjusted) deadline. Exposure
is limited to coins whose exclusive window or root deadline passed during the outage — the same
heights as §6's "if they do nothing" — not to everything you hold.

## 8. Comparison recap: over-time behaviour

| | **Ours** | **Spark** | **Ark / Second** | **Mercury (vanilla)** |
|---|---|---|---|---|
| Lifetime bound | Absolute: `initlock` blocks from deposit (6.9 d / 69 d), 100 hops; per-coin epoch optional | Relative ladder 2000, −100/hop; effectively unbounded *if* renewed | Round expiry (~weeks), hard | Absolute ladder, one horizon, no refusal layer |
| Renewal | **None by design**; extend via self-split (leaf only), branch materialization, or withdraw+redeposit | `renew_leaf` churn at ≤300 blocks, needs SO | Mandatory per-round refresh participation | None |
| Idle coin over time | Ages by calendar; must exit before horizon | Ages per hop; rots if renewal missed | Swept to server if round missed | Ages by calendar |
| Operator dies | All exits pre-signed; wait ≤ `initlock`, funds whole | Unilateral path exists; timelock race | Exit window critical; miss it → server sweeps | Same as ours (ladder only) |
| Stale state over time | Non-final → exclusive window → watched race; branches beat ancestors before root deadline | Timelock race; operator key-deletion honest-1-of-n | Dies at expiry (the same knife that threatens users) | Timelock race only |
| Operator misbehaviour visibility | Terminal state publicly auditable per node | Not queryable per node | Round tree is public | None |
| Offline requirement | Online (or watched) after your maturity/root deadline; nothing before | Online for renewals, forever | Online every round, forever | Online after maturity |

Further reading: normative requirements in [INVALIDATION-SPEC.md](../INVALIDATION-SPEC.md);
fee/size tables and feerate scenarios in
[invalidation-economics.md](../research/invalidation-economics.md); the short comparison in
[invalidation.md](invalidation.md); exit mechanics in [exits.md](exits.md); system spec
[SPEC.md](../SPEC.md) (REQ-16/18/25, INV-4/20/23/24/25, ERR-1/2/3/7/12); audit trail in
[AUDIT-2026-07.md](../AUDIT-2026-07.md). Test evidence: `SDK_E2E=7/8/10/12/13/14/15/16/17`,
`RGB_E2E=3/4/6/7`, chaos `SDK_E2E=22` (dispatch in `clients/tests/rust/src/main.rs`).
