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
> watchtower event trigger; child renewal (`renew_child`); and **REQ-83's leaf bands**
> (`LeafShape::for_value`, `lib/src/tesr.rs`) — a payee's leg is two rungs, one rung, rung-less at
> dust, or a sub-dust tail, and the piece floor `split_output_floors` applies is **1 satoshi**, not
> `min_child_value`. The bands below one rung do not exit unaided; that is an owner decision, not a
> gap.
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
>
> **Rule of 2026-09-06 — NO FLAT BACKUP, NO CALENDAR.** No coin carries an absolute-locktime backup
> transaction (`create_tx1` is deleted; none at deposit, none at any hop). A coin's only exit
> material is its TES-R ladder, co-signed at the FIRST MEMPOOL SIGHTING of its funding transaction
> (`coin_status::check_deposit` under `LadderAtSight::Plain`; `claim()`'s establish pass under
> `LadderAtSight::Defer` — and where that SDK pass cannot run, the deposit is BOOKED WITH NO EXIT
> MATERIAL and cooperative withdrawal is the only way out; §7.1 states the two lanes apart and they
> must not be quoted as one). `coin.locktime` is `None` for life and nothing on a coin matures on its
> own. `initlock` survives only as the FIXED exit window the depth and length caps measure a walk
> against (§5); `interval` is applied to nothing. Every figure in this document that was keyed on
> the ~69-day funding epoch — rounds per year (§4), the forced settlement path (§3.4), the leaf's
> inherited deadline (§9), the splitter's free void (§10), the recurring fan-out (§12.1), the root
> epoch (§13) — is re-derived or retired in place below, dated.

---

## 1. THE CENTRAL RESULT — this design sells payment VELOCITY, not payment granularity

Everything else in this document is a detail of this one claim, and the claim has a side on which we
LOSE. State both sides or the number is marketing.

### 1.1 The honest alternative

A one-to-many payout on Bitcoin is **ONE transaction with N+1 outputs**, not N transactions. Paying
100 recipients on chain costs **4 412 vB — about 44 vB per recipient** (`sweep_tx_vsize(1, 101)`,
`lib/src/transaction.rs`, which rounds the weight UP; this document published 4 411 until
2026-09-06, the truncated form). Any comparison that prices the on-chain alternative at
"N × 155 vB" is inventing an opponent that does not exist, and every favourable ratio derived that
way is worthless.

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
* It is also unreachable as a payment path in practice — but **not for the reason this section used
  to give.** `min_child_value` is `2·rung + dust`, **1 560 sat** at the shipped rate, and until
  2026-09-06 this bullet inferred from it that "the finest piece the protocol will mint is 1 560
  sat, so no coin set can be made fine enough for a subset sum to land on an arbitrary amount's
  residue". **That inference is WITHDRAWN.** 1 560 is the floor of the two-rung band only; REQ-83's
  leaf bands (`LeafShape::for_value`, `lib/src/tesr.rs`) mint below it on purpose — a one-rung
  `ThinPiece` at `min_spine_tip_value` = 945, a rung-less `Ladderless` stub at the dust limit, and a
  sub-dust `Tail` — and the piece floor `split_output_floors` actually applies is
  `SplitLegRole::Tail.min_value`, **1 satoshi**. A fine grid is therefore mintable. What survives is
  the measurement rather than the floor argument: simulated against a realistic mix
  (V = 1 M, 3 000 payments log-uniform 3k–150k, binary grid, the real `exact_subset` DP in
  `clients/libs/rust-sdk/src/select.rs`) the exact-subset hit rate with no leaf return was
  **5 in 3 000**. Whether a grid built deliberately out of sub-rung bands would do better is
  UNMEASURED, and it would be bought with exit dependence: those bands do not exit unaided
  (`LeafShape::exits_unaided` is false for them).

**Do not quote root-holder economics.** A root holder is the depositor. Theirs is the most flattering
number in this document — **0 vB/yr idle**: a laddered coin has no calendar (INV-27 is unconditional),
and its only on-chain event is the cooperative re-anchor at the rollover cap, which is spent by HOPS,
not by time (§13) — and it describes almost nobody once payments start flowing. After the first
payment, everyone downstream is on the leaf lane. *(The ~589 vB/yr this paragraph used to quote was
5.26 re-anchors/yr × 112 vB, keyed on the 10 000-block funding epoch. RETIRED 2026-09-06: no such
epoch exists.)*

### 1.3 The per-payment block-space ledger

| leaf lane, per payment | block space | against ~154 vB on chain |
|---|---:|---|
| **spent onward off-chain** | **0 vB** | this is the product |
| **swept and settled** (§3) | **~105 vB** | **1.47× better — and this is the CAP without the round** |
| **walked out unilaterally** | **668 – 2 719 vB** | **WORSE than on-chain** |
| **shipped default** | **418 vB** | 2.7× worse |

The walked range is the leaf's own exit chain, `293·d + 375` vB over `3 + 2d` sequential
transactions (`exit_cost_scaling_model`, `clients/libs/rust-sdk/src/invalidation_model.rs`); the top
of the range is the mainnet depth cap of 8 (`293·8 + 375 = 2 719` vB over 19 transactions). Walking a
depth-1 leaf out — the SHALLOWEST a leaf can be — is `293 + 375 = ` **668 vB over 5 sequential
transactions, 4.3× WORSE** than doing the payment on chain. *(This read "250 vB — 1.62× worse" until
2026-09-07. 250 is not a value the model produces at any depth: `exit_cost_scaling_model`
(`clients/libs/rust-sdk/src/invalidation_model.rs`) pins 375 at depth 0 and 668 at depth 1, and the
depth-0 figure is a root's walk, not a leaf's. The understatement was in the flattering
direction.)*

The **~105 vB** swept row amortises the shared prefix across a whole tree (§1.4). The sweep MARGINAL
alone — one further leaf into an existing batch — is **58 vB** (`COMBINE_MARGINAL_VB` = 57.75,
`lib/src/sweep.rs`), i.e. **0.38×** the on-chain 154; that is the quantity §2 prices, and it is a
floor the tree-wide figure never reaches.

**The 418-vB "shipped default" row is UNVERIFIED.** No derivation for it survives in this document,
and the obvious readings do not produce it from the corrected walk figures above. It is left as
written rather than guessed at; whoever restates the model should either derive it or drop the row.

