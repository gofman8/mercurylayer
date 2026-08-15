# Partial-payment economics — what a payment costs

The cost model for the only payment shape that occurs: a partial payment. Every figure here is
derived from constants read at `feat/spark` and is cited by SYMBOL and FILE.

> **Build status — read this before quoting anything below.**
>
> **BUILT and live:** the zero-CSV spine tier (`SPINE_CSV = 0`, signed by all three split builders —
> `in_ladder_split`, `cosign_colored_in_ladder_split`, `child_in_ladder_split`); the spine as a
> distinct verifier KIND with bounds `[0,0]`; the plain-root and coloured-root change leg as a
> one-rung spine tip (`SpineTipBundle`); the spine batch (`spine_batch_split`) and the coloured spine
> batch (`colored_spine_batch_pay`); the depth and exit-chain-length caps; the split journal; the
> watchtower event trigger; child renewal (`renew_child`).
>
> **DESIGN, not built:** the **discharge round** (§4, SPEC.md §5.4) — its enforcement point is
> empty. And the **sweep / SSP absorption** (§3): `combine_leaves` has **zero callers outside a
> test**, the absorption predicate exists as no function anywhere, and the `claim()`-time swap does
> not exist. §3's step S1 — a cooperative `spine + 1` child exit end to end — is **UNVERIFIED**;
> every number in §3 rests on it.
>
> **Modelled, not measured:** §1's settlement curve is a model. No realised hops-per-leaf or
> settlement cost from a live fleet has been published.
>
> **Still open:** per-output blinding on the coloured lane (§8), whole-coin handover of a spine tip
> (refused by name today), opt-in self-carve inventory, the lean leaf (§7.7).

---

## 1. THE CENTRAL RESULT — this design sells payment VELOCITY, not payment granularity

Everything else in this document is a detail of this one claim, and the claim has a side on which we
LOSE. State both sides or the number is marketing.

### 1.1 The honest alternative

A one-to-many payout on Bitcoin is **ONE transaction with N+1 outputs**, not N transactions. Paying
100 recipients on chain costs **4 411 vB — about 44 vB per recipient.** Any comparison that prices
the on-chain alternative at "N × 155 vB" is inventing an opponent that does not exist, and every
favourable ratio derived that way is worthless.

The ordinary on-chain baselines are **112 vB** (1-in-1-out) and **155 vB** (1-in-2-out) at
`INPUT_WITNESS_BYTES = 67`; ~**154 vB** is the ordinary-payment figure used for per-payment
comparisons below.

### 1.2 WHO THE USER IS — the leaf lane is the only lane

**Payments are arbitrary amounts.** An arbitrary amount equals a coin the sender already holds only
by coincidence, so essentially every payment is an **in-ladder split** and the payee receives a
**LEAF**. Whole-coin-holder and whole-coin-transfer economics describe nobody:

* The whole-coin handover path exists — `child_retransfer` (`clients/libs/rust/src/tesr.rs`) builds a
  replacement state over the *same* `ext_child.out[0]`, spends zero sats, adds zero depth and never
  touches `ancestors` (the only writer of `ancestors` is `child_in_ladder_split`). It is free.
* It is also structurally unreachable as a payment path. `min_child_value` is `2·rung + dust`; the
  finest piece the protocol will mint is **1 560 sat**, so no coin set can be made fine enough for a
  subset sum to land on an arbitrary amount's residue. Simulated against a realistic mix
  (V = 1 M, 3 000 payments log-uniform 3k–150k, binary grid, the real `exact_subset` DP in
  `clients/libs/rust-sdk/src/select.rs`) the exact-subset hit rate with no leaf return was
  **5 in 3 000**.

**Do not quote root-holder economics.** A root holder is the depositor. Theirs is the most flattering
number in this document — ~**589 vB/yr**, i.e. **5.9 vB** per payment at 100 payments/yr, **26×**
better than ~154 vB on chain — and it describes almost nobody once payments start flowing. After the
first payment, everyone downstream is on the leaf lane.

### 1.3 The per-payment block-space ledger

| leaf lane, per payment | block space | against ~154 vB on chain |
|---|---:|---|
| **spent onward off-chain** | **0 vB** | this is the product |
| **swept and settled** (§3) | **~105 vB** | **1.47× better — and this is the CAP without the round** |
| **walked out unilaterally** | **250 – 2 719 vB** | **WORSE than on-chain** |
| **shipped default** | **418 vB** | 2.7× worse |

The walked range is the leaf's own exit chain, `293·d + 375` vB over `3 + 2d` sequential
transactions (`exit_cost_scaling_model`, `clients/libs/rust-sdk/src/invalidation_model.rs`); the top
of the range is the mainnet depth cap of 8 (`293·8 + 375 = 2 719` vB over 19 transactions). Walking a
depth-1 leaf out is **250 vB — 1.62× WORSE** than doing the payment on chain.

The **~105 vB** swept row amortises the shared prefix across a whole tree (§1.4). The sweep MARGINAL
alone — one further leaf into an existing batch — is **58 vB**, i.e. **0.38×** the on-chain 154; that
is the quantity §2 prices, and it is a floor the tree-wide figure never reaches.

**For the population that actually exists, the shipped default settles a payment for MORE block
space than doing it on chain.** That is the sentence to lead with. The sweep (§3) is what changes
it — not an optimisation on a winning position, but the precondition for the median user's
block-space economics being positive at all. The discharge round (§4) is what would change it by an
order of magnitude, and it is not built.

**Why the distribution is not what we save** — a batched on-chain payout is nearly free too
(~44 vB/recipient). What we sell is every payment AFTER the first: on chain another ~154 vB each,
off chain zero. The saving is a function of how many times value MOVES before it settles, and the
design rule follows: **a piece received and immediately cashed out should never have been an
off-chain split.**

### 1.4 Settlement across a whole tree — the model

The sweep is not free. For an SSP to hold 90 % of a tree, 90 payees must each have TRANSFERRED their
leaf to it, and a transfer IS an onward hop. Sweep fraction `s` and hop count `h` are COUPLED:
`h ≥ s`.

One coin to 100 recipients, all settling. Utexo = prefix + tip + `(1−s)·100` walks + one
`sweep_tx_vsize(100s, 1)`; on-chain = a batched payout + `100s` onward payments:

| sweep fraction `s` | UTEXO | ALL ON-CHAIN | winner |
|---:|---:|---:|---|
| **0.00** — nobody hops, nobody sells | 29 800 vB | **4 411 vB** | **ON-CHAIN, 6.8×** |
| 0.50 | 20 241 | 12 111 | ON-CHAIN |
| 0.70 | 16 396 | 15 191 | ON-CHAIN |
| **0.74** | 15 627 | 15 807 | **crossover** |
| 0.90 | 12 551 | 18 271 | utexo 1.5× |
| 1.00 | 10 628 | 19 811 | utexo **1.87×** |

The crossover is at **~74 % sweep coverage**, and the ceiling is **1.87×**. Below ~74 % coverage a
batched on-chain payout is simply better, and at zero coverage it is better by 6.8×. The 1.87×
counts the on-chain column's batched payout in the denominator; per settled payment the same result
is ~105 vB against 154 — the **1.47×** of §1.3.

Without a sweep, settlement is 29 800 vB and NOTHING makes the lane win at any hop count, because
the walks dominate.

