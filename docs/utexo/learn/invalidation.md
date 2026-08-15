# Old-state invalidation & UTXO granularity — design and comparison

> Short comparison page. For the full explainer — lifecycle walkthroughs, over-time behaviour,
> failure scenarios, UX, and FAQ — see [invalidation-deep-dive.md](invalidation-deep-dive.md);
> the shipped ladder is specified in [PROTOCOL.md](../PROTOCOL.md) (§5.2–§5.9), the normative
> requirements in INVALIDATION-SPEC (retired 2026-08-15), and the pricing in
> invalidation-economics (retired 2026-08-15).

How do you stop a previous owner (or a malicious current owner) from using old off-chain state?
Every L2 answers differently. This page compares the designs we reviewed — Spark, Ark (and
Second's implementation), SuperScalar, vanilla Mercury — and specifies ours, which layers an
**SE hard-refusal** on top of a **relative-timelock (CSV) exit ladder**.

## One protocol, two coin shapes

Read this first — it decides which mechanic below applies to a given coin.

There is **one protocol**. `claim()` establishes a TES-R exit ladder — **T**rigger → **E**xtension →
**S**tate, all relative CSV, all pre-signed and **un-broadcast** — for every fresh confirmed **root**
coin, unconditionally. There is no protocol-version field and no legacy lane. But not every coin is
laddered, by design:

- **Laddered** — every plain BTC deposit. Old state is invalidated by
  **replace-by-lower-timelock at the consensus level**, and because relative locks do not tick until
  their parent confirms, **an idle coin never ages**: no calendar deadline, no expiry, **0 vB of idle
  rent**.
- **Un-laddered** — an RGB **carrier** is deliberately never laddered (a plain tier spend would sweep
  the sats and destroy the allocation — the terminal-freeze rule, [PROTOCOL.md §5.10](../PROTOCOL.md),
  pinned by `sdk52`), and a **split sub-coin whose funding output is un-broadcast** cannot root a
  trigger (the trigger would have no prevout to spend). These coins keep the **signed-once,
  absolute-nLockTime backup chain** and transfer by backup-chain handover.

The un-laddered shape is **load-bearing for RGB assets — current, not deprecated**. Decrementing
absolute locktimes, root deadlines and "materialize before the deadline" are all still real mechanics
— but *only* for that shape. Every claim below is tagged with the shape it belongs to; if you read a
deadline into a laddered coin, or a zero-rent forever into a carrier, you have crossed the wires.

## The designs

| System | Invalidation mechanism | Lifetime/renewal | Failure mode |
|---|---|---|---|
| **Spark** | Relative decrementing timelocks per transfer (2000 → −100) + operator key-deletion (honest-1-of-n); split nodes are zero-timelock, spent once by SO policy | Renewal (`renew_leaf`) at ≤300 blocks — churn, needs the SO | Full operator collusion can sign old state; timelock race decides |
| **Ark / Second** | VTXOs expire at round end; old state dies by **expiry**; server co-signs each round's tree | Refresh each round (~weeks) — mandatory participation | Miss the exit window → funds sweep to the server; liveness-critical |
| **SuperScalar** | Decker-Wattenhofer decrementing nSequence (bounded update counter) + laddered timeout trees + operator reclaim | Ladder epochs; limited number of in-place updates | Update counter exhausts; epoch deadline forces exit |
| **Mercury (vanilla)** | Absolute decrementing nLockTime backups: current owner's backup unlocks first | Coin expires as the ladder floor approaches the tip; on-chain re-anchor to survive | Old owner + SE collusion signs anything; ladder only orders honest broadcasts |
| **Ours — laddered** | **Relative-CSV ladder (TES-R)**: a transfer co-signs a fresh state one δ *lower* than the one it replaces, so the current owner's state always matures first; renewal replaces the whole extension horizontally at a lower CSV, making every older extension **unconfirmable** — **plus** SE single-active-state / terminality refusal as an independent second layer | **Unbounded and off-chain**: lower-CSV extension renewal (576 transfers per depth level), then off-chain self-split rollover at epoch exhaustion. Idle coins never age at all | SE collusion with the old owner (the statechain trust unit, unchanged) — otherwise a *race*, but one that cannot start until a **public on-chain trigger** gives ≥144 blocks (~1 day) of notice |
| **Ours — un-laddered** | Mercury's absolute nLockTime ladder (as above) **+ SE terminal-spend refusal per structural node + optional single-use + optional epoch deadline** | Fresh `initlock` ladder per sub-coin — depth does NOT consume lifetime; the root deadline is real and must be beaten by materialization | Old owner + SE collusion; plus a real clawback window if nobody materializes before the deadline |

**What changed against vanilla Mercury.** Absolute timelocks age while un-broadcast, so the defense
has to be renewed on the calendar — 112 vB per coin per ~1,008 blocks ≈ **5,840 vB per coin-year**,
which is ~11% of all Bitcoin block space at 1M coins and physically impossible at 10M. Relative
(BIP-112) locks on un-broadcast transactions **do not tick until a parent confirms**, so the clock
starts on *attack* rather than on deposit. That single substitution is what deletes the rent and the
deadlines. Full arithmetic: [PROTOCOL.md §2](../PROTOCOL.md).

## Our model, precisely

### Laddered coins (every plain deposit)

The funding UTXO **F** is the only thing on-chain. Above it sits a pre-signed, un-broadcast tree:

```
F  (on-chain)
└─ T   TRIGGER    — v3/TRUC + P2A, NO timelock, signed ONCE at deposit, never re-signed
   └─ X_m  EXTENSION  — relative CSV E_m = 720 − m·36; renewal replaces it horizontally
      └─ S_k  STATE   — relative CSV Δ_k = 1440 − k·36; decrements once per transfer
```

1. **A transfer invalidates the old state at consensus.** The new owner's state carries a CSV one
   δ = 36 blocks (~6 h) **lower** than the state it replaces, so it always matures first
   (replace-by-lower-timelock, Decker-Wattenhofer at one dedicated tier). The superseded state is
   **disclosed** to the receiver and counted: the receiver's census checks the SE's public signature
   counter against the exact expected tree size, so a hidden extra co-signed state shows up as a
   count mismatch. Evidence: `sdk40 PART 2` (a stale ladder dies outright at consensus once its
   prevout is gone), `sdk41` (after a transfer the receiver co-signs and exits a full ladder over the
   same funding outpoint while the sender is cryptographically locked out), `sdk46`/`sdk47` (the
   census against the SE's real counter, rejecting a hidden extra signature).
2. **Renewal invalidates a whole epoch, off-chain.** When the state CSV would fall to its floor, the
   SDK co-signs a fresh extension `X_{m+1}` at a strictly lower CSV. It undercuts every older
   extension in the race for `T.out[0]`, so every pre-renewal state now hangs on a parent that can
   never confirm. **Zero on-chain bytes.** At epoch exhaustion the coin rolls over off-chain into a
   fresh level with a fresh 576-hop budget. `sdk43` drives renew → rollover → renew past exhaustion
   with the funding outpoint untouched throughout; `sdk44` pins the schedule arithmetic.
3. **Nothing matures until someone broadcasts T.** T has no timelock, and BIP-112 relative locks only
   start counting once the parent confirms. So an idle coin — and an entire idle split DAG — never
   ages. There is **no calendar deadline** on a laddered coin, no "exit before your floor", no forced
   materialization, and `auto_exit_due` has nothing to act on (it computes a deadline only for coins
   that have an exit *branch*, i.e. the un-laddered shape). `sdk40 PART 1` shows real consensus
   enforcing this: an extension is rejected before E confirmations of T, a state before Δ of X.
4. **The honest defence is a walk, not a race to broadcast one tx.** A unilateral exit broadcasts the
   tiers in order, waiting out each relative timelock (`sdk50`; the keyless watchtower does the same
   walk in `sdk45`/`sdk51`). If someone hostile broadcasts T first, the tower either asks the SE for a
   **cooperative de-trigger** — a fresh no-timelock spend, ~111 vB, coin fully restored, after which
   the griefer's extension can never confirm (`sdk40 PART 2`) — or simply broadcasts the
   strictly-lowest-CSV current state and wins by ≥36 blocks per tier (`sdk51`, end-to-end against a
   real hostile trigger).
5. **`refresh` is the re-anchor primitive, not a deadline reset.** One on-chain transaction (~112 vB)
   moves the coin to a fresh funding outpoint and mints a new ladder at depth 0. It exists to cap
   *depth* (default policy: compact when depth > 3, priced into that transfer's fee) — not to buy
   lifetime, because lifetime is unbounded. `sdk30` covers both fee models (user-paid and
   operator-sponsored).

### Un-laddered coins (RGB carriers, split sub-coins over un-broadcast funding)

Mercury-native, and unchanged: the first backup unlocks at `tip + initlock` (1,000 blocks); every
transfer hands the new owner a backup unlocking `interval` (10) earlier, so the current owner always
wins an honest exit race. Each sub-coin gets a **fresh** `tip + initlock` ladder at creation — a
genuine improvement over Spark, where the tree shares one decrementing budget and renewal churn rises
with activity: here, splitting doesn't spend the children's lifetime.

Because these locktimes are absolute, this shape **does** keep a root deadline: a received carrier
owes one materialization (broadcast the colored branch only — never the sats-sweeping backup) before
an ancestor's stale backup matures. `auto_exit_due` (default on, 288-block margin) does this
automatically, using the *deposit-anchored* `exit_deadline_block` from `estimate_exit_cost`. Evidence:
`sdk34` (a watchtower materializes a carrier before its deadline), `sdk32` (the residual clawback
window if nobody does), `sdk39` (depth-2 colored branch exit, allocation preserved).

### Structural nodes (split/combine parents) — two independent layers

1. **SE terminal-spend budget** (`POST /statechain/spend_budget`, owner-signed and irreversible;
   public `GET /statechain/spend_budget/<id>`). Right before co-signing a split, the SDK sets the
   parent's budget to *exactly one more co-signature*. The split consumes it; from then on the SE
   refuses **everything** on that parent — a later withdraw, transfer, renewal or fresh backup cannot
   be signed even by the legitimate owner. This is Spark's "one spend per node" made explicit and
   **publicly verifiable**: anyone (e.g. the receiver of a piece) can query the endpoint and see
   `terminal: true`. Live evidence: `sdk58` asserts the parent **is** terminal after an in-ladder
   split (budget consumed by `SP`) and **rejects** a bundle whose claimed parent is non-terminal
   (case H); the flag is read directly by `sdk04`, `sdk31`, `sdk32`, `sdk60`.
2. **The ladder as fallback.** Even if the SE misbehaved, the parent's remaining pre-signed state is
   ordered *below* the child's in the timelock race (laddered), or locktimed above the locktime-free
   branch (un-laddered) — an honest receiver who exits in time wins. Defense in depth: refusal
   (instant, no race) + timelocks (race, bounded).

### Received split children are first-class, and deliberately *not* terminalized

A claimed child completes the standard SE **key handover**: the child's aggregate key `A_child` is
invariant across the share rotation (which is exactly what keeps its pre-signed exit chain valid),
and the sender is **permanently locked out**. The child is therefore payable onward off-chain — whole
(`child_retransfer`) or split again (`child_in_ladder_pay`, a depth-2 ancestors chain). Each hop costs
one co-signature and discloses exactly one superseded state, counted by the receiver's census.
Evidence: `sdk60` (alice → bob → carol, funding outpoint unspent throughout) and `sdk17` (partial
second hop). Details in [CHILDREN.md](../CHILDREN.md).

The **one** exception is the Lightning-latched piece: it sits unclaimed past the pending-transfer
lock's window (the SSP settles on its own schedule), so it is terminalized instead — a permanent
lockout. See [LIGHTNING.md](../LIGHTNING.md); Lightning runs both directions on the ladder via a
HODL-invoice latch (`sdk63` pay, `sdk64` receive, `sdk65`/`sdk67` non-exact, `sdk66`/`sdk68` failure
and rollback).

### Optional per-coin hard bounds (SE-side, off by default)

- **Epoch deadline.** A coin MAY carry an `epoch_deadline`; past it the SE refuses *new*
  co-signatures (HTTP 410, `RGB_E2E=7`). Like Ark's round expiry it bounds state lifetime — but
  unlike Ark, **unilateral exit stays live forever** (it needs no SE signature), so funds can never be
  swept by missing a window. Coins with `epoch_deadline = NULL` — the default — are co-signable
  indefinitely.
- **Single-use coins.** Deposit-time flag for one-shot nodes (skip the backup, one co-signature ever)
  — the strictest form, used by the RGB DAG flows (`RGB_E2E=4`).

## What we took from each system

- **Spark**: zero-timelock structural nodes; one-spend-per-node (now SE-enforced + queryable); and,
  independently, its zero-idle-footprint benchmark — which TES-R matches with **consensus-level**
  invalidation instead of 1-of-n key-deletion trust.
- **Ark/SuperScalar**: explicit lifetime bounds are *available* (epoch deadline) — but never
  mandatory, and never with expiry-sweep risk. Nothing in this protocol ever pays the operator by
  timeout.
- **Decker-Wattenhofer**: replace-by-lower-timelock — applied at one dedicated tier, so tree depth
  stays constant across all epochs instead of exhausting an update counter.
- **Mercury**: the pre-signed, SE-independent exit as the trustless floor under everything, and the
  absolute-locktime ladder itself, which remains the invalidation mechanism for the un-laddered shape.
- **Rejected**: revocation keys (grow the collusion surface), mandatory round refresh (liveness
  cliff), bounded DW update counters, and shared-factory roots (an operator-chooseable n-of-n that
  never rotates — see [PROTOCOL.md §4](../PROTOCOL.md)).

## UTXO granularity

Spark leaves are fixed denominations; exact amounts need SSP swap pools. Ark re-mints change each
round. Here, **exact amounts are a native off-chain operation** — one SE-co-signed transaction mints
any piece + change, chainable to depth, each piece a full coin. The mechanics differ by shape:

- **Laddered — the in-ladder split.** A split is a **state tier** `SP` spending `X_m.out[0]`: a
  *descendant* of the trigger, never a rival for the funding outpoint `F`. That is what closes the
  theft vector a naive split would open — a past owner's retained no-timelock trigger has nothing to
  race. Each child hosts its **own** extension + state tiers, so the admission floor is
  `min_child_value = 2·(committed_fee + P2A) + dust` = **1,306 sats at 2 sat/vB**, checked *before*
  the parent is terminalized. Attack-proven by `sdk58` (11 adversarial cases, all REJECT) and driven
  end-to-end by `sdk59`.
- **Un-laddered — the colored/backup-chain split.** 1-sat resolution above a 330-sat dust floor, with
  the smallest *mintable* piece at ≈442 sats (330 + its own 112-vB backup fee at 1 sat/vB) and a
  1,500-sat packaging floor for token pieces.

Granularity is strictly better than both comparators in every shape. Exact bounds, token packaging
and pricing: the granularity pack — [deep dive](granularity-deep-dive.md),
GRANULARITY-SPEC, economics.

## Unilateral exit economics

| Coin | Exit txs | vsize | Fee model | Wait |
|---|---|---|---|---|
| **Laddered, flat** | 3 pre-signed tiers (T → X_m → S_k) | 372 vB base; up to 828 vB with a P2A fee child on each tier in a spike | each tier carries a **committed** ~2 sat/vB fee (≈744 sats total, fixed at signing) and relays standalone; in a spike attach a ~152-vB P2A child per tier at the market rate | sequential relative CSV, E_m then Δ_k: worst **2,160 blocks ≈ 15 d** on a fresh coin, shrinking 36 blocks per hop and per renewal. The clock starts only when T is broadcast |
| **Laddered, in-ladder child at depth d** | 3 + 2d | ≈ 124·(3+2d) vB (depth-1 ≈ 620 vB) | as above | ≤ (d+1)·2,160 blocks; default depth cap 3 (≈60 d worst case), which the re-anchor exists to enforce |
| **Un-laddered, flat carrier** | 1 (backup) | 112 vB | committed at co-sign (`max_fee_rate` default 1 sat/vB ⇒ 112 sats); CPFP-bumpable from the backup's own output | absolute nLockTime: ≤ `initlock` (1,000 blk ≈ 7 d), −`interval` per handover |
| **Un-laddered, depth-N sub-coin** | N+1 (N branch txs + backup) | 112 + N·155 vB (depth-1 = **267 vB**) | branch fees pre-committed by the splitter (`clamp(parent/100, 300, 2000)` sats); backup as above | the branch is locktime-free and confirms immediately; the backup then waits ≈ `initlock` from the split tip |

Fees at other rates: multiply the vsize column (e.g. depth-1 un-laddered = 534 sats @2 sat/vB,
8,010 @30). Recorded and modelled figures, with the USD tables and the fee-spike analysis, are in
invalidation-economics (retired 2026-08-15). The laddered exit is driven live
by `sdk50` (the flat tier walk) and `sdk58`/`sdk59` (in-ladder split children); the un-laddered
depth exit by `sdk39` (depth-2 colored branch, allocation preserved).

**The API.** For an un-laddered coin, `estimate_exit_cost(coin)` decodes the actual stored pre-signed
transactions and returns real numbers — `branch_txs`, `total_vbytes`, `fee_sats_at(rate)`,
`wait_blocks` (when the exit *completes*) and `exit_deadline_block` (the *safety* deadline: when an
ancestor could start racing you — the number a watchtower must act on). A laddered coin has no branch,
so it returns no deadline: there is none. `unilateral_exit` handles both shapes without the caller
choosing: on a laddered coin it walks the tier chain idempotently, advancing as far as maturity allows
and reporting the blocks left until the next tier matures — call it again as the chain advances; on an
un-laddered coin it broadcasts the locktime-free branch immediately and reports the remaining backup
wait instead of failing.

**The traded property, stated plainly.** For laddered coins, vanilla Mercury's unconditional ~7-day
no-watch window is exchanged for *perpetual but alarm-driven* watching. Nothing expires and nothing
sweeps to the operator, so missed liveness is never confiscation-by-design; but no theft transaction
can even become valid until ≥144 blocks (~1 day) after a **publicly visible** on-chain trigger spends
F — strictly more defender notice than a silent calendar maturity. Towers are keyless, delegable and
cheap ([TRUST-MODEL.md](../TRUST-MODEL.md), [PROTOCOL.md §5.13](../PROTOCOL.md)).