**For the population that actually exists, the shipped default settles a payment for MORE block
space than doing it on chain.** That is the sentence to lead with. The sweep (§3) is what changes
it — not an optimisation on a winning position, but the precondition for the median user's
block-space economics being positive at all. The discharge round (§4) was costed as the
order-of-magnitude change; it is RETIRED (SPEC.md §5.4.0) and, with no calendar, has no cadence to be
costed at.

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
| **0.00** — nobody hops, nobody sells | 29 800 vB | **4 412 vB** | **ON-CHAIN, 6.8×** |
| 0.50 | 20 241 | 12 112 | ON-CHAIN |
| 0.70 | 16 396 | 15 192 | ON-CHAIN |
| **0.74** | 15 627 | 15 808 | **crossover** |
| 0.90 | 12 551 | 18 272 | utexo 1.5× |
| 1.00 | 10 628 | 19 812 | utexo **1.87×** |

(The ON-CHAIN column is the 4 412-vB batched payout plus `100s` onward payments at ~154 vB.)
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

**An absorber's margin is `1 230 − 57.75 × market` sat per leaf** — `surplus_sats`
(`lib/src/sweep.rs`), which floors: **1 056** sat at 3 sat/vB, **652** at 10, and **zero at
21.3 sat/vB**, above which the prepaid committed rate is the better deal and holders should simply
walk. (`BURN_SATS = 1 230` is `2 × 615`, i.e. the SHIPPED 3.0 rung — the same constant the §2.1
ledger is now derived at.) It is an INVERSE-fee-market business: it earns most when fees are low,
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
| `SP` / `CSP` split tier (2 payloads) | 744 | 873 | `lib/src/tesr.rs`; `clients/libs/rust/src/rgb.rs` |
| piece child — extension + state rung | 1 230 | 1 488 | `establish_child`, `clients/libs/rust/src/tesr.rs` |
| change child — extension + state rung | 1 230 | 1 488 | same |
| less the superseded state rung `SP` replaces | −615 | −744 | `clients/libs/rust/src/tesr.rs` |
| **system total, per partial payment** | **2 589** | **3 105** | |

*(Re-derived 2026-09-07 at the SHIPPED rate. This table read 576 / 980 / 980 / −490 / **2 046**
plain and **2 390** coloured until then — every one of those is the same formula evaluated at
r = 2.0, which the code itself calls "the superseded 2.0" in `SplitLegRole::min_value`. The shapes
did not change; the rate the tiers are signed at did.)*

The shipped plain-root and coloured-root lanes replace the change child with a one-rung spine tip,
which removes one rung (615 plain / 744 coloured) and one whole level of latency — see §7 and the
cost table in §11.

Derivation of the units:

```
committed_fee(r)                = ceil(125·r)                     lib/src/tesr.rs   -> 375 @ r=3
committed_fee_for_outputs(n,r)  = ceil((125 + 43(n−1))·r)         lib/src/tesr.rs
colored_committed_fee(n,r)      = ceil((168 + 43(n−1))·r)         clients/libs/rust/src/rgb.rs
P2A_VALUE                       = 240                             lib/src/tesr.rs
rung  = committed_fee + P2A     = 615 plain / 744 coloured        (r = 3.0, THE SHIPPED RATE)
min_child_value(3.0, 330)       = 2·615 + 330 = 1 560             lib/src/tesr.rs
colored_child_floor(3.0, 330)   = 2·744 + 330 = 1 818             clients/libs/rust/src/tesr.rs
min_spine_tip_value(3.0, 330)   = rung + dust = 945                lib/src/tesr.rs
colored_spine_tip_floor(3.0,330)= rung + dust = 1 074              clients/libs/rust/src/tesr.rs
mainnet params = { d0 1440, δ 36, d_floor 144, e0 720, δE 36, e_floor 144, m_max 15, rate 3.0 }
                                                                  lib/src/tesr.rs
```

Every floor above takes the rate as an ARGUMENT, so an evaluation at another rate stays internally
consistent while being wrong about the system that ships — which is the defect
`ci-guards/tests/deny_stale_committed_fee_figures.rs` exists to catch, and which this section
carried.

The toll is **flat and amount-independent**: 2 589 sat is 25.9 % of a 10 000-sat payment, 2.6 % of
100 000. Who pays: the sender loses 1 359 (its own change leg plus the split tier, less the
superseded rung), the payee loses 1 230 off the nominal — a 10 000-sat piece is worth **8 770** on
unilateral exit, which is exactly what `sweep::fair_price_floor(10_000)` returns and what
`lib/src/sweep.rs` pins in `the_fairness_floor_never_takes_value_from_a_payee`.

**The exit fee is not prepaid.** `committed_fee_rate` is a per-network protocol constant, **3.0** on
every shipped preset — `TesrParams::mainnet()` and `TesrParams::regtest()`, `lib/src/tesr.rs`,
[D44] — not the live rate. (2.0 survives only as a unit-test fixture; `TesrParams::mainnet()`'s own
doc-line still says "2 sat/vB committed fee" beside a literal of 3.0, which is a stale comment in
the code, not a second rate.) Above ~5 sat/vB every tier must be CPFP'd through its P2A, and TRUC's
one-unconfirmed-child rule plus the sequential CSVs forbid batching those children. The realised
top-up from an external funded wallet is the largest number in this document — see §11.

**The quote is the executor's own plan, and there is no other lane left for it not to be.**
`quote_transfer` runs the executor's planner and preflight, so `fee_sats` and the per-leg
`SplitFloors` come from the same `split_preflight` the executor obeys and `fundable` is what the
executor will do rather than an estimate. This used to be qualified — "on the laddered lane" — because
the `split_fee_reserve` clamp, `clamp(parent/100, 300, 2000)`, still priced the un-laddered plain
split, where the quote was an estimate. **That lane is deleted**: `split_coin`, `ParentShape::Unladdered`
and `ManyRoute::PlainSplit` are gone, and `parent_shape` refuses a coin with no ladder instead of
quoting it at the cheaper model — which was the silent-degradation shape [B3] made the resolution
fail-closed for. The clamp itself survives, and deliberately: it is the arithmetic behind
`split_amounts_floored`, which is the executable dust-boundary spec the invalidation and granularity
models are written against, and which sizes the LEGACY coloured split/combine lane
(`TOKEN_CARRIER_SATS` is derived from it). The BOUNDARY was never un-laddered-only — every split leg
still has to clear dust and fund what it owes — so what retired is the routing, not the arithmetic.

### 2.2 The splittability tail — a property of the TWO-TIER-CHANGE shape, not of the shipped lane

> **Read this section as the two-tier-change model, and not as the limit a wallet hits.** It assumes
> BOTH legs are floored at `min_child_value`, and neither half of that assumption is what ships: the
> plain-root and coloured-root lanes give the change a one-rung spine tip (§7.3), and REQ-83 floors
> the PIECE leg at `SplitLegRole::Tail.min_value` — 1 satoshi — which is what `split_output_floors`
> hands the planner (§9). The worked example's literals are additionally an evaluation at r = 2.0,
> the superseded committed rate, and are NOT re-derived here.

