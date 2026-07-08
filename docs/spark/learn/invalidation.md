# Old-state invalidation & UTXO granularity — design and comparison

> Short comparison page. For the full explainer — lifecycle walkthroughs, over-time behaviour,
> failure scenarios, UX, and FAQ — see [invalidation-deep-dive.md](invalidation-deep-dive.md);
> the normative requirements live in [INVALIDATION-SPEC.md](../INVALIDATION-SPEC.md).

How do you stop a previous owner (or a malicious current owner) from using old off-chain state?
Every L2 answers differently. This page compares the designs we reviewed — Spark, Ark (and
Second's implementation), SuperScalar, vanilla Mercury — and specifies ours, which layers an
**SE hard-refusal** on top of Mercury's timelock ladder.

## The designs

| System | Invalidation mechanism | Lifetime/renewal | Failure mode |
|---|---|---|---|
| **Spark** | Relative decrementing timelocks per transfer (2000 → −100) + operator key-deletion (honest-1-of-n); split nodes are zero-timelock, spent once by SO policy | Renewal (`renew_leaf`) at ≤300 blocks — churn, needs the SO | Full operator collusion can sign old state; timelock race decides |
| **Ark / Second** | VTXOs expire at round end; old state dies by **expiry**; server co-signs each round's tree | Refresh each round (~weeks) — mandatory participation | Miss the exit window → funds sweep to the server; liveness-critical |
| **SuperScalar** | Decker-Wattenhofer decrementing nSequence (bounded update counter) + laddered timeout trees + operator reclaim | Ladder epochs; limited number of in-place updates | Update counter exhausts; epoch deadline forces exit |
| **Mercury (vanilla)** | Absolute decrementing nLockTime backups: current owner's backup unlocks first | Coin expires as the ladder floor approaches the tip | Old owner + SE collusion signs anything; ladder only orders honest broadcasts |
| **Ours** | Ladder (per-coin, fresh) **+ SE terminal-spend refusal per structural node + optional single-use + epoch deadline** | Fresh `initlock` ladder per sub-coin — depth does NOT consume lifetime; epoch bounds each coin explicitly | SE collusion with the old owner — same unit as Mercury/Spark-with-full-collusion, but now auditable per node |

## Our model, precisely

**Flat coins (deposits):** Mercury-native. First backup at `tip + initlock` (1000 blocks); every
transfer hands the new owner a backup unlocking `interval` (10) earlier. The current owner always
wins an honest exit race.

**Structural nodes (split/combine parents):** two independent layers.

1. **SE terminal-spend budget** (`/statechain/spend_budget`, migration 0005). Right before
   co-signing a split, the SDK sets the parent's budget to *exactly one more co-signature*. The
   split consumes it; from then on the SE refuses **everything** — a later withdraw, transfer, or
   fresh backup on the parent cannot be signed even by the legitimate owner. This is Spark's
   "one spend per node" made explicit and **publicly verifiable**: anyone (e.g. the receiver of a
   branch coin) can query `GET /statechain/spend_budget/<id>` and see `terminal: true`.
   Verified by `SDK_E2E=8`: a post-split withdraw of the parent is refused with
   `spend budget exhausted`.
2. **The ladder as fallback.** Even if the SE were to misbehave, the parent's only *pre-signed*
   old state is its deposit backup, locktimed above the (locktime-free) branch — an honest
   receiver who exits in time wins. Defense in depth: refusal (instant, no race) + timelocks
   (race, bounded).

**Sub-coins (leaves):** each gets a **fresh** `tip + initlock` ladder at creation. This is a
genuine improvement over Spark's model, where the tree shares the decrementing budget and
renewal churn rises with activity: here, splitting doesn't spend the children's lifetime.

**Epoch deadline (optional, per coin):** the SE refuses *new* co-signatures past
`epoch_deadline` (`RGB_E2E=7`). Like Ark's round expiry it bounds state lifetime — but unlike
Ark, **unilateral exit stays live forever** (it needs no SE signature), so funds can never be
swept by missing a window.

**Single-use coins (optional):** deposit-time flag for one-shot nodes (skip backup, one
co-signature ever) — the strictest form, used by the RGB DAG flows (`RGB_E2E=4`).

## What we took from each system

- **Spark**: zero-timelock structural nodes; one-spend-per-node (now SE-enforced + queryable).
- **Ark/SuperScalar**: explicit lifetime bounds (epoch deadline) — but without expiry-sweep risk.
- **Mercury**: the absolute-locktime ladder as the trustless floor under everything.
- **Rejected**: revocation keys (grow the collusion surface), mandatory round refresh (liveness
  cliff), DW update counters (bounded updates for no benefit given SE refusal).

## UTXO granularity

Spark leaves are fixed denominations; exact amounts need SSP swap pools. Ark re-mints change each
round. Here, **exact amounts are a native off-chain operation**: one SE-co-signed split mints any
piece + change (1-sat resolution above a 330-sat dust floor), chainable to any depth, each piece a
full coin with its own ladder. Granularity is strictly better than both. Exact bounds, token
packaging and pricing: the granularity pack — [deep dive](granularity-deep-dive.md),
[GRANULARITY-SPEC](../GRANULARITY-SPEC.md), [economics](../research/granularity-economics.md).

## Unilateral exit economics (measured, `SDK_E2E=7`)

| Coin | Txs | vsize | Fee @2 sat/vB | Fee @30 sat/vB | Wait |
|---|---|---|---|---|---|
| Flat coin | 1 (backup) | ~112 vB | ~224 sats | ~3,360 sats | ≤ initlock (1000 blk ≈ 7 d), −10/handover |
| Depth-1 sub-coin | 2 (split + backup) | 267 vB | 534 sats | 8,010 sats | same (fresh ladder) |
| Depth-N sub-coin | N+1 | ~112 + N·155 vB | linear | linear | same |

`estimate_exit_cost(coin)` returns live numbers (tx count, vsize, `fee_sats_at(rate)`,
`wait_blocks`); `unilateral_exit` broadcasts the branch immediately and reports the remaining
wait instead of failing. The branch carries a pre-committed fee reserve; the backup can be
CPFP-bumped from the user's own output when feerates spike.