The shared prefix is `T + X_m + SP` = **`375 + 43K` vB**, not a flat 375 (`tesr_exit_vbytes`'s
`3 × TIER` counts the leaf's own final state, which is private). The sweep marginal is unreachable
for a payment tree whose leaves have different owners, which is exactly why acquisition — not
batching — is the mechanism that matters. Re-derive these from `lib/src/tesr.rs`,
`lib/src/transaction.rs::sweep_tx_vsize` and `clients/libs/rust-sdk/src/config.rs`, not from prose.

---

## 2. The satoshi ledger, which is a different quantity and a bigger number

Block space and VALUE are not the same saving, and the value one is larger.

Every pre-signed tier permanently burns `committed_fee(3.0) + P2A_VALUE` = **615 sat**, carved out of
the coin at split time. A leaf's own two tiers burn **1 230 sat** — and `min_child_value` = **1 560**
is exactly that plus dust, so a minimum-sized leaf walked out realises precisely the 330-sat dust
limit. A combine spends `SP.out[j]` directly and never broadcasts those tiers, so the 1 230 sat is
never burned:

| leaf face | walked out | via combine @ 3 sat/vB |
|---:|---:|---:|
| 1 560 | 330 — **21 %** | 1 387 — **89 %** |
| 5 000 | 3 770 — 75 % | 4 827 — 97 % |
| 20 000 | 18 770 — 94 % | 19 827 — 99 % |

**An absorber's margin is `1 230 − 57.75 × market` sat per leaf** — about 1 057 sat at 3 sat/vB,
653 at 10, and **zero at 21.3 sat/vB**, above which the prepaid committed rate is the better deal and
holders should simply walk. It is an INVERSE-fee-market business: it earns most when fees are low,
and should stop buying when they are high.

Note what this makes irrelevant: batching moves the per-leaf marginal 112 → 58 vB, worth ~160 sat at
3 sat/vB against a ~1 057-sat margin. **Skipping the burn is ~96 % of the value; consolidation is a
rounding error.** An absorber profits on a SINGLE leaf and needs neither whole trees nor majority
ownership — `SP`'s outputs are independent UTXOs, so a sweep of 9 of 10 leaves captures 99 % of the
available saving and the holdout is simply untouched.

### 2.1 The per-split fee ledger

A split carves a piece child and a change child from a state tier `SP` over `X_m.out[0]`, each funded
by `establish_child` (`clients/libs/rust/src/tesr.rs`). Measured as loss of total exitable value
across the tree, for the two-tier change shape:

| component | plain | coloured | source |
|---|---:|---:|---|
| `SP` / `CSP` split tier (2 payloads) | 576 | 662 | `lib/src/tesr.rs`; `clients/libs/rust/src/rgb.rs` |
| piece child — extension + state rung | 980 | 1 152 | `establish_child`, `clients/libs/rust/src/tesr.rs` |
| change child — extension + state rung | 980 | 1 152 | same |
| less the superseded state rung `SP` replaces | −490 | −576 | `clients/libs/rust/src/tesr.rs` |
| **system total, per partial payment** | **2 046** | **2 390** | |

The shipped plain-root and coloured-root lanes replace the change child with a one-rung spine tip,
which removes one rung (490 plain / 576 coloured) and one whole level of latency — see §7 and the
cost table in §11.

Derivation of the units:

```
committed_fee(r)                = ceil(125·r)                     lib/src/tesr.rs   -> 250 @ r=2
committed_fee_for_outputs(n,r)  = ceil((125 + 43(n−1))·r)         lib/src/tesr.rs
colored_committed_fee(n,r)      = ceil((168 + 43(n−1))·r)         clients/libs/rust/src/rgb.rs
P2A_VALUE                       = 240                             lib/src/tesr.rs
rung  = committed_fee + P2A     = 490 plain / 576 coloured        (r = 2.0)
                                = 615 plain                       (r = 3.0)
min_child_value(2.0, 330)       = 2·490 + 330 = 1 310             lib/src/tesr.rs
min_child_value(3.0, 330)       = 2·615 + 330 = 1 560             — the finest mintable piece
colored_child_floor(2.0, 330)   = 2·576 + 330 = 1 482             clients/libs/rust/src/tesr.rs
min_spine_tip_value             = rung + dust = 820 / 906 coloured
mainnet params = { d0 1440, δ 36, d_floor 144, e0 720, δE 36, e_floor 144, m_max 15, rate 2.0 }
                                                                  lib/src/tesr.rs
```

The toll is **flat and amount-independent**: 2 046 sat is 20.5 % of a 10 000-sat payment, 2.0 % of
100 000. Who pays: the sender loses 1 066 (its own change leg plus the split tier), the payee loses
980 off the nominal — a 10 000-sat piece is worth 9 020 on unilateral exit.

**The exit fee is not prepaid.** `committed_fee_rate` is a hardcoded 2.0 (`lib/src/tesr.rs`), not the
live rate. Above ~5 sat/vB every tier must be CPFP'd through its P2A, and TRUC's
one-unconfirmed-child rule plus the sequential CSVs forbid batching those children. The realised
top-up from an external funded wallet is the largest number in this document — see §11.

**The quote is the executor's own plan, on the laddered lane.** `quote_transfer` runs the executor's
planner and preflight, so `fee_sats` and the per-leg `SplitFloors` come from the same
`split_preflight` the executor obeys and `fundable` is what the executor will do rather than an
estimate. The `split_fee_reserve` clamp — `clamp(parent/100, 300, 2000)` — survives **only** on the
un-laddered plain-split lane, where the quote is still an estimate and not a plan.

### 2.2 The splittability tail

Splitting requires the change to fund two floored children: `c ≥ piece + 2 376`, absolute minimum
`c ≥ 2·1 310 + 1 066 = 3 686` plain (4 202 coloured). In the dead zone `1 310 ≤ c < 3 686` the change
is still exitable but can never make another partial payment. Worked at V = 1 000 000 and
10 000/payment, two-tier change: `c_N = 999 510 − 11 066N`, so **payment 91 is refused** — 900 000
nominal delivered, 811 800 exitable, **185 610 sat (18.56 % of the deposit) burned**, and the
survivor is a **3 570-sat** depth-90 coin worth **2 590** on exit.

Spine-tip floors are lower — exitable at **820**, splittable at **2 706** (coloured 3 050) — so the
reach extends to ~94 payments at K = 1 and further under batching, with 147 734 sat (14.8 %) of
reserve.

---

## 3. THE SWEEP — mechanism, and it is DESIGN

> **Not built.** `combine_leaves` has **zero callers outside a test**. The absorption predicate is
> not a function anywhere, the `claim()`-time swap does not exist, and no settlement scheduler
> exists. The parameters below are proposed defaults, not live configuration. Normative form in
> SPEC.md §5.3.

### 3.1 The one structural fact everything follows from

**The surplus is INDEPENDENT of the leaf's value.**

```
surplus(m) = BURN − combine_marginal(m) = 1 230 − 57.75·m   sat per leaf
```

`BURN` is what a leaf's own two pre-signed tiers destroy (2 × 615). It does not scale with face.
Neither does the combine input. So an absorber earns **the same ~1 057 sat at 3 sat/vB** whether the
leaf holds 1 560 sat or 1 BTC.

Three consequences, and they are not intuitive:

* **Small leaves are the BEST business, not the worst.** Same absolute surplus, far less capital at
  risk. At the admission floor the surplus is 68 % of the leaf's entire value; at 100 000 sat it is
  1 %.
* **There is a natural VALUE CEILING.** Above some face the absorber is taking balance-sheet risk for
  a return that has stopped growing. The ceiling is a risk-appetite parameter, not an economic one.
* **Batching is nearly irrelevant.** Going 1 → 10 leaves moves the marginal 112 → 63 vB, worth ~150
  sat against a ~1 057-sat surplus. **Absorption is the business; consolidation is a 4 %
  optimisation.** No whole trees, no majority ownership, no coordination with holdouts.

### 3.2 WHEN to absorb — at claim, inside the payment flow

The swap belongs in `claim()`, at the moment a payee first sees the leaf: runway is maximal (the
inherited deadline is furthest away), the user is online because they are already transacting, and no
separate coordination round is needed. The payee receives an ordinary root coin and never handles a
leaf.

**A root is strictly better for the payee than the leaf it replaces**, independent of any spread: no
inherited deadline, depth 0, a one-transaction cooperative exit, and no watchtower duty tied to a
parent it does not control. That is what makes a silent swap defensible rather than extractive —
subject to the fairness condition in §3.5.

### 3.3 The absorption predicate

Absorb a leaf iff ALL hold:

| condition | default | derivation |
|---|---|---|
| `market_fee_rate ≤ sweep_max_fee_rate` | **15 sat/vB** | surplus hits zero at 21.3; 15 keeps a ~30 % margin (369 sat/leaf) |
| `runway_blocks ≥ sweep_min_runway` | **903 blocks** | `e_csv(720) + confirmations(3)` = 723, +25 % safety. Below this the leaf CANNOT be settled and absorbing it buys a liability |
| `leaf_value ≤ sweep_max_leaf_value` | **100 000 sat** | the value at which a constant ~1 057-sat surplus falls below 1 % of face |
| `tree_exposure + leaf_value ≤ sweep_max_tree_exposure` | **1 000 000 sat** | `target_batch × max_leaf_value`; bounds loss if one tree's spine cannot be materialised |

### 3.4 WHEN to settle — the absorber holds an option and should price it as one

Having absorbed, the absorber is not obliged to settle promptly. It holds a **timing option**: settle
at the cheapest fee window inside the runway. Exercise when EITHER:

* `batch_size ≥ sweep_target_batch` **and** `market ≤ sweep_max_fee_rate` — the voluntary path; or
* `earliest_deadline − tip ≤ sweep_min_runway` — the **forced** path, and it is unconditional. A leaf
  that misses its inherited deadline is voided by the parent's flat backup and the loss is the whole
  face, not the spread.

`sweep_target_batch = 10` captures 94 % of the achievable batching gain; beyond it the curve is flat
and waiting only adds fee-market and deadline exposure.

**The risk is asymmetric and must be stated that way.** Settling too EARLY costs a few hundred sat of
foregone batching. Settling too LATE costs the whole leaf. Every default above is biased toward
acting early, and the forced path ignores the fee ceiling entirely — an expensive settlement beats a
voided one at every rate.

### 3.5 The fairness condition

A silent swap must leave the payee **no worse off than holding the leaf**, measured against the
leaf's own realisable value:

```
price_paid ≥ leaf_value − BURN          (what the payee would realise walking it out)
```

At the floor that means paying at least 330 sat for a 1 560-sat leaf — while the absorber realises
1 387. There is ~1 057 sat of surplus to divide, and the split is `sweep_spread_bps`, a policy
parameter and not a protocol constant. Two obligations follow: the payee is handed a coin that is
**strictly better in kind** (root, no inherited deadline), and the spread is disclosed in aggregate
rather than being the mechanism's hidden purpose.

**Do not let the spread exceed the surplus.** A swap priced below `leaf_value − BURN` takes value
from a payee who would have done better walking, which is the one outcome that turns this from a
service into a tax.

### 3.6 Build order, cheapest evidence first

| # | step | why it is first | evidence |
|---|---|---|---|
| **S1** | prove `spine + 1` cooperative child exit end to end — **UNVERIFIED** | every number in §3 rests on it; if a confirmed `SP.out[j]` cannot be cooperatively spent, the whole design collapses to the walk | an E2E: split, materialise spine, mine to `confirmation_target`, cooperative withdraw, assert ONE transaction |
| **S2** | wire `combine_leaves` to a caller — it has **zero** outside a test | the primitive exists and is unreachable; nothing else can be measured until it is | an E2E consolidating k ≥ 2 leaves of one `SP` |
| **S3** | the absorption predicate as a PURE function + its parameters | testable without a stack, and it is where a wrong sign silently becomes a policy | unit tests per row of §3.3, both directions |
| **S4** | the swap in `claim()`, behind a default-OFF flag | the payment-flow half; default-off so it ships before it is trusted | an E2E: payee claims, receives a ROOT, absorber holds the leaf |
| **S5** | the settlement scheduler (voluntary + forced paths) | needs S1–S4; the forced path is the one that must never be skipped | a test that the forced path fires **regardless** of the fee ceiling |
| **S6** | publish the realised curve from a live fleet | §1.4's break-even is modelled, not measured | measured hops-per-leaf and settlement cost against the model |

**S1 is the gate.** It decides whether this is a 4 %-margin batching play or a ~1 057-sat-per-leaf
value-recovery business.

---

## 4. THE DISCHARGE ROUND — the footprint scales with PIECES, not with PAYMENTS

> **Costs a design that does not exist** (SPEC.md §5.4). The enforcement point is empty: `disclosure`
> / `prevout_value` occur 83× in the client and **0× in `lockbox/`**. These are the numbers it
> *would* cost built, not numbers anything measures.

### 4.1 The structural result

A round re-mints **every outstanding leaf** regardless of how many payments produced it, and retires
the old tree in **one transaction**. So the on-chain footprint is set by

```text
    footprint  =  (outstanding pieces ÷ 256) × (365 ÷ epoch_days) × (155 + 43·absentees)
                   └── tree count ──┘          └── rounds/year ──┘   └── one collapse tx ──┘
```

and **payment volume appears nowhere in it.** 256 is a hard cap, not an assumption: a depth-8 tree
has `2⁸` leaf slots. Migration consumes the successor tree's slots, so tree count tracks *pieces
held*, never *payments made*. Without the round, cost scales with **payments** (and loses); with it,
cost scales with **held pieces** (and wins).

### 4.2 Worked: 1 M users, 4 000 BTC TVL, 1 M payments/month

12 M payments/yr. Bitcoin supplies 52.56 GvB/yr. On-chain baseline: 1.85 GvB/yr = **3.52 % of the
entire chain**.

| pieces/user | trees | collapses/yr | 0 % absent | 10 % | 50 % | 100 % |
|---:|---:|---:|---:|---:|---:|---:|
| 1 | 3 907 | 20 535 | 0.27 vB | 2.15 | 9.68 | 19.10 |
| **3** | 11 719 | 61 595 | 0.80 | **6.45** | 29.05 | 57.30 |
| 10 | 39 063 | 205 315 | 2.65 | 21.49 | 96.82 | 190.99 |

*(vB per payment.)* Central case — 3 pieces/user, 10 % absent — is **77.4 MvB/yr = 0.147 % of Bitcoin
block space**, about **77 blocks a year**, 24× better than an on-chain payment and 65× better than
the shipped 418 vB. The worst cell (10 pieces, nobody ever online) is 4.36 % of the chain and
**still beats the shipped default**.

### 4.3 Payment volume rides free

The same 77.4 MvB carries any of these:

| traffic | vB/payment | footprint |
|---|---:|---|
| 1 M/month | 6.45 | 0.147 % of chain |
| 10 M/month | 0.65 | **unchanged** |
| 1 B/year | 0.077 | **unchanged** |

### 4.4 The two levers

1. **Absentee rate — dominant.** 0.80 → 57.30 vB/payment is a **72× swing**, and it is a product
   problem (how often wallets check in), not a protocol one.
2. **Epoch length — linear.** `initlock = 10 000` sets 5.26 rounds/yr; raising it cuts the total
   proportionally. Depth is a *usability* dial rather than a safety limit, so there is real room —
   gated on reconciling depth admission against materialisability (SPEC.md §5.4.5 REQ-63.4).

**Quote the worst case, not the best** (SPEC.md §5.4.6): exit-key reassignment lets any holder force
a payout instead of a migration, free and unattributable.

---

## 5. Admission — depth, exit length and headroom

Three caps bound what may be minted and what may be received, all enforced today.

* **Exit headroom, receive-side.** `check_exit_headroom` (`lib/src/transfer/receiver.rs`), called from
  the conveyed-child verifier in `clients/libs/rust/src/tesr.rs`, admits a conveyed child only if its
  own exit can finish inside the epoch with `exit_slack_margin` to spare. Every input is
  receiver-derived: CSVs come off the signed `nSequence`, the epoch off the validated flat chain.
  E2E: `clients/tests/rust/src/sdk82_exit_headroom_gate.rs`. Without this gate the only bound on a
  conveyed child is `lock_time > tip`, and a sender can hand a payee a coin that provably cannot be
  materialised before the sender's own flat backup spends `F` and voids the whole tree.
* **Depth.** `max_split_depth`, enforced build-side by `enforce_split_depth_cap_shaped`
  (`clients/libs/rust/src/tesr.rs`) and derived from the live schedule and the coordinator's live
  `initlock`. Because admission adds `exit_slack_margin` on top of the bare latency rule, the caps
  are **depth 8 on mainnet** and **depth 54 on regtest** — a deeper child is minted by nobody,
  because no receiver would adopt it.
* **Exit-chain length.** `max_exit_txs = 3 + 2·max_split_depth` — **19 transactions on mainnet**,
  **111 on regtest**. The latency rule alone cannot see this: a spine tier costs one block of latency
  and a whole transaction, so an all-spine chain of thousands of tiers passes the headroom check and
  is still unusable. `enforce_split_depth_cap_shaped` evaluates the length cap **above** the latency
  rule's early return and charges each level by its real shape (`SplitLevelShape`).

A sender's spine tip walks `s + 3` transactions and a payee's piece `i + 4`, so the mainnet
19-transaction cap admits `s ≤ 16` spine levels for the sender and `i ≤ 15` for a payee's piece. The
cap is on the CHAIN, not on one tier: an `SP`'s width is a free parameter, and **a v3/TRUC tier above
10 000 vB never relays — a separate, open finding.**

### 5.1 Exit latency

BIP-68 relative timelocks are sequential, so exit latency compounds. A tier's relative lock only
starts counting once its parent confirms, so the real figure is `Σ csv + one confirmation each` —
`exit_wait_blocks` (`lib/src/transfer/receiver.rs`) is the single implementation of this convention
and both the delegated tower and the owner's own tower call it, so they cannot drift apart.

With `SP` signed at `SPINE_CSV = 0`, a two-tier level costs `720 + 0 + 2 = 722` blocks and a depth-1
leaf's whole walk is **2 885** blocks — **2 880** counting relative timelocks alone. The two
conventions differ by exactly `3 + 2d`; quote both ends of any comparison under the same one.

Latency is **contagious**: the piece child inherits the identical ancestor chain, so the recipient of
a payment inherits the sender's payment history as exit latency. The depth cap of §5 is what bounds
it.

---

## 6. What cannot be delivered, and why

Change that stays at **root level** (a sibling of `T` over `F`) is unreachable. `build_trigger` is the
only builder touching `f_txid/f_vout` (`lib/src/tesr.rs`), `T` carries
`TRIGGER_SEQUENCE = 0xFFFF_FFFD` — relative lock **disabled** — and every prior owner of a
Model-A-conveyed coin retains a signed copy. Any change output that is a sibling of `T` over `F`
loses unconditionally to a retained `T`: no timelock schedule out-races a transaction that has no
timelock.

**The Freeze Lemma.** Payee *i* holds a coin funded by an output of a pre-signed tx `P_i`. For the
conveyance to be theft-proof, the sender must be unable to confirm anything else over `P_i`'s input
outpoint — so that outpoint is dead to the sender the moment the bundle is conveyed, and the
sender's change must move to an output of `P_i`. **Every payment therefore adds at least one
transaction to the sender's exit chain**, in any design that funds payees from pre-signed
un-broadcast transactions and adds no fresh on-chain data. The spine pays exactly **one** and
therefore attains the bound; nothing in this architecture beats it. Constant depth is achievable only
by adding on-chain data per payment or by not funding payees from the sender's tree at all — i.e.
denominations, which fail for other reasons (§12.1).

---

## 7. The construction — spine plus batch

### 7.1 Shape

Root, unchanged. `claim()` → `establish_auto` (`clients/libs/rust/src/tesr.rs`) gives
`F → T → X_m → S_0`. Pure-handover coins are untouched.

Payment batch *i+1* replaces the live cap over the current spine outpoint `O_i` with a **spine tier**
`SP_{i+1}` carrying K+1 payload outputs:

```
O_i  ( = X_m.out[0] at i=0, else SP_i.out[spine] )
 │
 └─ SP_{i+1}      nSequence = 0        (K+1 payload outs + one P2A anchor)
      ├─ out[0..K−1]  → piece children: establish_child, ext CSV 720 + state CSV 1440, payee's key
      └─ out[K]       → the new spine tip
            └─ C_{i+1}   ONE state tier, CSV Δ_cap = 1440, sender's own exit key
```

`C_i` is disclosed as superseded. The sender's coin is always "a slot with one cap over its funding
outpoint" — payment 1 and payment 1000 are the same object and the same builder.

Three properties of substance:

1. **`SP`'s nSequence is 0.** `SPINE_CSV = 0` (`clients/libs/rust/src/tesr.rs`) is what all three
   split builders sign.
2. **The change gets no extension.** The extension exists to reset the state budget by renewal; on
   the spine every payment already lands the change on a virgin outpoint at a virgin `D0`, so the
   rung is dead weight. That missing rung is the 490 sat and the 720 blocks the spine saves per
   level.
3. **K+1 payloads, not 2.** `build_split_state` (`lib/src/tesr.rs`) and `committed_fee_for_outputs`
   are N-ary; `in_ladder_pay_many` (`clients/libs/rust-sdk/src/transfer.rs`) drives them. Depth
   advances **per batch**, not per payment.

### 7.2 Why nSequence 0 is correct and not a corner-cut

Over spine outpoint `O_i` exactly two transactions can ever exist: the sender's retained cap `C_i`
(CSV 1440) and, later, `SP_{i+1}` (CSV 0). `SP_{i+1}` is the transaction the payees need; `C_i` is
the transaction that would **steal** from them (it sweeps all of `O_i` to the sender's key). So the
honest transaction must win, and 0-vs-1440 is the largest possible margin.

The un-timelocked tier is signed only by the sole current owner of the outpoint it spends, on the
outpoint it is simultaneously giving up — the `T`-vs-`F` asymmetry does not arise, because the
voiding party and the victim are the same entity.

The payee's watchtower window — time to push `SP_{i+1}` after `SP_i` confirms — is **1 440 blocks
(10 days)**. `Δ_cap` is a free parameter that costs nothing per payment (it appears once, on the
sender's own final leg); 1440 is the safe default.

### 7.3 The tip, and the batch that keeps it usable

`in_ladder_split` takes a `ChangeLeg` and, on the plain ROOT lane, sends the change leg to
`establish_spine_tip_journalled` — ONE state tier at `p.state_csv(0)` directly over `SP.out[K]`, no
extension — returning it as a `SpineTipBundle` for `persist_spine_tip` rather than as a `ctesr-`
child. `change_leg_role()` is per-LANE and reports `SpineTip` for `SplitLane::PlainRoot` and for
`SplitLane::Colored`, so the 820-sat change floor is live on both and the Freeze-Lemma bound is
attained: a payment adds one transaction to the sender's exit chain, not two.

A tip is not a coin any other builder can take: it has no `tesr-` row, so `in_ladder_pay` cannot load
it, and no `ctesr-` row, so `child_in_ladder_pay` cannot either; and it cannot be handed over whole,
because a flat conveyance would give the recipient a backup chain over an un-broadcast funding output
— a coin with no exit. **The SPINE BATCH is what makes a tip spendable.** `spine_batch_split` builds
`SP_{i+1}` over the tip's own funding outpoint `SP_i.out[K]` at `SPINE_CSV` (via
`build_split_state_from`, never the vout-0 builder), retires the cap `C_i` into the segment's
`superseded_states`, terminalizes **the TIP's** slot (not the root parent's, which went terminal at
batch 1), and leaves another one-cap tip. `ParentShape::SpineTip` routes to it in both `transfer` and
`transfer_many`, and `split_preflight_pure` admits a tip on exactly the terms it admits the coin it
came from.

Two consequences that are easy to get backwards:

* The batch's `SP_{i+1}` is at `SPINE_CSV` while the new cap is at `state_csv(0)` — **two different
  tiers, two different bounds**. Pin the cap to `SPINE_CSV` and it ties with every future `SP`; the
  builder's `cap_csv <= SPINE_CSV` guard then refuses the next batch, stranding the tip when it is
  already terminal.
* A spine level costs the exit walk **ONE tier**, so `enforce_split_depth_cap` charges levels by
  shape (`SplitLevelShape`). Charging a spine level as two is a silent economic cap; charging a
  two-tier level as one mints a leaf whose exit does not fit the epoch.

`SpineTipBundle::validate()` is a PRECONDITION of `persist_spine_tip` (the producer's only door): the
cap must spend `(SP.txid, sp_vout)` **derived from its own signed prevout**, must pay the recorded
exit address at its declared payload index, `sp_out_value` must equal that output's real value, and
the cap's SIGNED `nSequence` must sit in `[d_floor, d0]` — not `[0,0]`, which would leave the next
batch's `SP` nothing to out-race and strand the tip behind the builders' own `s0_csv <= SPINE_CSV`
guard. Structural checks run strictly before value checks.

### 7.4 The coloured lane

The coloured lane is repeatable. `colored_spine_batch_pay` (`clients/libs/rust-sdk/src/tokens.rs`)
drives `build_colored_spine_batch` + `cosign_colored_spine_batch` and runs the root lane's
consignment pre-flight over every leg before the tip is terminalized; the coloured send router
dispatches a coloured spine tip to it, because a coloured spine tip is the carrier's shape from its
second payment onward. `colored_child_txids` and `colored_child_seals` walk `ancestors` N deep,
charging a spine segment one tier and a two-tier segment two. `cosign_colored_in_ladder_split` gives
the coloured root split's change leg a one-rung coloured tip floored at `colored_spine_tip_floor`.
`spine_batch_split_ex` forks by lane, and a coloured tip has a coloured `SP` to be built with.

The coloured legs are the SAME loop the coloured root split uses (`build_colored_split_legs`, shared
deliberately so the two shapes cannot drift): per-payee `ext_child`/`state_child` with consignments
and seals rooted at `SP.out[j]`, and one coloured cap for the next tip.

Two named lane refusals, both current:

* The **both-coloured arm of the PLAIN driver** `spine_batch_split_colored` is refused permanently.
  That entry point builds every leg with `establish_child_journalled` /
  `establish_spine_tip_journalled` and persists `rgb: None` — an uncoloured tier over the outpoint the
  allocation is booked at, which would burn the allocation rather than refuse. It has no callers; the
  refusal is a lane guard against a latent hole, pinned by
  `ci-guards/tests/deny_uncoloured_legs_under_a_coloured_sp.rs`.
* Carrier sizing is two named lanes: `LEGACY_CARRIER_SEND_DEPTH = 5` and
  `CTESR_CARRIER_SEND_DEPTH = 1` (`clients/libs/rust-sdk/src/tokens.rs`), with
  `TOKEN_CARRIER_SATS = 17 384` sized for the LEGACY lane (the max of the two; the CTES-R lane needs
  6 362) — a stated over-provision.

### 7.5 What the blind SE signs

Per batch of K pieces:

| co-sign | under | count |
|---|---|---:|
| `SP_{i+1}` (the spine tier) | `A_spine_i` | 1 |
| `C_{i+1}` (the new cap) | `A_spine_{i+1}` | 1 |
| each piece's extension + state | that piece's own aggregate | 2K |

**Total 2K + 2 co-signs, i.e. 2 + 2/K per payment.** Plus one `set_spend_budget(…, 1)` on the
outgoing spine slot, which is K-invariant.

The SE receives a sighash and a prevout amount. `cosign_tier` is issued **once** for `SP` regardless
of K, outside the child loop. nSequence lives inside the transaction and is invisible to it. The SE
never learns K, the denominations, the colour, or that a spine exists rather than a 2-way split.
**Zero server diff, zero enclave diff, no new endpoint, no new cryptography.**

### 7.6 Unilateral exit at every hop

Both chains are fully pre-signed, need no counterparty, and terminate at the holder's own exit key.
`child_exit_chain` (`clients/libs/rust/src/tesr.rs`) splices every ancestor segment root→leaf before
the leaf's own tiers; only the per-segment tier count changes.

With `s = ceil(N/K)` spine levels and a spine tier of `125 + 43K` vB:

```
SENDER (the spine tip):   [T, X_m, SP_1..SP_s, C_s]
  txs  = s + 3
  vB   = 375 + s·(125 + 43K)
  wait = 720 (X_m) + s·1 (one confirmation per zero-CSV tier; TRUC admits one unconfirmed
              ancestor, so the floor is one block per tier) + 1440 (C_s)
       = s + 2 160 blocks

PAYEE of a piece in batch i: [T, X_m, SP_1..SP_i, ext_child, state_child]
  txs  = i + 4
  vB   = 500 + i·(125 + 43K)
  wait = 720 + i + 720 + 1440 = i + 2 880 blocks   (≈ 20 days, flat in i)
```

Every piece in a batch exits at the same depth regardless of when it was paid. **This makes each
level cheap; it does not RESET depth.** The only depth reset remains the root re-anchor, which a
split tree does not have (§13).

### 7.7 The lean-leaf option — open, and a live capability to forfeit

Hang the piece's state tier **directly** off `SP.out[j]` and drop its extension. `build_state_from`
(`lib/src/tesr.rs`) already roots at an arbitrary outpoint. This cuts 490 plain / 576 coloured **and
720 blocks** per piece, taking the batched floor from 1 066 to 576 and the payee's wait to
`i + 2 160`.

The cost is real: `renew_child` / `renew_child_auto` rebuild the leaf's extension and state in place
over the same `SP.out[j]`, taking the leaf's transfer budget from 36 hops to 36 hops × 16 epochs per
depth level. Dropping the piece's extension forfeits that. The trade is **490 sat and 720 blocks per
piece against 15 further renewal epochs**, and it is a separate, argued decision.

---

## 8. Verifier and census rules

| # | rule | where | why it is load-bearing |
|---|---|---|---|
| V1 | `ChildSegment` is `{ extension: Option<TesrTier>, state: TesrTier }` | `clients/libs/rust/src/tesr.rs` | a spine segment has one tier |
| V2 | the ancestor expectation `CHILD_V2_BASELINE + 2 + seg_superseded_ok` derives the `2` from the **disclosed tier count** | `clients/libs/rust/src/tesr.rs` | without it every spine bundle is rejected outright; the literal `2` against a one-tier bundle is a free census slot, and that mismatch fails *open* — V1 and V2 are one change |
| V3 | a **SPINE tier kind** with CSV bounds `[0, 0]`, alongside state and extension | `clients/libs/rust/src/tesr.rs`, live and superseded paths | must be a new KIND, never a widened state range — see below |
| V4 | `SpineTipBundle` under `SPINE_TIP_KEY_PREFIX` | `clients/libs/rust/src/tesr.rs` | `withdraw` routes anything keyed `ctesr-` to unilateral exit; the tip must not be mistaken for a leaf |
| V5 | `split_output_floors` → `SplitFloors { piece, change }`, `min_spine_tip_value` = 820 plain / 906 coloured, per-leg refusal text | `clients/libs/rust-sdk/src/transfer.rs` | one number can only reach 820 by lowering the PIECE's floor too, which mints a child that cannot fund its second rung and dies after the parent is terminal |

### 8.1 What makes segment shape DERIVED, not declared

`extension: Option<TesrTier>` would otherwise make segment shape sender-declared, and the census does
not catch that on its own: a dropped tier is re-declared in `superseded_extensions`, where
`verify_superseded_segment` counts it, and the expectation moves by exactly the same 1 in the
opposite direction. `CHILD_V2_BASELINE + 1 + 1` and `CHILD_V2_BASELINE + 2 + 0` are the same number
for the same segment, so the census re-balances exactly and every co-sign is real. Three checks carry
the weight instead:

1. **The prevout re-anchor.** In the `None` branch the surviving tier must spend the segment's own
   **funding outpoint** — `st_in.previous_output == (fund_txid, seg.funding_vout)`. A genuine
   two-tier segment's state spends `ext.out[0]`, so it cannot be re-labelled. This is the single
   load-bearing check and it is *derived from a signature*: the outpoint is committed by the taproot
   `SIGHASH_ALL` sighash, so it cannot be repointed without invalidating the SE's own signature. The
   `Option` is a cross-checked declaration that must agree, never the source of truth. Without it, a
   real `[ext 720, state 1440]` segment declared as a spine loses 721 blocks from its declared exit
   chain, and `check_exit_headroom` admits a child near the epoch boundary whose real exit cannot
   finish.
2. **The `[0,0]` CSV pin** stays exactly disjoint from `[e_floor, e0]`. `[144,720]` is a strict
   *subset* of `[144,1440]`, so extension-vs-state is **not** CSV-separable; only the spine's `[0,0]`
   is disjoint from both, which is why widening it for the `None` case would destroy the last
   structural layer.
3. **The dead knob.** Child-side `superseded_extensions` has no honest writer: a non-empty list is
   refused whenever `extension.is_none()`.

**V1 is not applied to the conveyed leaf.** At the leaf the two CSV ranges overlap completely, so
nothing CSV-based separates a cap from an extension there — only the Model-A payee check does, which
is far more weight than that check is designed to carry. A conveyed piece is strictly two-tier; the
spine tip is never conveyed and has its own record (V4).

A superseded tier at CSV 0 is **always** rejected (`if sup.csv <= live_csv { reject }`), so the
CSV-0 admission fails closed and is not a theft primitive.

### 8.2 The enumeration hazard

V4's record was the easy half. Every site that ENUMERATES ladder artefacts had to be co-edited,
because a missed prefix does not produce an absence — it produces a confident wrong answer
(un-laddered, un-managed wallet, not a carrier, nothing to defend). The co-edited set is
`parent_shape`, `wallet_is_provably_pre_sdk`, `defend_ladders` (its own tower loop plus the L2
supersession evidence), `colored_child_sids`, `auto_exit_due`, `withdraw`, `unilateral_exit` and
`register_colored_exit_tip`.

`register_colored_exit_tip` is the shape this hazard takes: it resolved two record shapes in an
`if let … else if let … else { None }` chain, so a coloured tip took the trailing `else` and came
back `Ok(None)` — the answer a PLAIN coin gives, which its caller maps to no event, no fault and no
error. The tip's cap would land on chain while the RGB engine went on advertising the allocation at
the `SP.out[K]` that cap had just spent: not merely incomplete but STALE. All three shapes now route
through one `colored_exit_move` whose `match` is EXHAUSTIVE (a fourth shape is a compile error, not a
fourth silent `None`), plus a census asserting the CALLER still constructs all three variants — the
half an exhaustive match cannot see.

### 8.3 Census, K-invariant, exact equality

- *root slot* — `SP_1` is the terminal state; `S_0` and prior states are superseded, all with
  CSV ≥ 144 > 0, so the supersession check passes with the largest possible margin.
- *spine slot i* — baseline 0 (`CHILD_V2_BASELINE`: never funded on-chain, so `check_deposit` /
  `create_tx1` never runs). At rest 1 live (`C_i`) + 0 superseded = 1. After the next batch: 1 live
  (`SP_{i+1}`) + 1 superseded (`C_i`) = 2. A whole-coin handover of the tip adds exactly +1/+1, the
  arithmetic `child_retransfer` already relies on.
- *piece slot* — `flat_backups + 2 + superseded`.
- *replace-by-lower-timelock* — `X_m.out[0]`: 0 < 144…1440. `SP_i.out[spine]`: 0 < 1440. Leaf:
  untouched.
- *the census trap is respected* — `flat_backups` is never 0; `in_ladder_split` reads the parent's
  real chain and refuses `parent_backups.len() < PARENT_V2_BASELINE` **before** `set_spend_budget`.

### 8.4 RGB — per-output blinding is OPEN

`TierRole::Spine = 0x0C` (`clients/libs/rust/src/rgb.rs`; never renumber existing tags) is landed and
the N-deep witness list and seal schedule are landed. **Per-output blinding is not.**
`build_colored_tier` derives ONE `seal.blinding()` (`clients/libs/rust/src/rgb.rs`) and passes it once
for an `output_map` covering every payload; `colored_tier_seal` (`clients/libs/rust/src/tesr.rs`)
takes parent sid, role, `m` and CSV — nothing child-specific. A concealed seal commits to
`(method, txid, vout, blinding)`, so with B known and vouts enumerable, payee *j* de-conceals every
sibling seal in K tries. At K=1 this leaks the sender's change to the one payee already transacting
with them; at K=19 it makes nineteen mutually-unrelated payees and their exact allocations linkable.
Not theft — a seal is not spendable without the key — but **concealment across a batch is worth zero
bits**. The anti-collision property (rival tiers over one outpoint must not share a blinding, or
their `BundleId`s collapse into an arbitrary hash lottery) is preserved only because `SP` and `C`
differ in role and CSV.

Until per-output blinding lands, **coloured K > 1 is restricted to batches whose payees already know
each other** (payroll, one merchant's own settlements); coloured K = 1 for unrelated payees.

---

## 9. What K > 1 depends on

Each item is a live property, stated with the reason it must keep being true.

- **Crash-safe carve.** The unrecoverable window is `2K + 2` SE round-trips wide; at K = 20 that is an
  8.6× increase in independent failure points that would destroy the whole coin.
  `SplitJournalRecord` (`splitjrnl-`) is written complete **before** the parent's budget is touched,
  each tier's signature is journalled the instant it exists, and `resume_in_ladder_split` co-signs
  exactly the tiers still `None` — checking the journalled leg ROLE bidirectionally, so a `Piece` can
  never be resumed into a one-rung tip or the reverse.
- **Idempotent re-conveyance.** `in_ladder_pay_many` conveys the K pieces serially after the parent is
  terminal; each leg carries a `ConveyanceStage` advanced only forward and journalled **before** the
  network call it describes, so "the call never happened" and "the call happened and the answer was
  lost" are distinguishable (by `conveyance_x1`) instead of being one indistinguishable loss of
  bundles *j..K−1*.
- **Coin selection must not eat its own inventory.** `Candidate` carries `is_inventory` and
  `plan_with_floor` sorts on `(!is_inventory, amount_sats)` (`clients/libs/rust-sdk/src/select.rs`),
  so every inventory candidate outranks every non-inventory one regardless of size — a forecast miss
  splits the spine tip, not the smallest piece.
- **Derived-slot budget.** `max_derived_tokens_per_statechain = 64` (`server/src/server_config.rs`),
  counted over lifetime issuance **including spent rows** (`count_derived_tokens` is
  `SELECT COUNT(*) … WHERE derived_from = $1`). K ≤ 63 per spine level —
  `DERIVED_SLOTS_PER_STATECHAIN = 64`, `MAX_BATCH_RECIPIENTS = 63`, refused up-front by
  `refuse_oversized_slot_batch` — and because each level is a **fresh** statechain, the cap is
  per-level, not global. `take_derived_tokens` spends leftover vouchers from an earlier attempt first
  and persists the pool *before* handing any out, so a failed attempt costs the parent's lifetime
  allowance nothing.
- **The watchtower must express the trigger.** `WatchTrigger { watch_txid, watch_vout, csv_blocks,
  push_txs }` expresses the event; `watch_pass` evaluates it against the outpoint (`outpoint_spent`)
  alongside the height predicate and acts when **either** fires; `WatchState::Blind` means an entry
  the pass could not *evaluate* never averages into a green `Idle`.

  A leaf arms **both** predicates, because it genuinely has both: `deadline_block = L_k − head_start`
  (its clock is the *parent's* lowest flat-backup rung — a rung belonging to the splitter) and the
  event (an ancestor spending `F`). `head_start` comes from the **bound** chain, so a depth-N leaf is
  charged all N spliced spine levels, via the same `exit_wait_blocks` call `auto_exit_due` uses. Every
  unbuildable entry aborts the export by name rather than being dropped from it.

  The margin itself is DERIVED, not a fixed default: `auto_exit_margin_blocks_for(k_max, interval,
  depth)` = **2 120 blocks** on mainnet, **860** on regtest, over the `293·d + 375` vB /
  `3 + 2d`-transaction exit model (`exit_cost_scaling_model`,
  `clients/libs/rust-sdk/src/invalidation_model.rs`), with the wait taken per-coin from the coin's own
  chain rather than from a constant.

  **The failure mode this guards is silent absence, not a wrong trigger.** `export_watch_bundle` reads
  by key prefix; a split child is a `ctesr-` row and a spine tip a `SPINE_TIP_KEY_PREFIX` row, so a
  reader that looks only for `tesr-` and `branch-` rows finds both reads empty and takes the
  `continue` written for a flat deposit — a coin with on-chain funding that no ancestor can race,
  i.e. the exact opposite of a leaf — while still returning `Ok`. The in-process tower
  (`auto_exit_due`) covers leaves, which is what hides it: delegating to a third-party tower can
  protect the parents and leave the children unwatched.
- **Minimum parent value** for a K-batch: `1 396K + 1 310` sat (K=1 → 2 706; K=10 → 15 270;
  K=20 → 29 230). Below it, K falls back. Coloured carriers at `TOKEN_CARRIER_SATS = 17 384` support
  K ≤ 4 and must be re-sized at issue.

---

## 10. The finality trade — state it, do not hide it

The spine is **symmetric**: zero-CSV tiers accelerate the honest exit and the theft identically. That
is the mechanism, not a bug. The consequence must be published.

A payee's total on-chain warning before a steal confirms is `2 160 + s` blocks ≈ **15 days** at any
practical spine length, against multi-year windows under a two-tier-per-payment shape. You cannot
delete the latency and keep the margin — a multi-year safety window is a byproduct of a multi-year
exit that is itself unsound. ~10–15 days of required watchtower liveness is a normal L2 assumption
(LN `to_self_delay` runs 144–2016 blocks). `Δ_cap` is the dial: raising it above 1440 lengthens only
the sender's own final leg.

**The larger cost is finality, not liveness, and it is the sender's free option.** A split child has
no flat backup (`CHILD_V2_BASELINE = 0`); the sender keeps one that spends `F` for **112 vB**
(`lib/src/transaction.rs`) and pays them the whole coin — **6× to 265× cheaper** than the payee's
`293d + 375` vB walk, cheaper than the sender's *own* ladder at every fee rate, and with **zero
marginal cost** per additional piece voided. So an ordinary sender exiting for their own reasons
voids every sub-economic piece they ever paid, and the admission floor does not protect anyone:
`min_child_value` = 1 310 sat **is** the break-even function evaluated at the hardcoded
`committed_fee_rate = 2.0` (`lib/src/tesr.rs`) and at no other rate. At 20 sat/vB a depth-8 piece
admitted at 1 310 costs far more than its face to defend. The spine shrinks the `d` term and leaves
the option free, and shortens the window in which the payee could notice. See TRUST-MODEL.md.

---

## 11. Cost tables

Plain lane, `r = 2.0`, mainnet params. "Locked" = committed fee + P2A in un-broadcast tiers;
identical to burned, since it is recoverable only by broadcasting.

| shape | per payment | setup delta | locked after N |
|---|---:|---:|---:|
| two-tier change (no spine) | 2 046 | 0 | 1 470 + 2 046N |
| **spine, K=1** | **1 556** | **0** | 1 470 + 1 556N |
| **spine batch, K=10** | **1 115** | 0 | 1 470 + 1 115N |
| **spine batch, K=20** | **1 091** | 0 | 1 470 + 1 091N |
| spine batch K=20 + lean leaf (§7.7, open) | 601 | 0 | 1 470 + 601N |

Coloured per payment: 2 390 two-tier; **1 814** at K=1; **1 296** at K=10; **1 267** at K=20 — and
repeatable. Coloured locked after N on the two-tier shape is `1 728 + 2 390N`.

The fee win is bounded: 980 of the 1 066-sat asymptote is the **piece child's own two rungs**, which
batching cannot touch and only the lean-leaf variant removes. **The fee is not the reason for this
shape.** Exit is:

| shape, at N = 100 payments | txs | vB | wait | CPFP top-up @20 sat/vB |
|---|---:|---:|---|---:|
| two-tier change per payment | 203 | 29 675 | 4.08 years | 1 102 550 sat |
| spine, K=1 | 103 | 17 175 | **15.7 days** | 597 550 sat |
| spine batch, K=20 | 8 | 5 300 | **15.0 days** | **117 800 sat** |

> **These rows are the COST MODEL, not an admissibility claim.** The mainnet exit-chain cap is 19
> transactions, so the 203-tx and 103-tx rows are refused at build time by
> `enforce_split_depth_cap_shaped` — the payment forces a batch instead. What the rows show correctly
> is the ordering and the ratio: per-payment latency added to the sender's own exit horizon falls to
> **1 block**, and a payee's exit latency becomes constant in the sender's payment history.

The same model at N = 10 and N = 1000, same caveat:

| shape | N = 10 | N = 1000 |
|---|---|---|
| two-tier change per payment | 23 tx / 3 305 vB / **162 days** | 2 003 tx / 293 375 vB / **40.4 years** |
| spine, K=1 | 13 tx / 2 055 vB / **15.1 days** | 1 003 tx / 168 375 vB / **21.9 days** |
| spine batch, K=10 | 4 tx / 930 vB / **15.0 days** | 103 tx / 55 875 vB / **15.7 days** |
| spine batch, K=20 | 4 tx / 1 360 vB / **15.0 days** | 53 tx / 49 625 vB / **15.3 days** |

(K=10 at N = 100 is 13 tx / 5 925 vB / 15.0 days. The lean-leaf variant does not change tx count or
vB; it takes the payee's wait down by a further 720 blocks.) Against the two-tier shape that is
**95× on latency at N = 100**, **673× at N = 1000**, and **9.4× on the realised exit cost** that
decides whether the exit is solvent at all.

Realised exit cost at a live fee rate (external CPFP top-up, N = 100 payments, ~152-vB child per
tier because TRUC admits one unconfirmed child and the CSVs are sequential):

| live rate | two-tier | spine K=1 | spine batch K=20 |
|---:|---:|---:|---:|
| 5 sat/vB | 194 585 | 105 085 | **20 060** |
| 20 sat/vB | 1 102 550 | 597 550 | **117 800** |
| 50 sat/vB | 2 918 480 | 1 582 480 | **313 280** |

**This is the largest number in the document.** On a 1 000 000-sat deposit a two-tier-per-payment exit
is insolvent above ~15 sat/vB; the batched spine's is solvent to well past 50.

---

## 12. Alternatives not built, and what kills each

### 12.1 DFO — Denominated Fan-Out (one split, N self-owned leaves, then whole-leaf handovers)

Fan the deposit into N denominated leaves at claim; pay by exact-subset handover; handle the residue
by payee-makes-change or a swap.

**Killed by: an irreversible one-way commitment whose transferability depends on an artifact it has
just made un-renewable.** `in_ladder_split` calls `set_spend_budget(parent, 1)` and `SP` consumes it,
so `sign/first` and `sign/second` return 410 Gone thereafter (`server/src/endpoints/sign.rs`) and
`set_sig_budget` can only tighten (`server/src/database/deposit.rs`). There is **no second fan-out and
no re-denomination**. Meanwhile every leaf hop re-runs `validate_backup_chain_v2` against the **live**
tip and fee rate (`clients/libs/rust/src/tesr.rs`), which rejects on a two-sided ±5 sat/vB fee band
(`lib/src/transfer/receiver.rs`) — and `auto_refresh_before_spend()`
(`clients/libs/rust-sdk/src/transfer.rs`) has no subject left, because after a fan-out every coin is a
terminal parent or a `ctesr-` child. A fee move of >5 sat/vB in either direction makes **all N leaves
simultaneously un-conveyable** with no remedy but a full unilateral exit.

Cost columns, for comparison with §11: **0 per payment on-grid**, `E[u/2]` off-grid; setup delta
**1 066N − 86, recurring 9.2×/yr**; locked `1 384 + 1 066N`; exit at N = 10 is 5 tx / 1 012 vB /
29.8 days, and at N = 100 the exit is *unreachable* because the tree is terminal.

Compounding it: DFO universalises the exit-headroom hazard, so near an epoch boundary it can hand
payees provably unexitable coins; N ≤ 63 from the derived-token lifetime cap; and
`colored_multi_carrier_transfer` never admits children as legs
(`clients/libs/rust-sdk/src/tokens.rs`), so after a fan-out the wallet reports **"COLOURED carriers
hold 0 in total"** while holding the entire deposit.

Economics, even setting safety aside: with its own recommended binary ladder the ceiling is
**17.3×** — rows above that exceed the N·36 leaf-hop budget, since a leaf survives exactly
`(1440−144)/36 = 36` hops (`child_supersede_csv`). With **no leaf return** it is **1.0×** — identical
to a plain split. And the fan-out **recurs 9.2×/year** (the tree must fully materialise before
`H_deposit + initlock`, and materialisation itself consumes most of the epoch), so a 10-leaf lattice
on a 1M deposit burns **9.72 %/yr regardless of payment count** — a loss for any wallet under ~46
payments/year.

### 12.2 DENOM-SWAP — fixed-denomination lattice with atomic-batch reshaping

Hold a lattice of denominations; pay by exact subset; reshape via the N-party atomic batch transfer
with an SSP, value-conserving and coin-for-coin.

**Killed by: the batch primitive is not atomic, and the sender's veto is bypassable without a
signature.** Three independent breaks, all in shipped code, and items 2 and 3 are CURRENT security
gaps in `lightning_latch` independent of this design:

1. **An aborted leg permanently bricks the coin.** `presign_receiver_state` co-signs `S'` on a
   **clone** and does not mutate the sender's bundle (`clients/libs/rust/src/tesr.rs`), but the SE's
   `sig_count` increments regardless. The sender keeps a bundle whose census can never balance: on
   ROLLBACK the orphan `S'` co-sign inflates the reclaimed coin's `sig_count`, so a later
   `verify_bundle` bricks re-transfer (`clients/libs/rust/src/transfer_sender.rs`,
   `clients/libs/rust-sdk/src/ssp.rs`). One stalled leg bricks **all K′** of the user's outgoing
   coins; recovery is K′ on-chain re-anchors; and for an RGB allocation sitting on a **plain** ladder
   there is no recovery at all. Worse, the SSP then holds a co-signed `S'` at `csv − δ` while the
   user's retained `S` sits at `csv` — the SSP's rival matures **first**, and the tree states the bar:
   the SSP holds the broadcastable `S'` and is trusted not to race it. That is operator trust, not
   atomicity.
2. **A caller-supplied `batch_id` would delete a live guard.** `post_paymenthash` validates only that
   the caller signed for its **own** `statechain_id` (`server/src/endpoints/lightning_latch.rs`) — no
   check that it is entitled to `batch_id`. That is contained today only because `create_pre_image`
   mints a fresh UUID client-side (`clients/libs/rust/src/lightning_latch.rs`). Make `batch_id`
   caller-supplied and anyone who learns one self-registers into it and wedges every honest leg.
3. **Theft.** `post_paymenthash_external` accepts any `batch_id` with an attacker-chosen
   `payment_hash` (`server/src/endpoints/lightning_latch.rs`); `unlock_by_preimage` then enumerates
   **every** `statechain_id` in the batch by `batch_id` alone
   (`server/src/database/lightning_latch.rs`) and clears `locked2` — the **sender's veto** — with no
   signature from those senders (`server/src/database/transfer_receiver.rs`). An SSP knows the
   `batch_id` by construction. It can clear the veto, unlock its own legs, create no outbound legs,
   and claim every coin the user put in.

Additionally: a recommended denomination `b = 2 000` is below the **maintenance bound** — `reanchor`
refuses unless `amount − ceil(112·r) ≥ 330` (`clients/libs/rust-sdk/src/refresh.rs`), so a 2 000-sat
coin is unmaintainable above 14.9 sat/vB — and a defensible `b` of 10 000–20 000 makes the off-lattice
rounding residual (`E[b/2]`) **worse than the 2 046-sat two-tier cost**.

Cost columns: **0 per payment on-lattice**, `E[b/2]` off; setup delta K onboarding tokens + `11·43·r`
vB; locked `K · 1 800`, flat; exit a flat 3 tx / 375 vB / 14.75 days at every N, comfortably inside
the 19-transaction cap. It is rejected for the three breaks above, not on cost.

### 12.3 Denominations as an opt-in mode

A batch already produces K self-owned pieces if they are pointed at the holder's own backup address; a
later payment of exactly that piece's amount is then a free `child_retransfer`. That is worth doing
for a **repeating fixed-amount book** (payroll, subscriptions, exchange withdrawal tiers, LSP
rebalancing) and nothing else. The gate is utilisation: a carved batch of K beats a plain spine iff
**more than `0.685K + 0.315` pieces** are consumed as exact matches — ~69 % of any K. Carve 20, use
10, and it is a loss. This mode is **not built**.

### 12.4 Batching alone, without the spine

Not rejected — **absorbed**; batching is one of the two composable properties of what shipped. On its
own it divides depth by K but leaves the leading term at the two-tier level cost, so at K = 20 a
1000-payment history still costs **103 txs and 2.06 years** to exit. The zero-CSV spine is what
removes that term; batching alone does not.

---

## 13. What does NOT improve

None of this is fixed by the spine, batching, denominations or swaps.

**The ~69-day root epoch survives untouched.** The depositor holds a flat backup maturing at
`H_deposit + lockheight_init` — **10 000 blocks ≈ 69.4 days on every mainnet-schedule network**
(`TesrParams::flat_ladder_params`: `bitcoin`/`testnet`/`signet` = 10 000/100, regtest = 1 000/10).
`T` is un-timelocked and spends `F`, so strictly the obligation is that **`T` confirm before the
earliest live flat-backup locktime** — once `T` confirms, `F` is spent and every flat backup is dead,
and the remainder of the chain is relative-only with no absolute deadline.

**One on-chain re-anchor per tree per epoch is unavoidable — and for a split tree it is not one
transaction.** For a coin that has never been split, `refresh()` → `reanchor()` is a clean 1-tx /
112-vB reset. For a tree that has made even one partial payment it does not exist: the root is
terminal (`set_spend_budget(…,1)` consumed by `SP`), so the SE refuses to co-sign
(`server/src/endpoints/sign.rs`) and `withdraw` has no confirmed outpoint to spend. The only
re-anchor is **full unilateral materialisation followed by a fresh deposit**, which the batched spine
makes affordable (8 txs, 15 days at N = 100 — comfortably inside a 10 000-block epoch) without
removing it. **Depth resets only there.**

To actually move it you would need one of:

- **Raise `lockheight_init`.** It lengthens the depositor's clawback window and therefore the trust
  window, and it is **not a per-deployment dial**: clients compile in
  `TesrParams::flat_ladder_params(network)` and refuse any coordinator whose `initlock`/`interval`
  disagree, and the coordinator **panics at boot** rather than serve a mismatched pair
  (`server/src/server_config.rs`). Changing it is a protocol change shipped on both sides at once,
  not a compose edit.
- **A co-operative de-trigger for terminal trees** — the SE co-signing a fresh spend of `F` after the
  tree is terminal. Requires raising the spend budget on a terminalized statechain *and* a protocol
  for invalidating every live child with its holder's consent. Hard; **not designed.**
- **A child re-anchor primitive.** Structurally impossible as posed: a child's funding `SP.out[j]` is
  un-broadcast, so there is no confirmed outpoint to spend, and producing one *is* the on-chain
  transaction you were trying to avoid.

**Also unchanged:**

- **The payee-borne 980 sat** (1 152 coloured) per received piece. Only the lean-leaf variant (§7.7,
  open) touches it, halving it to 490. Batching and the spine do not.
- **Depth never RESETS** — it is bounded, not reset. `enforce_split_depth_cap_shaped` refuses past
  `max_split_depth` (**8 on mainnet**) and past `max_exit_txs` (**19 transactions**), the latter
  evaluated above the latency rule precisely because a spine level is cheap in blocks and not in
  transactions. The spine makes each level cost one tx and one block; batching divides the level
  count by K; the cap is what turns "unbounded" into "priced".
- **A child can never be RE-ANCHORED.** `refresh()` routes a `ctesr-` coin through `withdraw` to
  `unilateral_exit`, because `SP.out[j]` is un-broadcast and there is no confirmed outpoint to
  co-operatively spend. **It CAN be RENEWED**: `renew_child` / `renew_child_auto` rebuild
  `child_extension` + `child_state` in place over the same `SP.out[j]` — +2 co-signatures, +2
  superseded entries, census unchanged — for zero on-chain bytes and no depth. The refusal string
  names it (`child_supersede_csv`).
- **A coloured carrier CAN be re-anchored, if its ladder is coloured.** `colored_reanchor`
  (`clients/libs/rust-sdk/src/refresh.rs`) broadcasts the trigger if it is not already on chain and
  then a co-signed **coloured de-trigger** carrying a valid state transition — two transactions, no SE
  change. What remains dead is the crossed pair, and both lanes refuse it by name: a coloured carrier
  down the plain `refresh` (which would destroy the allocation), and an RGB allocation sitting on a
  **plain** ladder, which `colored_reanchor` cannot help with — a coloured de-trigger needs coloured
  material to build from, so such a coin must be moved off-carrier first. That residue is what dies at
  its root epoch.
- **The 36-hop CSV budget, now renewable.** A child survives `(1440−144)/36 = 36` whole-coin handovers
  per epoch (`child_supersede_csv`), and `renew_child_auto` steps the extension one rung down and
  resets the state to `state_csv(0)`, so the budget is **36 hops × 16 epochs** (`m_max + 1`) per depth
  level. A leaf that has itself made a partial payment is TERMINAL at the SE and cannot renew —
  `renew_child` refuses that by name, pre-flight, before burning a co-signature. **`CoinInfo`
  (`clients/libs/rust-sdk/src/types.rs`) exposes no `hops_remaining`, and no such field exists
  anywhere in the tree, so no wallet can warn a user that a received coin is one hop from needing a
  renewal it may not be entitled to.**
- **Nothing is offline.** Every payment needs an authenticated derived-token draw and SE co-signs.
  The spine buys depth, latency and fees — not availability.

---

## 14. Open work

| # | item | gate | status |
|---|---|---|---|
| 1 | Per-output blinding on the coloured lane (§8.4) | none | **OPEN** — until it lands, coloured K > 1 only for mutually-known payees |
| 2 | Whole-coin handover of a spine tip — promote it to an ordinary two-tier child, census `0 + 2 + 1` | none | **OPEN** — refused by name today |
| 3 | The sweep / absorption path S1–S6 (§3.6) | S1 first: it is the gate | **DESIGN** — `combine_leaves` has zero callers outside a test |
| 4 | The discharge round (§4, SPEC.md §5.4) | reconciling depth admission against materialisability (SPEC.md §5.4.5 REQ-63.4) | **DESIGN** — enforcement point empty, 0 occurrences in `lockbox/` |
| 5 | Opt-in self-carve inventory (§12.3) for fixed-amount books | utilisation gate `> 0.685K + 0.315` enforced in the planner | **OPEN** |
| 6 | Lean leaf (§7.7) — separate, argued decision | forfeits child renewal, which exists (`renew_child`) | **OPEN** |
| 7 | A v3/TRUC tier above 10 000 vB never relays (§5) | none | **OPEN finding**, unaddressed |
| 8 | The `lightning_latch` holes (§12.2 items 2 and 3) | none | **OPEN** — no swap primitive can be exposed safely until they are closed |

Related: SPEC.md (normative constants and the round), PROTOCOL.md (the tier machine),
TRUST-MODEL.md (the finality option and the residual trust surface), CHILDREN.md (child lifecycle,
renewal and conveyance), LIGHTNING.md, README.md.