Under that model splitting requires the change to fund two floored children — absolute minimum
`c ≥ 2·1 560 + 1 359 = 4 479` plain, `2·1 818 + 1 617 = 5 253` coloured, at the shipped rate. *(The
figures printed here until 2026-09-07 were `3 686` / `4 202`; those were the r = 2.0 evaluation, and
the plain one did not even balance against the 1 560 it was written beside — the arithmetic slip a
half-finished rate correction leaves behind.)* Below that minimum the change is still exitable but
cannot make another partial payment **of this shape**.

Worked at V = 1 000 000 and 10 000/payment, two-tier change, **at r = 2.0**: `c_N = 999 510 −
11 066N`, so payment 91 is refused — 900 000 nominal delivered, 811 800 exitable, 185 610 sat
(18.56 % of the deposit) burned, and the survivor is a 3 570-sat depth-90 coin worth 2 590 on exit.
**Every number in that sentence is at the superseded rate and has not been re-derived; at the
shipped 3.0 the per-payment drain is larger and the cut-off therefore earlier.** The companion
figures for the spine-tip change — "the reach extends to ~94 payments at K = 1 and further under
batching, with 147 734 sat (14.8 %) of reserve" — rest on the same superseded footing. What is
current is the tip's own floor, **945**, and the fact that the splittable floor is a function of two
rates rather than a publishable literal (§9).

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

The swap belongs in `claim()`, at the moment a payee first sees the leaf: the user is online because
they are already transacting, and no separate coordination round is needed. The payee receives an
ordinary root coin and never handles a leaf. *(This section used to add "runway is maximal — the
inherited deadline is furthest away". RETIRED 2026-09-06: a leaf inherits no deadline. Its laddered
parent holds no flat backup, and nothing on it matures.)*

**A root is strictly better for the payee than the leaf it replaces**, independent of any spread:
depth 0 (a three-transaction, ~375-vB unilateral walk instead of `3 + 2d`), a one-transaction
cooperative exit and a cooperative re-anchor a leaf can never have (§13), and no watchtower duty tied
to an ancestor's `F` it does not control — a leaf's tower must answer the EVENT of an ancestor's `T`
confirming (§9); a root's need only answer its own. That is what makes a silent swap defensible rather
than extractive — subject to the fairness condition in §3.5.

### 3.3 The absorption predicate

Absorb a leaf iff ALL hold (three live rows; the runway row is retired):

| condition | default | derivation |
|---|---|---|
| `market_fee_rate ≤ sweep_max_fee_rate` | **15 sat/vB** | surplus hits zero at 21.3; 15 keeps a ~30 % margin — `surplus_sats(15.0)` = **363** sat/leaf (this row read 369 until 2026-09-07, which is not the value the function returns) |
| ~~`runway_blocks ≥ sweep_min_runway`~~ | ~~903 blocks~~ | **RETIRED 2026-09-06.** `runway_blocks` was `inherited_deadline − tip`, and no laddered leaf has an inherited deadline. `SweepLimits::min_runway_blocks` and the `runway_blocks` parameter of `may_absorb` (`lib/src/sweep.rs`) survive in the pure predicate with no live source for their value — see §14 item 9 |
| `leaf_value ≤ sweep_max_leaf_value` | **100 000 sat** | the value at which a constant ~1 057-sat surplus falls below 1 % of face |
| `tree_exposure + leaf_value ≤ sweep_max_tree_exposure` | **1 000 000 sat** | `target_batch × max_leaf_value`; bounds loss if one tree's spine cannot be materialised |

### 3.4 WHEN to settle — the absorber holds an option and should price it as one

Having absorbed, the absorber is not obliged to settle promptly. It holds a **timing option**: settle
at the cheapest fee window. There is no runway to settle inside — an absorbed leaf, like any laddered
coin, does not age (§13). Exercise when:

* `batch_size ≥ sweep_target_batch` **and** `market ≤ sweep_max_fee_rate` — the voluntary path.
* ~~`earliest_deadline − tip ≤ sweep_min_runway` — the **forced** path~~ — **RETIRED 2026-09-06.**
  Its trigger was the parent's lowest flat-backup locktime, which no laddered parent has; nothing on
  chain forces settlement of an absorbed leaf by a date. `should_settle`'s `earliest_runway_blocks`
  parameter (`lib/src/sweep.rs`) survives with no live source for its value. A replacement forcing
  condition, if one is wanted, is open (§14 item 9).

What CAN happen to an absorbed leaf before it is settled is an EVENT, not a date: an ancestor's
retained trigger `T` confirming over `F` — a prior owner exiting, or a griefer. From that block the
leaf's own walk is under way, the absorber's tower must push the chain (§9), and the leaf settles by
WALKING — burning its 1 230 sat of pre-signed tiers — rather than by the combine. The absorber loses
the surplus, not the face.

`sweep_target_batch = 10` captures 94 % of the achievable batching gain; beyond it the curve is flat
and waiting only adds fee-market exposure and the event exposure above.

**The risk is asymmetric and must be stated that way.** Settling too EARLY costs a few hundred sat of
foregone batching. Settling too LATE no longer costs the whole leaf — that loss needed a matured spend
of `F` in a prior owner's hands, and no prior owner holds one (§10) — but it leaves the ~1 057-sat
surplus exposed to the ancestor-trigger event for as long as the leaf sits unsettled. Every default
above is still biased toward acting early.

### 3.5 The fairness condition

A silent swap must leave the payee **no worse off than holding the leaf**, measured against the
leaf's own realisable value:

```
price_paid ≥ leaf_value − BURN          (what the payee would realise walking it out)
```

At the floor that means paying at least 330 sat for a 1 560-sat leaf — while the absorber realises
1 387. There is ~1 057 sat of surplus to divide, and the split is `sweep_spread_bps`, a policy
parameter and not a protocol constant. Two obligations follow: the payee is handed a coin that is
**strictly better in kind** (root: depth 0, re-anchorable), and the spread is disclosed in aggregate
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
| **S5** | the settlement scheduler (the voluntary path) plus the absorbed leaf's EVENT duty (§3.4) | needs S1–S4; the forced path is RETIRED — there is no deadline to key it on | a test that an ancestor's `T` confirming during absorption gets the leaf's chain pushed, with no fee ceiling gating that push |
| **S6** | publish the realised curve from a live fleet | §1.4's break-even is modelled, not measured | measured hops-per-leaf and settlement cost against the model |

**S1 is the gate.** It decides whether this is a 4 %-margin batching play or a ~1 057-sat-per-leaf
value-recovery business.

---

## 4. THE DISCHARGE ROUND — the footprint scales with PIECES, not with PAYMENTS

> **Costs a design that does not exist — and is RETIRED** (SPEC.md §5.4.0, owner decision
> 2026-08-20: the protocol MUST NOT require operator liquidity, and the round did). The enforcement
> point is empty: `disclosure` / `prevout_value` occur 83× in the client and **0× in `lockbox/`**.
> These are the numbers it *would* cost built, not numbers anything measures.
>
> **And since 2026-09-06 the round has no cadence.** Every "rounds/yr" figure below was
> `365 ÷ 69.4 = 5.26`, i.e. one round per 10 000-block funding epoch. There is no funding epoch: no
> coin carries an absolute-locktime backup and nothing on a coin matures (§13). A round, if one were
> ever scheduled, would run at a cadence chosen by POLICY, and the protocol supplies none. The tables
> are kept as the historical costing at 5.26/yr; do not quote them as a live rate.

### 4.1 The structural result

A round re-mints **every outstanding leaf** regardless of how many payments produced it, and retires
the old tree in **one transaction**. So the on-chain footprint is set by

```text
    footprint  =  (outstanding pieces ÷ 256) × (365 ÷ epoch_days) × (155 + 43·absentees)
                   └── tree count ──┘          └── rounds/year ──┘   └── one collapse tx ──┘
```

and **payment volume appears nowhere in it.** (`epoch_days` was the funding epoch, 69.4 days; since
2026-09-06 it is a policy parameter the protocol does not set.) 256 is a hard cap, not an assumption: a depth-8 tree
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
2. **Round cadence — linear, and no longer a protocol constant.** `initlock = 10 000` used to set
   5.26 rounds/yr because it was the calendar on which every flat backup matured. *(RETIRED
   2026-09-06.)* `initlock` is now the FIXED exit window the depth and length caps measure a walk
   against (§5), and nothing matures on it, so a round's cadence would be a policy choice with no
   floor from the protocol — the lever is unbounded in the cheap direction. Depth remains a
   *usability* dial rather than a safety limit — gated on reconciling depth admission against
   materialisability (SPEC.md §5.4.5 REQ-63.4).

**Quote the worst case, not the best** (SPEC.md §5.4.6): exit-key reassignment lets any holder force
a payout instead of a migration, free and unattributable.

---

## 5. Admission — depth, exit length and headroom

Two caps bound what may be minted and what may be received, both enforced today, and both measured
against one FIXED window: `initlock` — 10 000 blocks on the mainnet-schedule networks, 1 000 on
regtest — read from the receiver's own `/info/config` fetch and compiled into
`TesrParams::flat_ladder_params` (`lib/src/tesr.rs`) on the client. The window is a constant, not a
calendar: a laddered coin has no absolute-locktime backup and no epoch deadline, so there is no
"remaining window" to measure against the tip. What the caps bound is the LENGTH and LATENCY of the
exit walk a leaf would inherit, and the answer does not depend on when the sender conveys.

* **Depth.** `max_split_depth` (`lib/src/transfer/receiver.rs`), enforced build-side by
  `enforce_split_depth_cap_shaped` (`clients/libs/rust/src/tesr.rs`) from the live schedule and the
  fixed `initlock` window. The rule is the latency one with the admission margin included —
  `exit_wait_blocks(chain) + exit_slack_margin(chain) ≤ initlock` — so the caps are **depth 8 on
  mainnet** and **depth 54 on regtest**; a deeper child is minted by nobody. Every input is
  receiver-derived: CSVs come off the signed `nSequence` (`child_exit_chain_bound`), the schedule from
  `cap_schedule` (the receiver's own preset, refusing a conveyed one that contradicts it), the window
  from `/info/config`.
* **Exit-chain length.** `max_exit_txs = 3 + 2·max_split_depth` — **19 transactions on mainnet**,
  **111 on regtest** — enforced by `enforce_exit_chain_length` on the build side (inside
  `enforce_split_depth_cap_shaped`, **above** the latency rule's early return) and on the receive
  side: for a conveyed child in `verify_conveyed_child` (`clients/libs/rust/src/tesr.rs`), for a
  conveyed root ladder in `validate_encrypted_message` (`clients/libs/rust/src/transfer_receiver.rs`).
  The latency rule alone cannot see this: a spine tier costs one block of latency and a whole
  transaction, so an all-spine chain of thousands of tiers passes the latency test and is still
  unusable. Each level is charged by its real shape (`SplitLevelShape`).

*(RETIRED 2026-09-06: the receive-side **exit-headroom** gate. `check_exit_headroom_with_margin`
admitted a conveyed child only if its walk fitted inside the epoch the payee inherited — `available =
epoch_expiry − tip`, the lowest locktime of the parent's flat backup chain. No such chain exists, so
the function has no caller on the receive path; `verify_conveyed_child` binds the chain's timelocks to
their signatures and measures the length cap against the fixed window instead. The defect the gate
closed — a sender handing over a coin that could not be materialised before the sender's own flat
backup spent `F` — cannot arise, because no sender holds a matured spend of `F` (§10). Its E2E,
`clients/tests/rust/src/sdk82_exit_headroom_gate.rs`, mined toward a flat backup's maturity and is
being re-derived: **pending run, not evidence.**)*

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

Root, unchanged in shape: `F → T → X_m → S_0`, built by `establish_auto`
(`clients/libs/rust/src/tesr.rs`) at the FIRST MEMPOOL SIGHTING of `F`. It is the coin's only exit
material; no `tx1` precedes it. Pure-handover coins are untouched.

**Two entry points, and they fail differently — do not state one lane's behaviour as if it were
both.**

* `coin_status::check_deposit` under **`LadderAtSight::Plain`** (mercuryrustlib's `update_coins`)
  ladders inside the deposit pass itself. If `establish_auto` fails there, the deposit is **not
  booked**: the pass clears the outpoint, returns the coin to `INITIALISED`, errors by name and
  retries next pass. A `single_use` coin is the one exception — it gets no ladder on this lane, as
  it got no `tx1`.
* The SDK's `claim()` establish pass under **`LadderAtSight::Defer`** ladders NOTHING inside
  `check_deposit`; the coin IS booked (`IN_MEMPOOL` / `UNCONFIRMED` / `CONFIRMED`) and the SDK's own
  pass then ladders it, plain or coloured. **If that pass cannot ladder the coin, the coin stays
  booked with NO exit material** — the reason is recorded in `ladderskip-<sid>` and surfaced by
  `flat_only_coins`, and it licenses nothing: the coin cannot be conveyed
  (`transfer_sender::execute` refuses it by name) and it has no ladder to walk out on. **Cooperative
  withdrawal is then the only route out.**

That second failure is not hypothetical. `tesr::establish_auto` / `cosign_tier` do not call
`get_statechain_info`, so the `Plain` lane ladders without an attestation pin; the SDK pass DOES call
it, because it needs the coordinator's aggregate to bind against. `TesrParams::attestation_identity`
refuses when there is neither a compiled-in pin nor a configured identity, and
`attestation_identity_const` returns `None` for mainnet AND for every public testnet — only regtest
has a pin. On such a network the SDK pass records `LadderSkipReason::AttestationIdentityUnpinned` and
ladders nothing, so **an SDK wallet's deposit is booked, cannot be conveyed and cannot be
unilaterally exited.** The flat backup used to supply that unilateral exit without any attestation;
it no longer exists. Mainnet has no enclave provisioned at all, so this is a not-yet-deployable
state rather than a live regression — but any sentence of the form "deposits and exits work without
a pin, only receiving does not" is FALSE for the SDK path, which is the only path a wallet user
takes.

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
   rung is dead weight. That missing rung is the 615 sat and the 720 blocks the spine saves per
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
`SplitLane::Colored`, so the 945-sat change floor is live on both and the Freeze-Lemma bound is
attained: a payment adds one transaction to the sender's exit chain, not two.

A tip is not a coin any other builder can take: it has no `tesr-` row, so `in_ladder_pay` cannot load
it, and no `ctesr-` row, so `child_in_ladder_pay` cannot either; and it cannot be handed over whole:
it has no conveyance builder and there is no flat lane to fall through to (2026-09-06), so
`transfer_sender::execute` refuses it by name — at EVERY caller, not only through
`UtexoWallet::transfer`, which is where the refusal used to live — with "is a SPINE TIP … whose
builder is not landed". Its funding output is un-broadcast, and a coin conveyed over it would have
no exit. **The SPINE BATCH is what makes a tip spendable.** `spine_batch_split` builds
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
  two-tier level as one mints a leaf whose exit does not fit the fixed `initlock` window.

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
  `TOKEN_CARRIER_SATS = 22 536` sized for the LEGACY lane (the max of the two; the CTES-R lane needs
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
(`lib/src/tesr.rs`) already roots at an arbitrary outpoint. This cuts 615 plain / 744 coloured **and
720 blocks** per piece, taking the batched asymptote from 1 359 to 744 and the payee's wait to
`i + 2 160`.

The cost is real: `renew_child` / `renew_child_auto` rebuild the leaf's extension and state in place
over the same `SP.out[j]`, taking the leaf's transfer budget from 36 hops to 36 hops × 16 renewals
(`m_max + 1` extension rungs — a CSV budget, not a calendar) per depth level. Dropping the piece's
extension forfeits that. The trade is **615 sat and 720 blocks per piece against 15 further
renewals**, and it is a separate, argued decision.

---

## 8. Verifier and census rules

| # | rule | where | why it is load-bearing |
|---|---|---|---|
| V1 | `ChildSegment` is `{ extension: Option<TesrTier>, state: TesrTier }` | `clients/libs/rust/src/tesr.rs` | a spine segment has one tier |
| V2 | the ancestor expectation `CHILD_V2_BASELINE + 2 + seg_superseded_ok` derives the `2` from the **disclosed tier count** | `clients/libs/rust/src/tesr.rs` | without it every spine bundle is rejected outright; the literal `2` against a one-tier bundle is a free census slot, and that mismatch fails *open* — V1 and V2 are one change |
| V3 | a **SPINE tier kind** with CSV bounds `[0, 0]`, alongside state and extension | `clients/libs/rust/src/tesr.rs`, live and superseded paths | must be a new KIND, never a widened state range — see below |
| V4 | `SpineTipBundle` under `SPINE_TIP_KEY_PREFIX` | `clients/libs/rust/src/tesr.rs` | `withdraw` routes anything keyed `ctesr-` to unilateral exit; the tip must not be mistaken for a leaf |
| V5 | `split_output_floors` → `SplitFloors { piece, change }`, `min_spine_tip_value` = 945 plain / 1 074 coloured, per-leg refusal text | `clients/libs/rust-sdk/src/transfer.rs` | one number can only reach the change floor by lowering the PIECE's floor too, which mints a child that cannot fund its second rung and dies after the parent is terminal |

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
   chain, and the depth cap (§5) would admit a leaf whose real walk is over the cap.
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

**One of those confident wrong answers is now inexpressible, which is worth recording because it is
the only one that got fixed by DELETION rather than by co-editing.** `parent_shape` used to return
`ParentShape::Unladdered` — a POSITIVE verdict reached by three consecutive absences, carrying the
cheaper cost model, the lower floor and a route to the plain split. The variant is gone with the lane:
`parent_shape` now REFUSES, and `parent_shape_opt` is the probe form for the one caller
(`has_exit_material`) where absence is genuinely data. A missed prefix there is still a defect, but its
worst outcome is a named refusal on a healthy coin instead of a laddered coin priced and routed as an
un-laddered one. The other seven sites are unchanged: they still enumerate, and still must be
co-edited.

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
- *spine slot i* — baseline 0 (`CHILD_V2_BASELINE = 0`). At rest 1 live (`C_i`) + 0 superseded = 1.
  After the next batch: 1 live (`SP_{i+1}`) + 1 superseded (`C_i`) = 2. A whole-coin handover of the
  tip adds exactly +1/+1, the arithmetic `child_retransfer` already relies on.
- *piece slot* — `0 + 2 + superseded`.
- *replace-by-lower-timelock* — `X_m.out[0]`: 0 < 144…1440. `SP_i.out[spine]`: 0 < 1440. Leaf:
  untouched.
- *the flat term is zero at every slot, by construction* — `PARENT_V2_BASELINE = 0` and
  `CHILD_V2_BASELINE = 0` (`clients/libs/rust/src/tesr.rs`). No flat backup is co-signed at deposit
  (the first three co-signs on any coin are `T`, `X_0`, `S_0`) or at any hop, so
  `se_num_sigs == tiers + superseded` at the root as at every slot below it. The conveyed
  `parent_flat_backups` vector is always empty and `refuse_conveyed_flat_backups` refuses a non-empty
  one by name; `verify_child_bundle` is called with both baselines. *(RETIRED 2026-09-06: "the census
  trap is respected — `flat_backups` is never 0" and the `parent_backups.len() < PARENT_V2_BASELINE`
  pre-check in `in_ladder_split`: there is no parent chain to read.)*

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

  A leaf arms the EVENT predicate only. `leaf_watch_entry` (`clients/libs/rust-sdk/src/watchtower.rs`)
  exports `deadline_block: u32::MAX` — the height predicate permanently false, exactly as for a
  laddered root — and a trigger on `F` with `csv_blocks = head_start`, where `head_start` is
  `exit_wait_blocks` over the **bound** chain, so a depth-N leaf is charged all N spliced spine
  levels. There is no height to arm: the leaf's parent holds no flat backup, so no ancestor holds a
  matured spend of `F` and the race cannot be lost on its own. What starts it is somebody spending
  `F` — an ancestor's retained `T`, broadcast by a prior owner or a griefer — and from that block the
  leaf's walk must be under way. Every unbuildable entry aborts the export by name rather than being
  dropped from it. *(RETIRED 2026-09-06: `deadline_block = L_k − head_start`, "the parent's lowest
  flat-backup rung — a rung belonging to the splitter". A leaf has no clock, and none belonging to the
  splitter.)*

  `auto_exit_margin_blocks_for(k_max, interval, depth)` (`clients/libs/rust-sdk/src/config.rs`) —
  **2 120 blocks** on mainnet, **860** on regtest, over the `293·d + 375` vB / `3 + 2d`-transaction
  exit model (`exit_cost_scaling_model`, `clients/libs/rust-sdk/src/invalidation_model.rs`) — is
  still derived and still the default `auto_exit_margin_blocks`, but the pass it sizes,
  `auto_exit_due`, has no laddered subject: its leaf near-deadline loop is deleted, and it keys only
  on branch-lane rows a laddered wallet does not have. Leaves are defended by the event-driven child
  loop of `defend_ladders_inner` (`clients/libs/rust-sdk/src/wallet.rs`), admitted by
  `is_live_for_defence` (`IN_MEMPOOL | UNCONFIRMED | CONFIRMED`) from the block the deposit is first
  seen in, and by the exported trigger.

  **The failure mode this guards is silent absence, not a wrong trigger.** `export_watch_bundle` reads
  by key prefix; a split child is a `ctesr-` row and a spine tip a `SPINE_TIP_KEY_PREFIX` row, so a
  reader that looks only for `tesr-` rows finds both reads empty and returns `Ok` while exporting
  nothing for them. Delegating to a third-party tower can then protect the parents and leave the
  children unwatched, and the in-process child loop is the only thing hiding it.
- **Minimum parent value** for a K-batch: `K · floors.piece + floors.change` — and the two legs are
  floored by DIFFERENT rules. `split_output_floors` (`clients/libs/rust-sdk/src/transfer.rs`) is not
  one formula applied twice, and reading it as one overstates what a payee's leg must be worth:
  - `floors.piece` is `SplitLegRole::Tail.min_value` — **1 satoshi** [REQ-83]. A tail funds nothing,
    not a rung, not a backup, not even its own transaction's fee, so the only thing it must clear is
    being a payment. What binds a small leg is the BUILDER rather than admission: at most one
    sub-dust leg per split (REQ-85), refused at the top of `in_ladder_split` and therefore before
    the parent is terminalized.
  - `floors.change` is
    `max(min_split_output(backup_rate), change_leg_role(lane).min_value(committed_rate))` — a
    function of TWO independent rates, the committed rate the tiers are signed at and the backup fee
    rate read from the live network (`min_split_output` = `DUST_LIMIT + ceil(BACKUP_TX_VBYTES ·
    backup_rate)`), combined through a `max`. **No literal is correct across configurations**;
    evaluate it for the rates you are running. *(A parenthetical "K=10 → 15 270; K=20 → 29 230" stood
    here until 2026-09-07 and is deleted: it implies a per-piece floor of 1 396, which no current
    constant produces at any rate, and it contradicted the sentence it was attached to.)*

  Below the minimum, K falls back. Coloured carriers at `TOKEN_CARRIER_SATS = 22 536` are stated
  here as supporting K ≤ 4 and needing to be re-sized at issue; that bound is **not re-derived**
  against the coloured floors above.

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

**The larger cost was finality, and it was the sender's free option — RETIRED 2026-09-06.** This
paragraph used to read: a split child has no flat backup, but the sender keeps one that spends `F`
for **112 vB**, pays them the whole coin, and voids every sub-economic piece they ever paid at zero
marginal cost. **No prior owner holds a matured spend of `F` any more.** The only spends of `F` in
past owners' hands are the retained triggers `T` — no timelock, so the current owner or their watcher
can always pre-empt, and broadcasting one merely STARTS every leaf's walk — and the superseded states,
which lose the CSV race to the live tier the leaf's tower pushes. Neither pays a prior owner a piece
it handed over: after the handover the child aggregate `A_child` is the receiver's and the SE's, and
a confirmed `SP.out[j]` pays that aggregate whether or not the leaf holder ever walks.

What survives is the VALUE bound, and it is a liveness cost rather than a theft option (SPEC.md G4,
L-2): defending a piece costs its walk — `3 + 2d` transactions, `293d + 375` vB — so a piece below
break-even is STRANDED, on chain at its own aggregate and worth less than the fee to sweep it, not
taken. `min_child_value` = 1 560 sat **is** that break-even function evaluated at the SHIPPED
`committed_fee_rate = 3.0` (`TesrParams::mainnet`, `lib/src/tesr.rs`) and at no other rate — this
paragraph said "the hardcoded 2.0" until 2026-09-07, which is a rate the code does not ship; at
20 sat/vB a depth-8 piece admitted at 1 560 costs far more than its face to defend.

**And the protocol admits legs BELOW that break-even on purpose.** REQ-83's bands run down to a
1-satoshi tail (§9), and `LeafShape::exits_unaided` is false for every band under one rung: those
legs have no walk of their own at any fee rate, so they do not become defensible when fees fall.
That is the owner's accepted trade — a small leaf leaves with whoever broadcasts `SP`, not alone —
but it must be read as what it is. The value bound has not been removed; it has been moved onto
somebody else's exit, and a payee holding a sub-rung band with no co-operating group holds satoshis
it cannot put on chain by itself.

A sender who exits pays their own full walk
(`s + 3` transactions, §7.6), not 112 vB, and gains nothing from the pieces it strands. The spine
shrinks the `d` term and shortens the window in which a payee must notice. See TRUST-MODEL.md.

---

## 11. Cost tables

Plain lane, mainnet params, at the SHIPPED committed rate **`r = 3.0`**. "Locked" = committed fee +
P2A in un-broadcast tiers; identical to burned, since it is recoverable only by broadcasting.

| shape | per payment | setup delta | locked after N |
|---|---:|---:|---:|
| two-tier change (no spine) | 2 589 | 0 | 1 845 + 2 589N |
| **spine, K=1** | **1 974** | **0** | 1 845 + 1 974N |
| **spine batch, K=10** | **1 421** | 0 | 1 845 + 1 421N |
| **spine batch, K=20** | **1 390** | 0 | 1 845 + 1 390N |
| spine batch K=20 + lean leaf (§7.7, open) | 775 | 0 | 1 845 + 775N |

*(Re-derived 2026-09-07 from §2.1's units: per payment is
`piece child + (committed_fee_for_outputs(K+1, r) + P2A) / K`, the new cap rung and the superseded
rung it replaces cancelling; K = 10 and K = 20 carry a half satoshi, rounded up. The locked base is
the root ladder's three rungs. This table stood at r = 2.0 — the superseded rate — with rows
2 046 / 1 556 / 1 115 / 1 091 / 601 over a base of 1 470. Ordering and ratios are unchanged; only
the rate moved.)*

Coloured per payment: 3 105 two-tier; **2 361** at K=1; **1 691** at K=10; **1 654** at K=20 — and
repeatable. Coloured locked after N on the two-tier shape is `2 232 + 3 105N`. *(The r = 2.0 row was
2 390 / 1 814 / 1 296 / 1 267 over a base of 1 728.)*

The fee win is bounded: 1 230 of the 1 359-sat asymptote is the **piece child's own two rungs**,
which batching cannot touch and only the lean-leaf variant removes — the asymptote is the two rungs
plus `P2TR_OUT_VBYTES · r` for the payload the batch adds. **The fee is not the reason for this
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

**Killed by: an irreversible one-way commitment.** `in_ladder_split` calls
`set_spend_budget(parent, 1)` and `SP` consumes it, so `sign/first` and `sign/second` return 410 Gone
thereafter (`server/src/endpoints/sign.rs`) and `set_sig_budget` can only tighten
(`server/src/database/deposit.rs`). There is **no second fan-out and no re-denomination**, and
`auto_refresh_before_spend()` (`clients/libs/rust-sdk/src/refresh.rs`) has no subject left, because
after a fan-out every coin is a terminal parent or a `ctesr-` child. *(RETIRED 2026-09-06, the second
kill: "every leaf hop re-runs `validate_backup_chain_v2` against the live tip and fee rate, and a
>5 sat/vB move makes all N leaves un-conveyable". No flat chain travels with a laddered coin, so
nothing on the laddered receive path validates one; a leaf's tiers are priced at the constant
`committed_fee_rate` that `verify_conveyed_child` binds exactly, and the live-rate exposure is at
EXIT — the CPFP top-up of §11 — where it is the same for every shape.)*

Cost columns, for comparison with §11 and on the same re-derived footing (shipped `r = 3.0`):
**0 per payment on-grid**, `E[u/2]` off-grid; setup delta **1 359N − 129, once** *(RETIRED
2026-09-06: "recurring 9.2×/yr" — there is no epoch to recur in; and re-derived 2026-09-07 from
1 066N − 86, which was the r = 2.0 evaluation)*; locked `1 716 + 1 359N`; exit at N = 10 is
5 tx / 1 012 vB / 29.8 days — vB and latency are independent of the committed rate — and at N = 100
the exit is *unreachable* because the tree is terminal.

Compounding it: every leaf sits under the depth and length caps of §5; N ≤ 63 from the derived-token
lifetime cap; and `colored_multi_carrier_transfer` never admits children as legs
(`clients/libs/rust-sdk/src/tokens.rs`), so after a fan-out the wallet reports **"COLOURED carriers
hold 0 in total"** while holding the entire deposit.

Economics, even setting safety aside: with its own recommended binary ladder the ceiling is
**17.3×** — rows above that exceed the N·36 leaf-hop budget, since a leaf survives exactly
`(1440−144)/36 = 36` hops (`child_supersede_csv`). With **no leaf return** it is **1.0×** — identical
to splitting once per payment, which is what an ordinary in-ladder payment already does. *(RETIRED
2026-09-06: "the fan-out recurs 9.2×/year because the tree must fully materialise before
`H_deposit + initlock`" — there is no such height. A fan-out is ONE-SHOT: it burns `1 359N − 129`
once, and the only way to fan out again is full materialisation plus a fresh deposit, forced by
nothing. The 9.72 %/yr and the ~46-payments/year break-even are withdrawn with it.)*

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
rounding residual (`E[b/2]`) **worse than the 2 589-sat two-tier cost** (§2.1).

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

**There is no root epoch — RETIRED 2026-09-06.** This section used to open: *"The ~69-day root epoch
survives untouched. The depositor holds a flat backup maturing at `H_deposit + lockheight_init`."*
The depositor holds no flat backup: `create_tx1` is deleted, the ladder `T → X_m → S_0` is co-signed
at the first mempool sighting of `F` (the enclave count after deposit is 3), and `coin.locktime` is
`None` for life. `initlock`/`interval` survive in `/info/config` and `TesrParams::flat_ladder_params`
(`lib/src/tesr.rs`: `bitcoin`/`testnet`/`signet` = 10 000/100, regtest = 1 000/10) as compatibility
constants — `initlock` is the fixed exit window of §5 and `interval` is applied to nothing. Nothing on
a coin matures on its own: INV-27 ("an idle coin never ages") is unconditional; the deadline passes
`deadline_safety_due` and `auto_refresh_due` (`clients/libs/rust-sdk/src/refresh.rs`) have no laddered
subject, because `coin_near_final` reads a locktime that is never set; and a watch-bundle entry for a
laddered coin or a leaf carries `deadline_block: u32::MAX`. `T` is un-timelocked and spends `F`; the
obligation that used to attach to it — confirm before the earliest live flat-backup locktime — has no
date left to name.

**No on-chain re-anchor is forced by time.** A coin's off-chain life is bounded by HOPS: each
whole-coin hop takes one `δ` off the state rung; `renew` / `renew_auto` (`clients/libs/rust/src/tesr.rs`)
reset it off-chain, up to `m_max` extension rungs; and `rollover` / `rollover_auto` add a level at the
extension floor — all zero on-chain bytes. The on-chain cadence that remains is the cooperative
re-anchor (`refresh()` → `reanchor()`, 1 tx / 112 vB) at the renewal/rollover cap: when a coin has
been HOPPED enough, never because it has been HELD long enough. *(Renewal and rollover are library
calls the transfer path does not yet invoke — renewal is by hand; SPEC.md §0.4 carries the row.)*

**For a split tree the cooperative re-anchor does not exist, and that is a depth limit, not a
clock.** The root is terminal (`set_spend_budget(…,1)` consumed by `SP`), so the SE refuses to co-sign
(`server/src/endpoints/sign.rs`) and `withdraw` has no confirmed outpoint to spend. The only re-anchor
is **full unilateral materialisation followed by a fresh deposit**, which the batched spine makes
affordable (8 txs, ~15 days at N = 100) — but nothing forces it: a parked tree's relative timelocks do
not start counting until somebody broadcasts, and a leaf hopped to its floor is renewed in place
(`renew_child`). **Depth resets only there**, and only when its holder chooses.

What each of the three moves listed here used to buy, re-read without a calendar:

- **Raise `lockheight_init`.** It no longer lengthens anybody's clawback window — there is none. It
  widens the fixed exit window the depth and length caps measure against (§5), i.e. it raises the
  admissible depth; and it is still **not a per-deployment dial**: clients compile in
  `TesrParams::flat_ladder_params(network)` and refuse any coordinator whose `initlock`/`interval`
  disagree, and the coordinator **panics at boot** rather than serve a mismatched pair
  (`server/src/server_config.rs`). Changing it is a protocol change shipped on both sides at once,
  not a compose edit.
- **A co-operative de-trigger for terminal trees** — the SE co-signing a fresh spend of `F` after the
  tree is terminal. Requires raising the spend budget on a terminalized statechain *and* a protocol
  for invalidating every live child with its holder's consent. Hard; **not designed** — and no longer
  needed to beat a date, only to shorten a terminal tree's exit chain.
- **A child re-anchor primitive.** Structurally impossible as posed: a child's funding `SP.out[j]` is
  un-broadcast, so there is no confirmed outpoint to spend, and producing one *is* the on-chain
  transaction you were trying to avoid.

**Also unchanged:**

- **The payee-borne 1 230 sat** (1 488 coloured) per received two-rung piece — the shipped-rate
  figure; this line read 980 / 1 152 / 490 at the superseded 2.0. Only the lean-leaf variant (§7.7,
  open) touches it, halving it to 615. Batching and the spine do not. A payee whose leg is one of
  REQ-83's thinner bands bears less because it *has* less: one rung, or none (§9, §10).
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
  then a co-signed **coloured de-trigger** carrying a valid state transition — two transactions, no
  SE change.

  **The crossed pair has NO remedy, and an allocation caught in it is STRANDED.** Both lanes refuse
  it by name, and neither refusal is a redirection to a working path: `refresh` refuses a COLOURED
  carrier because its plain re-anchor routes through the RGB-unaware `withdraw` and would destroy
  the allocation, and `colored_reanchor` refuses a PLAIN ladder ("use `refresh`") because a coloured
  de-trigger needs coloured material to build from. An RGB allocation that ended up on a plain
  ladder therefore has nowhere to go: `refresh` would burn it, CR-D cannot build for it, and the
  plain tiers already signed over the sealed output cannot be unsigned. It is recorded as
  `LadderSkipReason::PlainLadderOverCarrier` (`clients/libs/rust-sdk/src/events.rs`), whose variant
  documentation now states exactly this — no remedy today, recoverable only if the coin's
  counterparty co-operates off this path. The coin stays exitable as SATOSHIS and must never be
  conveyed as a carrier; the tokens are what is lost. It has no clock to die at, which is the only
  comfort available: nothing forces the loss to a date. *(This bullet said "such a coin must be
  moved off-carrier first" until 2026-09-07, which read as a remedy. There is no in-protocol move —
  `refresh`'s own refusal text advises moving the asset off the coin, but the two paths that would
  do it are the two that refuse. Avoid the state instead: do not move an allocation onto an outpoint
  that is already plain-laddered, which under laddering-at-first-sight is every confirmed plain
  deposit. The `FLAT_PLAIN_LADDER_OVER_CARRIER` doc-comment in
  `clients/libs/rust/src/transfer_sender.rs` still names the coloured re-anchor as the remedy and is
  stale.)*
- **The 36-hop CSV budget, now renewable.** A child survives `(1440−144)/36 = 36` whole-coin handovers
  per renewal cycle (`child_supersede_csv`), and `renew_child_auto` steps the extension one rung down
  and resets the state to `state_csv(0)`, so the budget is **36 hops × 16 renewals** (`m_max + 1`
  extension rungs — a CSV budget spent by hops, not a calendar) per depth level. A leaf that has itself made a partial payment is TERMINAL at the SE and cannot renew —
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
| 4 | The discharge round (§4, SPEC.md §5.4) | — | **RETIRED** — SPEC.md §5.4.0 (owner decision 2026-08-20); and since 2026-09-06 it has no cadence, because nothing on a coin matures (§4 banner) |
| 5 | Opt-in self-carve inventory (§12.3) for fixed-amount books | utilisation gate `> 0.685K + 0.315` enforced in the planner | **OPEN** |
| 6 | Lean leaf (§7.7) — separate, argued decision | forfeits child renewal, which exists (`renew_child`) | **OPEN** |
| 7 | A v3/TRUC tier above 10 000 vB never relays (§5) | none | **OPEN finding**, unaddressed |
| 8 | The `lightning_latch` holes (§12.2 items 2 and 3) | none | **OPEN** — no swap primitive can be exposed safely until they are closed |
| 9 | A forcing condition for settling absorbed leaves (§3.4): the forced arm lost its trigger with the calendar, and `should_settle`'s `earliest_runway_blocks` / `may_absorb`'s `runway_blocks` (`lib/src/sweep.rs`) have no live source | none — the voluntary path and the event duty stand alone | **OPEN** since 2026-09-06 |
| 10 | An RGB allocation on a PLAIN ladder (§13): `refresh` would burn it, `colored_reanchor` refuses it, the plain tiers cannot be unsigned | none | **OPEN, and it is a LOSS not a delay** — the allocation is stranded; only the satoshis exit |
| 11 | The per-leg admission floor is 1 satoshi (§9) while `LeafShape::exits_unaided` is false below one rung (§10) | none | **OPEN.** The trade is the owner's, but `exits_unaided` has **no production caller** — it is reached only from a unit test in `lib/src/tesr.rs` — so nothing on the payment path discloses it |

Related: SPEC.md (normative constants and the round), PROTOCOL.md (the tier machine),
TRUST-MODEL.md (the finality option and the residual trust surface), CHILDREN.md (child lifecycle,
renewal and conveyance), LIGHTNING.md, README.md.
