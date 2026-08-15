# Partial-payment economics — what a real payment costs, and what to build

> ⚠️ **[D53] CORRECTION, 2026-08-14 — the depth cap in this document is STALE.**
> Every statement here of `max_split_depth = 10` / **23 transactions** (mainnet) or
> `max_split_depth = 68` / **139 transactions** (regtest) was measured against the BARE latency rule
> `exit_wait_blocks <= epoch`. The rule a conveyed child is actually ADMITTED by adds
> `exit_slack_margin`, so the shipped caps are **depth 8 / 19 transactions** (mainnet) and
> **depth 54 / 111 transactions** (regtest). Depths 9 and 10 were unadoptable at every tip. The
> 9 383-block / 65.2-day figure remains correct as the WAIT of a depth-10 walk — it is not the cap.
> Read `DECISIONS.md` D53 before carrying any depth figure from this document into the spec.


> **Status: PARTLY SHIPPED.** Numbers are derived from constants read at `feat/spark` and cited
> `file:line`; §3.0 lists defects that existed independent of which design shipped, and **all six
> are now closed at HEAD** — each row carries the symbol that closed it. **The "nothing in §4 exists
> in code yet" this line used to carry is false** — see the build-status block at the head of §4,
> which is authoritative: change 1 (the spine tier) is landed and change 2 is landed in both halves.
>
> **§1 and §2 are the PRE-CATS baseline and are kept as the "before" half of §6's comparison.** The
> live builders sign `SP` at `SPINE_CSV = 0` and give the plain root's change leg a one-rung spine
> tip, so every 2 124-blocks-per-level figure below describes the design that was measured, not the
> one that ships. Each such block is labelled where it appears.
>
> **Read the ⚠️ CORRECTION inside §4.5 before implementing anything from §4.5** — it voids that
> section's stated safety argument, and the code now carries an executable disproof of it.

---

## 0. THE CENTRAL RESULT — this design sells payment VELOCITY, not payment granularity

**Everything else in this document is a detail of this one claim, and the claim has a side on which
we LOSE. State both sides or the number is marketing.**

Measured against the honest alternative — not against a strawman.

### 0.1 The comparison people get wrong

A one-to-many payout on Bitcoin is **ONE transaction with N+1 outputs**, not N transactions. Paying
100 recipients on chain costs **4 411 vB — about 44 vB per recipient.** Any comparison that prices
the on-chain alternative at "N × 155 vB" is inventing an opponent that does not exist, and every
favourable ratio derived that way is worthless.

### 0.2 The result — CORRECTED BY [D80], read this and not the first version

**The sweep is NOT free, and my first table treated it as if it were.** For an SSP to hold 90 % of a
tree, 90 payees must each have TRANSFERRED their leaf to it — and a transfer IS an onward hop. Putting
a swept figure in the `h = 0` row gave Utexo a sweep for nothing while charging the on-chain column
for none of the hops that produced it. Sweep fraction `s` and hop count `h` are COUPLED: `h ≥ s`.

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

**The crossover is at ~74 % sweep coverage, not 0.53 hops, and the ceiling is 1.87× — not the order
of magnitude the first version implied.** Below ~74 % coverage a batched on-chain payout is simply
better, and at zero coverage it is better by 6.8×.

**What survives, and it is the part that matters commercially:** the PER-LEAF value recovery in
§0.5/§0.7 is unaffected — it is a satoshi quantity, independent of K and of the aggregate block-space
story. The SSP's ~1 057-sat margin at 3 sat/vB stands.

### 0.3 WHO THE USER IS — the leaf lane is the normal one, and it is the one that loses

**Do not quote the whole-coin economics as representative.** Payments are arbitrary amounts, so every
non-exact payment is an in-ladder split and the payee receives a CHILD. A ROOT holder is the
DEPOSITOR, or the rare payee of an exact-amount transfer. **After the first payment, everyone
downstream is on the leaf lane.**

| who | block space per payment | against ~154 vB on chain |
|---|---:|---|
| **leaf, spent onward off-chain** | **0** | this is the product |
| **leaf, swept** (§0.7) | 58 vB | 0.38× |
| **leaf, WALKED out — the shipped default** | **250 vB** | **1.62× WORSE** |
| root holder — depositor only | ~589 vB/yr ⇒ 5.9 at 100 payments/yr | 26× |

The root row is the most flattering number in this document and it describes almost nobody once
payments start flowing. **For the population that actually exists, the shipped default settles a
payment for MORE block space than doing it on chain** — 250 vB against 154. That is the sentence to
lead with, and §0.7's sweep is what changes it: not an optimisation on a winning position, but the
precondition for the median user's economics being positive at all ([D81]).

**Why the distribution is not what we save** — a batched on-chain payout is nearly free too
(~44 vB/recipient). What we sell is every payment AFTER the first: on chain another ~154 vB each, off
chain zero. So the saving is a function of how many times value MOVES before it settles, and the
design rule follows: **a piece received and immediately cashed out should never have been an
off-chain split.**

### 0.4 What the sweep is for — it protects the result, it does not create it

Without a sweep, settlement is 29 800 vB and NOTHING makes the lane win — an on-chain batched payout
is better at every hop count, because the walks dominate. With 90 % swept it is 12 551 vB and the lane
wins 1.5×. **The sweep is not an optimisation on a winning position; it is what creates the winning
position at all.** It is not a nicety for rescuing dust; it is what keeps §0.2's
result true at realistic velocities.

### 0.5 The satoshi side, which is a different quantity and a bigger number

Block space and VALUE are not the same saving, and the value one is larger.

Every pre-signed tier permanently burns `committed_fee(3.0) + P2A_VALUE` = **615 sat**, carved out of
the coin at split time. A leaf's own two tiers burn **1 230 sat** — and `min_child_value` = **1 560**
is DEFINED as exactly that plus dust, so a minimum-sized leaf walked out realises precisely the
330-sat dust limit. A combine spends `SP.out[j]` directly and never broadcasts those tiers, so the
1 230 sat is never burned:

| leaf face | walked out | via combine @ 3 sat/vB |
|---:|---:|---:|
| 1 560 | 330 — **21 %** | 1 387 — **89 %** |
| 5 000 | 3 770 — 75 % | 4 827 — 97 % |
| 20 000 | 18 770 — 94 % | 19 827 — 99 % |

**The SSP's margin is therefore `1 230 − 57.75 × market` sat per leaf** — about 1 057 sat at
3 sat/vB, 653 at 10, and **zero at 21.3 sat/vB**, above which the prepaid committed rate is the
better deal and holders should simply walk. It is an INVERSE-fee-market business: it earns most when
fees are low, and should stop buying when they are high.

Note what this makes irrelevant: batching moves the per-leaf cost 112 → 58 vB, worth ~160 sat at
3 sat/vB against a ~1 057-sat margin. **Skipping the burn is ~96 % of the value; consolidation is a
rounding error.** An SSP profits on a SINGLE leaf and needs neither whole trees nor majority
ownership — `SP`'s outputs are independent UTXOs, so a sweep of 9 of 10 leaves captures 99 % of the
available saving and the holdout is simply untouched.

### 0.6 Provenance, because this section has been wrong three times

The figures above are the SECOND derivation. The first was wrong in three ways, each caught by an
independent agent reading the code rather than the docs: the shared prefix is `T + X_m + SP` =
`375 + 43K` vB and NOT a flat 375 (`tesr_exit_vbytes`'s `3 × TIER` counts the leaf's own final state,
which is private); the on-chain baseline is 112 vB (1-in-1-out) or 155 (1-in-2-out) at
`INPUT_WITNESS_BYTES = 67`, not the pre-[D4] 111; and the sweep marginal is unreachable for a payment
tree whose leaves have different owners, which is exactly why acquisition — not batching — is the
mechanism that matters. **Do not re-derive these from prose. Re-derive them from
`lib/src/tesr.rs`, `lib/src/transaction.rs::sweep_tx_vsize` and `clients/libs/rust-sdk/src/config.rs`.**

### 0.7 THE SWEEP, AS A MECHANISM — when to absorb a leaf, and when to settle it

The sweep is not a rescue service bolted on the side; it is the thing that keeps §0.2 true at
realistic velocities. This section specifies WHEN it fires. Normative form in `SPEC.md` §5.3.

#### 0.7.1 The one structural fact everything follows from

**The surplus is INDEPENDENT of the leaf's value.**

```
surplus(m) = BURN − combine_marginal(m) = 1 230 − 57.75·m   sat per leaf
```

`BURN` is what a leaf's own two pre-signed tiers destroy (2 × 615). It does not scale with face.
Neither does the combine input. So the SSP earns **the same ~1 057 sat at 3 sat/vB** whether the leaf
holds 1 560 sat or 1 BTC.

Three consequences, and they are not intuitive:

* **Small leaves are the BEST business, not the worst.** Same absolute surplus, far less capital at
  risk. At the admission floor the surplus is 68 % of the leaf's entire value; at 100 000 sat it is
  1 %.
* **There is a natural VALUE CEILING.** Above some face the SSP is taking balance-sheet risk for a
  return that has stopped growing. The ceiling is a risk-appetite parameter, not an economic one.
* **Batching is nearly irrelevant.** Going 1 → 10 leaves moves the marginal 112 → 63 vB, worth ~150
  sat against a ~1 057-sat surplus. **Absorption is the business; consolidation is a 4 % optimisation.**
  The SSP therefore needs no whole trees, no majority ownership, and no coordination with holdouts.

#### 0.7.2 WHEN to absorb — at claim, inside the payment flow

The swap belongs in `claim()`, at the moment a payee first sees the leaf. That timing is optimal on
every axis at once: runway is maximal (the inherited deadline is furthest away), the user is online
because they are already transacting, and no separate coordination round is needed. The payee receives
an ordinary root coin and never handles a leaf.

**And a root is strictly better for the payee than the leaf it replaces**, independent of any spread:
no inherited deadline, depth 0, a one-transaction cooperative exit, and no watchtower duty tied to a
parent it does not control. That is what makes a silent swap defensible rather than extractive — but
see the fairness condition in §0.7.5.

#### 0.7.3 The absorption predicate

Absorb a leaf iff ALL hold:

| condition | default | derivation |
|---|---|---|
| `market_fee_rate ≤ sweep_max_fee_rate` | **15 sat/vB** | surplus hits zero at 21.3; 15 keeps a ~30 % margin (369 sat/leaf) |
| `runway_blocks ≥ sweep_min_runway` | **903 blocks** | `e_csv(720) + confirmations(3)` = 723, +25 % safety. Below this the leaf CANNOT be settled and absorbing it buys a liability |
| `leaf_value ≤ sweep_max_leaf_value` | **100 000 sat** | the value at which a constant ~1 057-sat surplus falls below 1 % of face — past it the SSP adds risk without adding return |
| `tree_exposure + leaf_value ≤ sweep_max_tree_exposure` | **1 000 000 sat** | `target_batch × max_leaf_value`; bounds loss if one tree's spine cannot be materialised |

#### 0.7.4 WHEN to settle — the SSP holds an option, and should price it as one

Having absorbed, the SSP is not obliged to settle promptly. It holds a **timing option**: settle at
the cheapest fee window inside the runway. Exercise when EITHER:

* `batch_size ≥ sweep_target_batch` **and** `market ≤ sweep_max_fee_rate` — the voluntary path; or
* `earliest_deadline − tip ≤ sweep_min_runway` — the **forced** path, and it is unconditional. A leaf
  that misses its inherited deadline is voided by the parent's flat backup and the loss is the whole
  face, not the spread.

`sweep_target_batch = 10` captures 94 % of the achievable batching gain; beyond it the curve is flat
and waiting only adds fee-market and deadline exposure.

**The risk is asymmetric and must be stated that way.** The downside of settling too EARLY is a few
hundred sat of foregone batching. The downside of settling too LATE is total loss of the leaf. Every
default above is therefore biased toward acting early, and the forced path ignores the fee ceiling
entirely — an expensive settlement beats a voided one at every rate.

#### 0.7.5 The fairness condition, stated because "silently" invites the opposite

A silent swap must leave the payee **no worse off than holding the leaf**, measured against the
leaf's own realisable value:

```
price_paid ≥ leaf_value − BURN          (what the payee would realise walking it out)
```

At the floor that means paying at least 330 sat for a 1 560-sat leaf — while the SSP realises 1 387.
There is ~1 057 sat of surplus to divide, and the split is `sweep_spread_bps`, a policy parameter and
not a protocol constant. Two obligations follow: the payee is handed a coin that is **strictly better
in kind** (root, no inherited deadline), and the spread is disclosed in aggregate rather than being
the mechanism's hidden purpose.

**Do not let the spread exceed the surplus.** A swap priced below `leaf_value − BURN` takes value from
a payee who would have done better walking, which is the one outcome that turns this from a service
into a tax.

#### 0.7.6 Game plan — build order, cheapest evidence first

| # | step | why it is first | evidence |
|---|---|---|---|
| **S1** | prove `spine + 1` cooperative child exit end to end ([D77], currently UNVERIFIED) | every number in §0.7 rests on it; if a confirmed `SP.out[j]` cannot be cooperatively spent, the whole design collapses to the 250-vB walk | an E2E: split, materialise spine, mine to `confirmation_target`, cooperative withdraw, assert ONE transaction |
| **S2** | wire `combine_leaves` to a caller — it has **zero** outside a test today | the primitive exists and is unreachable; nothing else can be measured until it is | an E2E consolidating k ≥ 2 leaves of one `SP` |
| **S3** | the absorption predicate as a PURE function + its parameters | testable without a stack, and it is where a wrong sign silently becomes a policy | unit tests per row of §0.7.3, both directions |
| **S4** | the swap in `claim()`, behind a default-OFF flag | the payment-flow half; default-off so it ships before it is trusted | an E2E: payee claims, receives a ROOT, SSP holds the leaf |
| **S5** | the settlement scheduler (voluntary + forced paths) | needs S1–S4; the forced path is the one that must never be skipped | a test that the forced path fires **regardless** of the fee ceiling |
| **S6** | publish the realised curve from a live fleet | §0.2's break-even is modelled, not measured | measured hops-per-leaf and settlement cost against the model |

**S1 is the gate.** It is a day of work and it decides whether this is a 4 %-margin batching play or a
~1 057-sat-per-leaf value-recovery business.

---

### 0.8 THE DISCHARGE ROUND — the footprint scales with PIECES, not with PAYMENTS

> Costs a design that does not exist (SPEC §5.4). The enforcement point is empty: `disclosure` /
> `prevout_value` occur 83× in the client and **0× in `lockbox/`**. These are the numbers it *would*
> cost built, not numbers anything measures.

#### The structural result

A round re-mints **every outstanding leaf** regardless of how many payments produced it, and retires
the old tree in **one transaction**. So the on-chain footprint is set by

```text
    footprint  =  (outstanding pieces ÷ 256) × (365 ÷ epoch_days) × (155 + 43·absentees)
                   └── tree count ──┘          └── rounds/year ──┘   └── one collapse tx ──┘
```

and **payment volume appears nowhere in it.** 256 is a hard cap, not an assumption: a depth-8 tree has
`2⁸` leaf slots. Migration consumes the successor tree's slots, so tree count tracks *pieces held*,
never *payments made*.

This is the whole point, and it is the reversal of the pre-round position: without the round, cost
scaled with **payments** (and lost); with it, cost scales with **held pieces** (and wins).

#### Worked: 1 M users, 4 000 BTC TVL, 1 M payments/month

12 M payments/yr. Bitcoin supplies 52.56 GvB/yr. On-chain baseline: 1.85 GvB/yr = **3.52 % of the
entire chain**.

| pieces/user | trees | collapses/yr | 0 % absent | 10 % | 50 % | 100 % |
|---:|---:|---:|---:|---:|---:|---:|
| 1 | 3 907 | 20 535 | 0.27 vB | 2.15 | 9.68 | 19.10 |
| **3** | 11 719 | 61 595 | 0.80 | **6.45** | 29.05 | 57.30 |
| 10 | 39 063 | 205 315 | 2.65 | 21.49 | 96.82 | 190.99 |

*(vB per payment.)* Central case — 3 pieces/user, 10 % absent — is **77.4 MvB/yr = 0.147 % of Bitcoin
block space**, about **77 blocks a year**, 24× better than an on-chain payment and 65× better than
today's shipped 418 vB. The worst cell (10 pieces, nobody ever online) is 4.36 % of the chain and
**still beats the shipped default**.

#### Payment volume rides free

The same 77.4 MvB carries any of these:

| traffic | vB/payment | footprint |
|---|---:|---|
| 1 M/month | 6.45 | 0.147 % of chain |
| 10 M/month | 0.65 | **unchanged** |
| 1 B/year | 0.077 | **unchanged** |

#### The two levers

1. **Absentee rate — dominant.** 0.80 → 57.30 vB/payment is a **72× swing**, and it is a product
   problem (how often wallets check in), not a protocol one.
2. **Epoch length — linear.** `initlock = 10 000` sets 5.26 rounds/yr; raising it cuts the total
   proportionally. Depth is a *usability* dial rather than a safety limit, so there is real room —
   gated on reconciling depth admission against materialisability (SPEC §5.4.5 REQ-63.4).

**Quote the worst case, not the best** (SPEC §5.4.6): exit-key reassignment lets any holder force a
payout instead of a migration, free and unattributable.

---

## 1. The correction, stated plainly

Utexo has been described — in [README.md](README.md), in PARITY (retired 2026-08-15), in the pitch — as
transacting **"off-chain, instantly, at no per-payment on-chain cost — any amount"**. That sentence
is true only for an **exact-subset handover**: a payment whose amount happens to equal a coin the
sender already holds, moved whole by replacing its state tier over the same outpoint. That case is
genuinely free — `child_retransfer` (`clients/libs/rust/src/tesr.rs:2653`) builds a replacement
state over the *same* `ext_child.out[0]`, spends zero sats, adds zero depth, and never touches
`ancestors` (the only writer is `child_in_ladder_split`, `clients/libs/rust/src/tesr.rs:2611-2612`).

**In the real world essentially every payment is partial.** Like a Bitcoin UTXO, you send an amount
and take change, ~99% of the time. That path is the **in-ladder split**: a state tier `SP` over
`X_m.out[0]` carving a piece child and a change child, each funded with its own headless ladder by
`establish_child` (`clients/libs/rust/src/tesr.rs:1418`).

It is not free, and the cost was being counted wrong.

### 1.1 The real per-payment ledger

The commonly-quoted figure — 576 sat plain / 662 coloured — counts **only the `SP` tier**. It omits
the two brand-new children the split creates, each of which gets its own extension **and** state
rung. Measured as loss of total exitable value across the tree:

| component | plain | coloured | source |
|---|---:|---:|---|
| `SP` / `CSP` split tier (2 payloads) | 576 | 662 | `lib/src/tesr.rs:405,412,41`; `clients/libs/rust/src/rgb.rs:946` |
| piece child — extension + state rung | 980 | 1 152 | `clients/libs/rust/src/tesr.rs:1418` |
| change child — extension + state rung | 980 | 1 152 | same |
| less the superseded state rung `SP` replaces | −490 | −576 | `clients/libs/rust/src/tesr.rs:2100` |
| **system total, per partial payment** | **2 046** | **2 390** | |

Derivation of the units, all re-checked against the tree:

```
committed_fee(r)                = ceil(125·r)                     lib/src/tesr.rs:73,83   -> 250 @ r=2
committed_fee_for_outputs(n,r)  = ceil((125 + 43(n−1))·r)         lib/src/tesr.rs:400,405
colored_committed_fee(n,r)      = ceil((168 + 43(n−1))·r)         clients/libs/rust/src/rgb.rs:917,946
P2A_VALUE                       = 240                             lib/src/tesr.rs:41
rung  = committed_fee + P2A     = 490 plain / 576 coloured
min_child_value(2.0, 330)       = 2·490 + 330 = 1 310             lib/src/tesr.rs:105
colored_child_floor(2.0, 330)   = 2·576 + 330 = 1 482             clients/libs/rust/src/tesr.rs:1470
mainnet params                  = { d0 1440, δ 36, d_floor 144, e0 720, δE 36, e_floor 144, m_max 15, rate 2.0 }
                                                                  lib/src/tesr.rs:151
```

The toll is **flat and amount-independent**: 2 046 sat is 20.5% of a 10 000-sat payment, 2.0% of
100 000, and ~$2.05 at $100k/BTC. Who pays: the sender loses 1 066 (its own change child plus the
split tier), the payee loses 980 off the nominal — a 10 000-sat piece is worth 9 020 on unilateral
exit.

> Doc fix: [README.md](README.md) quotes `min_child_value` as "1306 sat at 2 sat/vB". The code gives
> **1 310** (`2·(250+240)+330`). Stale by 4 sat.

### 1.2 The cost that actually matters is not the sats

Two structural properties are worse than the fee:

**Depth grows by +1 per payment and never resets.** `child_in_ladder_split`
(`clients/libs/rust/src/tesr.rs`) pushes the entire preceding segment into `ancestors`. When this
was written there was no cap anywhere; **there are two now** — `max_split_depth` and `max_exit_txs`
(`lib/src/transfer/receiver.rs`), enforced build-side by `enforce_split_depth_cap_shaped`
(`clients/libs/rust/src/tesr.rs`) and receive-side by `check_exit_headroom` — and on mainnet they
admit **depth 10 and a 23-transaction exit chain**. What still does not happen is a **reset**: a
child cannot be re-anchored, because `refresh()` routes a `ctesr-` coin through `withdraw()` to
`unilateral_exit` — `SP.out[j]` is un-broadcast, so there is no confirmed outpoint to co-operatively
spend. What did land is **renewal, not re-anchor**: `renew_child` / `renew_child_auto` rebuild both
leaf tiers in place over the same `SP.out[j]`, resetting the state rung to `state_csv(0)` = 1440
"for zero on-chain bytes and no depth" (`child_supersede_csv`'s own refusal text). The coloured lane
is no longer a dead end either: `refresh` still refuses a carrier, but it now names
`colored_reanchor` (CR-D, `clients/libs/rust-sdk/src/refresh.rs`) instead of leaving no route.

**BIP-68 relative timelocks are sequential**, so exit latency compounds:

```
WAIT(d) = 2124·d + 2160 blocks        [T 0 | X_m 720 | SP 1404 | (d−1)×(720+1404) | ext 720 | state 1440]
```

> Two corrections to that line, both from the code that now implements the rule.
> **(a) It omits one block per transaction.** A tier's relative lock only starts counting once its
> parent confirms, so the real figure is `Σ csv + one confirmation each` — `exit_wait_blocks`
> (`lib/src/transfer/receiver.rs`), whose doc comment names this formula and this section as the
> thing it corrects. Add `3 + 2d`.
> **(b) The `SP 1404` term is the pre-CATS shape.** The live builders sign `SP` at `SPINE_CSV = 0`
> (`in_ladder_split`, `child_in_ladder_split`, `spine_batch_split`), so a two-tier level costs
> `720 + 0 + 2 = 722` blocks and a depth-1 leaf's whole walk is **2 885** blocks. Quote both ends of
> that comparison under the SAME convention or the difference is off by five: pre-CATS depth 1 is
> **4 284** blocks of relative timelock and **4 289** once (a) is applied (`lib/src/transfer/receiver.rs`
> writes it as `4 284 + 5`); post-CATS it is **2 880** and **2 885**. The `2124·d` figure below is
> retained as the measured baseline, not as a live number.

d = 1 → 4 284 blocks (29.8 days). d = 100 → 214 560 blocks = **4.08 years**. And it is
**contagious**: the piece child inherits the identical ancestor chain, so the *recipient* of the
100th payment receives a coin costing 203 txs and 4.08 years to exit unilaterally. The sender's
payment history becomes the payee's exit latency. Survival that takes four years does not satisfy
"unilateral exit must survive at every hop, for every holder" — and no adversary is required; the
wallet inflicts it on itself and its payees by spending normally.

### 1.3 The coloured lane is capped at ONE partial payment, ever

`child_in_ladder_split`'s first statement is `refuse_uncolored_over_colored_child`
(`clients/libs/rust/src/tesr.rs:2515`, guard at `:348`); `colored_child_txids` refuses any child with
non-empty `ancestors` (`:243-256`); `colored_in_ladder_pay` only loads a ROOT `tesr-` bundle
(`clients/libs/rust-sdk/src/tokens.rs:3388-3397`). After one coloured partial payment the RGB change
is a depth-1 coloured child that can be moved only **whole** or exited.

> **Both halves of this section have moved.** The one-payment cap **no longer holds** — see the
> build-status block at the head of §4: the refusals that enforced it are gone, `colored_child_txids`
> and `colored_child_seals` now walk `ancestors` N deep [S5], and the coloured root split gives its
> change leg a one-rung spine tip [S3]. And the stale constant is gone with it: the global
> `CARRIER_SEND_DEPTH = 5` was replaced by two named lanes — `LEGACY_CARRIER_SEND_DEPTH = 5` and
> `CTESR_CARRIER_SEND_DEPTH = 1` (`clients/libs/rust-sdk/src/tokens.rs`) — with
> `TOKEN_CARRIER_SATS = 17 384` documented as the LEGACY lane's sizing (the max of the two;
> the CTES-R lane needs 6 362), so the number is now a stated over-provision rather than a
> mis-stated assumption.

---

## 2. The baseline cost curve

> **Labelled superseded baseline — kept, not corrected.** These rows are the design as measured
> before CATS landed, and they are the "before" half of §6's comparison; the *sats* and *vbytes*
> columns still hold at HEAD, two other columns do not. (i) **Latency**: `SP` is signed at
> `SPINE_CSV = 0` now, so a two-tier level costs 722 blocks rather than 2 124 and a depth-1 leaf
> waits 2 885. (ii) **Reach**: the N = 100 and N = 1000 rows are no longer buildable at all —
> `enforce_split_depth_cap_shaped` refuses past **depth 10** and past a **23-transaction** exit
> chain on mainnet (`max_split_depth` / `max_exit_txs`, 68 / 139 on regtest). They are the cost of a
> tree the protocol now declines to mint, which is the point they were written to make.

Loss of total exitable value, locked reserve, depth and exit cost after N partial payments. Reserve
equals burn: every sat is committed fee + P2A parked in an un-broadcast tier, recoverable only by
broadcasting it.

```
BURNED(N)  = LOCKED(N) = 1 470 + 2 046·N sat        (plain; 1 728 + 2 390·N coloured, N ≤ 1)
DEPTH(N)   = N
EXIT(N)    = 3 + 2N txs, 293N + 375 vB, 2 124N + 2 160 blocks
```

| N | burned (sat) | of which sender-borne | depth | exit txs | exit vB | exit wait |
|---:|---:|---:|---:|---:|---:|---|
| 10 | 21 930 | 12 130 | 10 | 23 | 3 305 | 23 400 blk = **162 days** |
| 100 | 206 070 | 108 070 | 100 | 203 | 29 675 | 214 560 blk = **4.08 years** |
| 1000 | 2 047 470 | 1 067 470 | 1000 | 2 003 | 293 375 | 2 126 160 blk = **40.4 years** |

**The tail.** Splittability requires the change to fund two floored children:
`c ≥ piece + 2 376`, absolute minimum `c ≥ 2·1 310 + 1 066 = 3 686` plain (4 202 coloured). Dead
zone `1 310 ≤ c < 3 686`: the change is still exitable but can never make another partial payment.
Worked, V = 1 000 000 at 10 000/payment: `c_N = 999 510 − 11 066N`, so **payment 91 is refused**.
900 000 nominal delivered, 811 800 exitable, **185 610 sat (18.56% of the deposit) burned**, and the
survivor is a 3 570-sat depth-90 coin needing 183 txs and 3.68 years to exit.

**The exit fee is not prepaid.** `committed_fee_rate` is a hardcoded 2.0 (`lib/src/tesr.rs:151`), not
the live rate. Above ~5 sat/vB every tier must be CPFP'd through its P2A, and TRUC's
one-unconfirmed-child rule plus the sequential CSVs forbid batching those children. Net top-up from
an **external** funded wallet, N = 100 payments, per exiting holder:

| live rate | baseline top-up |
|---:|---:|
| 5 sat/vB | 194 585 sat |
| 20 sat/vB | **1 102 550 sat** |
| 50 sat/vB | 2 918 480 sat |

At 20 sat/vB the baseline's exit costs **more than the entire 1 000 000-sat deposit**. That is a
total loss, not a slow one, and it is the dominant term — an order of magnitude above the
2 046-sat/payment fee everyone quotes.

---

## 3. Recommendation

### 3.0 Ship these first — defects under every design (all six now CLOSED)

None of these depend on which design wins. Two were exploitable when this was written.

> **STATUS AT HEAD: all six are CLOSED.** The table is kept as the record of what was wrong and why
> — the fixes are named against it, and each names the symbol that closes it, not a line number.
>
> | # | closed by |
> |---|---|
> | P0-1 | `check_exit_headroom` (`lib/src/transfer/receiver.rs`), called from the conveyed-child verifier in `clients/libs/rust/src/tesr.rs`; every input receiver-derived (CSVs off the signed `nSequence`, epoch off the validated flat chain). E2E `clients/tests/rust/src/sdk82_exit_headroom_gate.rs`. |
> | P0-2 | `max_split_depth` + the build-side `enforce_split_depth_cap_shaped`, derived from the live schedule and the SE's live `initlock` — mainnet **depth 10**. Its companion `max_exit_txs` adds the length cap the latency rule cannot see (**23 txs** mainnet), because a spine tier costs one block and a whole transaction. |
> | P0-3 | `SplitJournalRecord` under `splitjrnl-` + `resume_in_ladder_split`: the whole plan is durable **before** the parent's budget is consumed, and each tier's signature is journalled the instant it exists. |
> | P0-4 | `quote_transfer` now runs the executor's own planner and preflight ([B2]) — `fee_sats` and the per-leg `SplitFloors` come from the same `split_preflight` the executor obeys, so `fundable` is what the executor will do rather than an estimate. `split_fee_reserve`'s clamp survives only on the un-laddered plain-split lane. |
> | P0-5 | `auto_exit_margin_blocks` is DERIVED — `auto_exit_margin_blocks_for(k_max, interval, depth)` = **2 120** mainnet / **860** regtest — and the exit model is now `293·d + 375` vB over `3 + 2d` sequential transactions (`exit_cost_scaling_model`, `clients/libs/rust-sdk/src/invalidation_model.rs`), with the wait taken per-coin from the coin's own chain. |
> | P0-6 | The global `CARRIER_SEND_DEPTH` is gone; both lanes are named and sized (§1.3). |

| # | defect | where | consequence |
|---|---|---|---|
| P0-1 | **No exit-headroom admission gate.** The only bound on a conveyed child is `lock_time > tip` | `lib/src/transfer/receiver.rs:537-539`, called from `clients/libs/rust/src/tesr.rs:2418-2440` | A depth-1 child needs 4 284 blocks to exit; `lockheight_init` is 10 000 (`server/src/server_config.rs:82`). For the **last 4 284 blocks of every epoch (43%)** a sender can hand a payee a coin that provably cannot be materialised before the sender's own flat backup can spend `F` and void the whole tree. Census balances, Model A holds, coin is worthless. |
| P0-2 | **No depth cap.** `child_in_ladder_split` has no `MAX_DEPTH` | `clients/libs/rust/src/tesr.rs:2508` | At d ≥ 4, `WAIT = 10 656 > 10 000`-block epoch — such a coin can never be safely parked between epochs. |
| P0-3 | **`in_ladder_split` persists nothing.** `set_spend_budget(…,1)` + `cosign_tier(SP)` run at `:2093-2095`; the `establish_child` loop runs after; no write to disk anywhere in the function | `clients/libs/rust/src/tesr.rs:2005-2119`; SDK persists only on `Ok`, `clients/libs/rust-sdk/src/transfer.rs:1342-1358` | A failure mid-loop returns `Err` with the parent **terminalized server-side** and zero bundles on disk. The signatures can never be regenerated. Whole coin exit-only, forever. |
| P0-4 | **The fee quote is disconnected.** `quote_transfer` uses `split_fee_reserve = clamp(parent/100, 300, 2000)` | `clients/libs/rust-sdk/src/transfer.rs:327-330, 1926-1929` | Quotes 300 sat against a real 2 046 on a 10 000-sat parent — a 6.8× under-quote. It also plans with a 554-sat floor while the executor enforces 1 310 (`transfer.rs:1104-1112`), so `fundable: true` is followed by a refusal. |
| P0-5 | **The exit model is wrong.** `exit_cost_scaling_model` = 155d + 112 vB with **zero wait**; `auto_exit_margin_blocks` default 288 | `clients/libs/rust-sdk/src/invalidation_model.rs:284-333`; `clients/libs/rust-sdk/src/config.rs:153` | Understates a depth-N exit by ~1.9× in vB and by 100% in latency. The watchtower fires ~15× too late for a coin needing 4 284 blocks. |
| P0-6 | **Stale coloured constants.** `CARRIER_SEND_DEPTH = 5`, `TOKEN_CARRIER_SATS = 17 384` | `clients/libs/rust-sdk/src/tokens.rs:113,126` | Assume five sends per carrier; the live lane supports one. |

### 3.1 What to build

**Build CATS-B: a zero-CSV change spine, batched.** Two orthogonal, composable changes:

- **CATS (the spine).** Replace the change child's `[extension, state]` pair with a **single
  un-timelocked spine tier** at the next level plus **one** state cap. Each payment then costs one
  transaction and ~one block of exit latency instead of two transactions and 2 124 blocks.
- **Batching (the multiplier).** Widen the spine tier from 2 payload outputs to K+1. `build_split_state`
  (`lib/src/tesr.rs:437`) is already N-ary, `in_ladder_pay_many`
  (`clients/libs/rust-sdk/src/transfer.rs:1265`) already drives it. Depth advances **per batch**, not
  per payment.

**Do not build blanket denominations, and do not build an SSP swap.** Both were fully costed and
both fail (§5). The short version: a denomination purse only beats splitting if leaves come back,
and leaf return needs either a payee-makes-change protocol or an SSP swap. Simulated against a
realistic mix (V = 1M, 3 000 payments log-uniform 3k–150k, binary grid, real `exact_subset` DP over
`clients/libs/rust-sdk/src/select.rs:37`), the exact-subset hit rate with **no** leaf return was
**5 in 3 000** — denominations degenerate to exactly the baseline, plus a ~1 000-sat round-up on
top. And the swap primitive is not safe to expose today: see §5.2.

**Denominations survive only as an opt-in mode.** CATS-B's batch already produces K self-owned
pieces if you point them at your own backup address; a later payment of exactly that piece's amount
is then a free `child_retransfer`. That is worth doing for a **repeating fixed-amount book**
(payroll, subscriptions, exchange withdrawal tiers, LSP rebalancing) and nothing else. The gate is
utilisation: a carved batch of K beats plain CATS iff **more than 0.685K + 0.315 pieces** are
consumed as exact matches — ~69% of any K. Carve 20, use 10, you lost money.

### 3.2 The numbers, and what they buy

Per payment, plain (coloured in brackets):

| | per payment | vs baseline |
|---|---:|---|
| baseline (pre-CATS) | 2 046 (2 390) | — |
| CATS, K=1 | 1 556 (1 814) | −24% |
| CATS-B, K=10 | 1 115 (1 296) | −45% |
| CATS-B, K=20 | 1 091 (1 267) | −47% |
| CATS-B, K=20, **lean leaf** (§4.6, optional) | 601 | −71% |

The fee win is real but bounded — 980 of the 1 066-sat asymptote is the **piece child's own two
rungs**, which batching cannot touch and only the lean-leaf variant removes. **The fee is not why
you build this.** Exit, at N = 100 payments:

| | txs | vB | wait | CPFP top-up @20 sat/vB |
|---|---:|---:|---|---:|
| baseline | 203 | 29 675 | 4.08 years | 1 102 550 sat |
| CATS, K=1 | 103 | 17 175 | **15.7 days** | 597 550 sat |
| CATS-B, K=20 | 8 | 5 300 | **15.0 days** | **117 800 sat** |

**95× on latency at N = 100, 673× at N = 1000, and 9.4× on the realised exit cost that determines
whether the exit is solvent at all.** Per-payment latency added to the sender's own exit horizon
falls from 2 124 blocks (14.75 days) to **1 block**. The contagion is removed: a payee's exit
latency becomes constant in the sender's payment history.

The second-largest result is a **capability**, not a saving: the coloured lane goes from **exactly
one partial payment per carrier, ever** to unlimited, because the spine tip is always the same
object and there is no distinct "coloured child split" shape to implement. That comes with real
prerequisites — see §4.5, it is **not** free as previously assumed.

---

## 4. The chosen design — CATS-B in implementable detail

> **Build status (2026-08-03).** The **spine tier is BUILT**: `SPINE_CSV = 0`
> (`clients/libs/rust/src/tesr.rs`) is what all three split builders sign — `in_ladder_split`,
> `cosign_colored_in_ladder_split` and `child_in_ladder_split` — and the verifier admits it as a
> distinct KIND with bounds `[0,0]`, both for the parent's `SP` (`verify_bundle_ex` under the
> receiver-chosen `final_is_split`) and for every intermediate segment's split state. That is change
> **1** of the three in §4.1, plus **V3** of §4.5, and it is the one that removes the rung
> consumption: `§1.3`'s "coloured carriers get exactly ONE partial payment, ever" **no longer holds**
> — the refusals that enforced it are deleted, and the per-level exit latency falls from 2 124 blocks
> to 722 (`720 + SPINE_CSV + one confirmation each`). Mainnet depth 1 falls **4 289 → 2 885** blocks
> charging each of the five transactions the block its parent needs to confirm — the convention
> `exit_wait_blocks` and the `max_exit_txs` derivation both use — or **4 284 → 2 880** counting
> relative timelocks alone, which is the pair `clients/libs/rust-sdk/src/config.rs`'s exit-latency
> model quotes. The two differ by exactly `3 + 2d`; a "4 284 → 2 885" pair takes one end from each
> and overstates the saving by five blocks. (§4.4's walk below is a third accounting again — it
> charges a confirmation only on the zero-CSV spine tiers, which is why the payee line reads
> `i + 2 880` — and it says so in its own block.) **Depth 100 is not the other
> end of that comparison any more**: `enforce_split_depth_cap_shaped` refuses past depth 10 and past
> a 23-transaction exit chain on mainnet, so the deepest admissible two-tier leaf now waits ~9 383
> blocks (65 days) rather than 4.08 years, and the 1.41-year figure this line used to quote describes
> a coin the protocol declines to mint.
>
> **V1, V2, V4 and V5 are also BUILT.** V1/V2 landed together with the shape DERIVATION the
> CORRECTION block below demands (the `None` branch requires the surviving tier to spend the
> segment's own funding outpoint) and the dead-knob refusal. V4 is `SpineTipBundle` under
> `spinetip-<sid>`, with every fail-open enumerator co-edited — `parent_shape` (a tip is no longer
> read as un-laddered), `wallet_is_provably_pre_sdk`, `defend_ladders` (its own tower loop, plus the
> L2 supersession evidence), `colored_child_sids`, `auto_exit_due`, `withdraw`,
> `unilateral_exit` and — **added 2026-08-04, the one the first sweep missed** —
> `register_colored_exit_tip`. That last one is worth stating in full, because it is the shape the
> sweep is fighting: it resolved two record shapes in an `if let … else if let … else { None }`
> chain, so a coloured tip took the trailing `else` and came back `Ok(None)` — the answer a PLAIN
> coin gives, which its caller maps to no event, no fault and no error. The tip's cap would land on
> chain and the RGB engine would go on advertising the allocation at the `SP.out[K]` that cap had
> just spent: *not merely incomplete but STALE*, the exact wording that function's own doc comment
> uses about the gap it was written to close. The fix routes all three shapes through one
> `colored_exit_move` whose `match` is EXHAUSTIVE (a fourth shape is a compile error, not a fourth
> silent `None`), plus a census asserting the CALLER still constructs all three variants — the half
> an exhaustive match cannot see. V5 is `split_output_floors` → `SplitFloors { piece, change }` with
> `min_spine_tip_value` = 820 plain / 906 coloured and per-leg refusal text.
>
> `SpineTipBundle::validate()` is now a PRECONDITION of `persist_spine_tip` (the producer's only
> door): the cap must spend `(SP.txid, sp_vout)` **derived from its own signed prevout**, must pay
> the recorded exit address at its declared payload index, `sp_out_value` must equal that output's
> real value, and the cap's SIGNED `nSequence` must sit in `[d_floor, d0]` — not `[0,0]`, which
> would leave the next batch's `SP` nothing to out-race and strand the tip behind the builders'
> own `s0_csv <= SPINE_CSV` guard. Structural checks run strictly before value checks.
>
> **Change 2 is now BUILT, in two halves that had to land together.** The PRODUCER:
> `in_ladder_split` takes a `ChangeLeg` and, on the plain ROOT lane, sends the change leg to
> `establish_spine_tip_journalled` — ONE state tier at `p.state_csv(0)` directly over `SP.out[K]`,
> no extension — returning it as a `SpineTipBundle` for `persist_spine_tip` rather than as a
> `ctesr-` child. `change_leg_role()` (change 2's single flip point, which fails OPEN if flipped
> early) is now per-LANE and reports `SpineTip` for `SplitLane::PlainRoot`, so V5's 820-sat change
> floor is live on that lane and the Freeze-Lemma bound of §4.0 is **attained**: a payment adds one
> transaction to the sender's exit chain, not two.
>
> The SECOND half is the **SPINE BATCH** (`spine_batch_split`), and without it the first half is a
> capability REGRESSION rather than a saving: a tip can be neither split (it has no `tesr-` row, so
> `in_ladder_pay` cannot load it, and no `ctesr-` row, so `child_in_ladder_pay` cannot either) nor
> handed over whole (a flat conveyance would give the recipient a backup chain over an un-broadcast
> funding output — a coin with no exit), so a wallet that had made ONE partial payment was
> exit-only for the rest of its balance. The batch builds `SP_{i+1}` over the tip's own funding
> outpoint `SP_i.out[K]` at `SPINE_CSV` (via `build_split_state_from`, never the vout-0 builder),
> retires the cap `C_i` into the segment's `superseded_states`, terminalizes **the TIP's** slot
> (not the root parent's, which went terminal at batch 1), and leaves another one-cap tip. The
> sender's coin is therefore the same object and the same builder at batch 1 and at batch 1000.
> `ParentShape::SpineTip` routes to it in both `transfer` and `transfer_many`, and
> `split_preflight_pure` now admits a tip on exactly the terms it admits the coin it came from.
>
> Two consequences worth stating because they are easy to get backwards. The batch's `SP_{i+1}` is
> at `SPINE_CSV` while the new cap is at `state_csv(0)` — **two different tiers, two different
> bounds**; pin the cap to `SPINE_CSV` and it ties with every future `SP`, and the builder's own
> `cap_csv <= SPINE_CSV` guard then refuses the next batch, stranding the tip when it is already
> terminal. And a spine level costs the exit walk **ONE tier**, so `enforce_split_depth_cap` had to
> stop charging every intermediate level as two (`SplitLevelShape`, derived from the segment's own
> shape): charging a spine level as two is a silent economic cap, and charging a two-tier level as
> one mints a leaf whose exit does not fit the epoch.
>
> Still to build: the **whole-coin handover of a tip** (D3 of the phase-1 plan — promote it to an
> ordinary two-tier child, census `0 + 2 + 1`), which is refused by name today; and, on the coloured
> lane, **§4.5's RGB item 3 (per-output blinding)**. **The coloured spine BATCH is no longer on this
> list** — this sentence named it and HEAD has overtaken that: `colored_spine_batch_pay`
> (`clients/libs/rust-sdk/src/tokens.rs`) drives `build_colored_spine_batch` +
> `cosign_colored_spine_batch` and runs the root lane's consignment pre-flight over every leg before
> the tip is terminalized, and the coloured send router dispatches a coloured spine tip to it ("a
> COLOURED SPINE TIP is the carrier's shape from its SECOND payment onward, and it routes to the
> batch"). The coloured half has moved a long way since this paragraph was first written and
> the remainder is now specific: §4.5's RGB item 1 is landed (`TierRole::Spine = 0x0C`), item 2 is
> landed ([S5] — `colored_child_txids` and `colored_child_seals` both walk `ancestors`, charging a
> spine segment one tier and a two-tier segment two), and the coloured ROOT split's change leg is a
> one-rung coloured tip ([S3] — `change_leg_role(SplitLane::Colored)` reports `SpineTip`, floored at
> `colored_spine_tip_floor`), so `cosign_colored_in_ladder_split` no longer carves a two-tier change.
> `refuse_uncolored_over_colored_tip` is no longer a blanket refusal either: `spine_batch_split_ex`
> forks by lane and a coloured tip has a coloured `SP` to be built with (`build_colored_spine_batch`,
> `cosign_colored_spine_batch`). The coloured legs exist too, and are the SAME loop the coloured root
> split uses (`build_colored_split_legs`, shared deliberately so the two shapes cannot drift): per-payee
> `ext_child`/`state_child` with consignments and seals rooted at `SP.out[j]`, and one coloured cap
> for the next tip. What is still refused, deliberately and by name, is the **both-coloured** arm
> [S4b] of the **PLAIN** driver `spine_batch_split_colored` — that entry point builds every leg with
> `establish_child_journalled` / `establish_spine_tip_journalled` and persists `rgb: None`, an
> uncoloured tier over the outpoint the allocation is booked at, so it would burn the allocation
> rather than refuse. It has no callers in the repo (the coloured lane is the pair above), so that
> arm is a permanent lane guard against a latent hole, not a missing capability — CI pins it
> (`ci-guards/tests/deny_uncoloured_legs_under_a_coloured_sp.rs`). **§4.5's RGB item 3
> (per-output blinding) is still open**: `colored_tier_seal` takes `(sid, role, level, m‖csv)` and
> nothing child-specific, so the batch-concealment argument below stands unchanged.

### 4.0 What cannot be delivered, and why

The brief asked for change that stays at **root level** (a sibling of `T` over `F`). It is
unreachable, and the reason is [B1] itself. `build_trigger` is the only builder touching
`f_txid/f_vout` (`lib/src/tesr.rs:321`), `T` carries `TRIGGER_SEQUENCE = 0xFFFF_FFFD` — relative lock
**disabled** (`lib/src/tesr.rs:191`) — and every prior owner of a Model-A-conveyed coin retains a
signed copy. Any change output that is a sibling of `T` over `F` loses unconditionally to a retained
`T`: no timelock schedule out-races a transaction that has no timelock.

**The Freeze Lemma.** Payee *i* holds a coin funded by an output of a pre-signed tx `P_i`. For the
conveyance to be theft-proof, the sender must be unable to confirm anything else over `P_i`'s input
outpoint — so that outpoint is dead to the sender the moment the bundle is conveyed, and the
sender's change must move to an output of `P_i`. **Every payment therefore adds at least one
transaction to the sender's exit chain**, in any design that funds payees from pre-signed
un-broadcast transactions and adds no fresh on-chain data. The live design pays **two**. CATS pays
exactly **one**. CATS attains the bound; nothing in this architecture beats it. Constant depth is
achievable only by adding on-chain data per payment or by not funding payees from the sender's tree
at all — i.e. denominations, which fail for other reasons (§5.1).

### 4.1 The construction

Root, unchanged. `claim()` → `establish_auto` (`clients/libs/rust/src/tesr.rs:1394`) gives
`F → T → X_m → S_0`. Pure-handover coins are untouched.

Payment batch *i+1* replaces the live cap over the current spine outpoint `O_i` with a **spine
tier** `SP_{i+1}` carrying K+1 payload outputs:

```
O_i  ( = X_m.out[0] at i=0, else SP_i.out[spine] )
 │
 └─ SP_{i+1}      nSequence = 0        (K+1 payload outs + one P2A anchor)
      ├─ out[0..K−1]  → piece children: establish_child, ext CSV 720 + state CSV 1440, payee's key
      └─ out[K]       → the new spine tip
            └─ C_{i+1}   ONE state tier, CSV Δ_cap = 1440, sender's own exit key
```

Three changes of substance versus the live `in_ladder_split`:

1. **`SP`'s nSequence is 0**, not `s0_csv − δ`. The live builder computes
   `sp_csv = s0_csv.checked_sub(δ).filter(|c| *c >= d_floor)`
   (`clients/libs/rust/src/tesr.rs:2018-2025`); CATS sets it to 0.
2. **The change gets no extension.** The extension exists to reset the state budget by renewal; on
   the spine every payment already lands the change on a virgin outpoint at a virgin `D0`, so the
   rung is dead weight. That missing rung is the 490 sat and the 720 blocks CATS saves per level.
   (This was argued as dead weight **today** too, on the grounds that `renew`/`rollover` take
   `&mut TesrBundle` and no `ChildTesrBundle` analogue existed, so a split child's extension could
   never be renewed and both designs gave the change exactly 36 whole-coin hops. **That premise is
   gone**: `renew_child` is the analogue, it rebuilds both leaf tiers in place, and the budget is now
   36 hops × 16 epochs. The argument for dropping the CHANGE leg's extension survives anyway, and on
   its own stronger footing — the spine lands every payment's change on a virgin outpoint at a virgin
   `D0`, so the change leg never needs the rung the renewal path exists to step down.)
3. **K+1 payloads, not 2.** `build_split_state` and `committed_fee_for_outputs` are already N-ary.

`C_i` is disclosed as superseded. The sender's coin is always "a slot with one cap over its funding
outpoint" — payment 1 and payment 1000 are the same object and the same builder.

### 4.2 Why nSequence 0 is correct and not a corner-cut

Over spine outpoint `O_i` exactly two transactions can ever exist: the sender's retained cap `C_i`
(CSV 1440) and, later, `SP_{i+1}` (CSV 0). `SP_{i+1}` is the transaction the payees need; `C_i` is
the transaction that would **steal** from them (it sweeps all of `O_i` to the sender's key). So the
honest transaction must win, and 0-vs-1440 is the largest possible margin.

The un-timelocked tier is signed only by the sole current owner of the outpoint it spends, on the
outpoint it is simultaneously giving up — the `T`-vs-`F` asymmetry that makes [B1] dangerous does not
arise, because the voiding party and the victim are the same entity.

The payee's watchtower window — time to push `SP_{i+1}` after `SP_i` confirms — goes from **δ = 36
blocks (6 h)** in the live design to **1 440 blocks (10 days)**. `Δ_cap` is a free parameter that
costs nothing per payment (it appears once, on the sender's own final leg); 1440 is the safe
default.

### 4.3 What the blind SE signs

Per batch of K pieces:

| co-sign | under | count |
|---|---|---:|
| `SP_{i+1}` (the spine tier) | `A_spine_i` | 1 |
| `C_{i+1}` (the new cap) | `A_spine_{i+1}` | 1 |
| each piece's extension + state | that piece's own aggregate | 2K |

**Total 2K + 2 co-signs, i.e. 2 + 2/K per payment, versus 5 today.** Plus one
`set_spend_budget(…, 1)` on the outgoing spine slot — the same call `in_ladder_split` already makes
(`clients/libs/rust/src/tesr.rs:2094`), and it is K-invariant.

The SE receives a sighash and a prevout amount. `cosign_tier` is issued **once** for `SP` regardless
of K (`:2095`, outside the child loop). nSequence lives inside the transaction and is invisible to
it. The SE never learns K, the denominations, the colour, or that a spine exists rather than a
2-way split. **Zero server diff, zero enclave diff, no new endpoint, no new cryptography** — nothing
here lands on the signing lane at all, so which signing implementation is deployed does not enter
the argument. (This line used to justify itself by "the SGX/lockbox lane divergence". Do not carry
that framing forward: the deployed signer is the lockbox, and a hazard about a lane that is not the
shipping lane is the kind of claim that discredits the rest of the section.)

### 4.4 Unilateral exit at every hop

Both chains are fully pre-signed, need no counterparty, and terminate at the holder's own exit key.
`child_exit_chain` (`clients/libs/rust/src/tesr.rs:2261`) already splices every ancestor segment
root→leaf before the leaf's own tiers; only the per-segment tier count changes.

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

Every piece in a batch exits at the same depth regardless of when it was paid.

**This makes each level cheap; it does not RESET depth.** The only depth reset remains the root
re-anchor, which a split tree does not have (§7).

> **Correction — `s` no longer "grows without limit".** That was true of the latency rule alone, and
> it is exactly the hole `max_exit_txs` was built to close — the cap the code labels **[P0-3]** under
> its own numbering (`enforce_exit_chain_length`, "THE LENGTH GATE"), which in **this document's**
> §3.0 numbering is the second half of **P0-2**; §3.0's P0-3 is the split journal, and the two
> numberings do not line up. A spine tier costs one block of latency and a
> whole transaction, so an all-spine chain of thousands of tiers passes `check_exit_headroom` and is
> still unusable. `max_exit_txs` prices it in transactions instead — `3 + 2·max_split_depth`, i.e.
> **23 on mainnet** (139 on regtest) — and `enforce_split_depth_cap_shaped` evaluates it **above** the
> latency rule's early return, charging each level by its real shape (`SplitLevelShape`). A sender's
> tip walks `s + 3` transactions and a payee's piece `i + 4`, so the cap admits about **20 spine
> levels** on mainnet, not an unbounded number. That bound is on the CHAIN, not on one tier: an
> `SP`'s width is still a free parameter, and a v3/TRUC tier above 10 000 vB never relays — a
> separate, still-open finding.

### 4.5 The verifier and census changes — all security-critical

| # | change | file:line | why it is load-bearing |
|---|---|---|---|
| V1 | `ChildSegment` becomes `{ extension: Option<TesrTier>, state: TesrTier }` | `clients/libs/rust/src/tesr.rs:365-378` | a spine segment has one tier |
| V2 | the ancestor expectation `CHILD_V2_BASELINE + 2 + seg_superseded_ok` must derive the `2` from the **disclosed tier count** | `clients/libs/rust/src/tesr.rs:4706` | without it every CATS bundle is rejected outright |
| V3 | add a **SPINE tier kind** with CSV bounds `[0, 0]` alongside state/extension | `clients/libs/rust/src/tesr.rs:4287` (live), `:4162` (superseded) | **must be a new kind, never a widened state range** — see below |
| V4 | new persisted bundle key for the sender's spine tip | `clients/libs/rust/src/tesr.rs` (`SpineTipBundle`, `SPINE_TIP_KEY_PREFIX`) | `withdraw` routes anything keyed `ctesr-` to unilateral exit; the tip must not be mistaken for a leaf. **BUILT** — and the record was the easy half: every site that ENUMERATES ladder artefacts had to be co-edited, because a missed prefix does not produce an absence there, it produces a confident wrong answer (un-laddered, un-managed wallet, not a carrier, nothing to defend) |
| V5 | new floor `min_spine_tip_value = rung + dust = 820`, applied to the **change leg only** | `clients/libs/rust-sdk/src/transfer.rs` (`split_output_floors`, `inladder_amounts_floored`) | the executor applied `min_child_value` (1 310) to both legs and refused payments its own arithmetic permits; the refusal text ("each child funds its own extension + state tier") is also false for a spine child. **BUILT** as two floors, `SplitFloors { piece, change }` — the split is the point: one number can only reach 820 by lowering the PIECE's floor too, which mints a child that cannot fund its second rung and dies after the parent is terminal |

On V3: the design's own risk ranking previously called the CSV-0 admission a theft primitive. It is
not — `if sup.csv <= live_csv { reject }` (`clients/libs/rust/src/tesr.rs:4181-4187`) means a
superseded tier at CSV 0 is **always** rejected; it fails closed. The invariant that genuinely
weakens is different and unnamed: today the tier kind (and therefore its CSV bound) is **structural
and unforgeable**, derived from position parity (`let is_extension = i % 2 == 1;`, `:4285-4288`) and
from a hard-coded pair in the ancestor loop (`:4659-4668`). `extension: Option<TesrTier>` makes
segment **shape sender-declared**. ~~It still fails closed via the exact-equality census (a dropped
tier leaves `expected` one short of `num_sigs`)~~ — **struck: that claim is false, see the CORRECTION
immediately below.** The adversarial budget does belong here rather than at the race check, which was
the one part of this paragraph that held up.

> ### ⚠️ CORRECTION (2026-08-03) — the sentence above is WRONG, and the reason matters
>
> **"A dropped tier leaves `expected` one short of `num_sigs`" is false.** A dropped tier is not
> lost. The sender re-declares it in `superseded_extensions`, where `verify_superseded_segment`
> counts it (it returns `sups.len()`), and `expected` moves by exactly the same 1 in the opposite
> direction. `CHILD_V2_BASELINE + 1 + 1` and `CHILD_V2_BASELINE + 2 + 0` are the same number for
> the same segment. **The census re-balances exactly, every co-sign is real, and every other check
> passes.** Three independent adversarial lenses reached this conclusion separately.
>
> Today the attack is blocked by something else entirely: `live_ids` contains BOTH tier txids, so
> the [C-2] dedup refuses any attempt to also disclose the extension as superseded. **V1's `None`
> branch takes the extension out of `live_ids` and un-blocks it.** The defence that is about to be
> removed is not the one this section credits.
>
> What actually closes sender-declared shape, and what must therefore be implemented deliberately
> rather than inherited:
>
> 1. **The prevout re-anchor.** In the `None` branch, require the surviving tier to spend the
>    segment's own **funding outpoint** — `st_in.previous_output == (fund_txid, seg.funding_vout)`.
>    A genuine two-tier segment's state spends `ext.out[0]`, so it cannot be re-labelled. This is
>    the single load-bearing check, and it is *derived from a signature*: the outpoint is committed
>    by the taproot `SIGHASH_ALL` sighash, so it cannot be repointed without invalidating the SE's
>    own signature. Per ADMISSION-INPUTS (retired 2026-08-15), that makes shape **derived**, not **declared** — the
>    `Option` becomes a cross-checked declaration that must agree, never the source of truth.
> 2. **The `[0,0]` CSV pin** stays exactly disjoint from `[e_floor, e0]`. Note `[144,720]` is a
>    strict *subset* of `[144,1440]`, so extension-vs-state was **never** CSV-separable; only the
>    spine's `[0,0]` is disjoint from both, which is why widening it for the `None` case destroys
>    the last structural layer.
> 3. **The dead knob.** Child-side `superseded_extensions` has no honest writer. Refuse a non-empty
>    list whenever `extension.is_none()`. Free, independent of (1), and it closes the re-declaration
>    route directly.
>
> Without (1) the concrete consequence is **P0-1 re-opened through a new door**: a real
> `[ext 720, state 1440]` segment declared as a spine loses 721 blocks from its declared exit chain,
> and `check_exit_headroom` admits a child near the epoch boundary whose real exit cannot finish.
>
> Also corrected: **V1 must not be applied to the conveyed leaf.** At the leaf the two ranges
> overlap completely, so nothing CSV-based separates a cap from an extension there — only the
> Model-A payee check does, which is far more weight than that check was designed to carry. A
> conveyed piece stays strictly two-tier; the spine tip is never conveyed and gets its own record
> (V4). And **V1 must land in the same commit as V2**: the literal `2` against a one-tier bundle is
> a free census slot, and that mismatch fails *open*.

**Census, K-invariant, exact equality holds:**

- *root slot* — `SP_1` is the terminal state; `S_0` and prior states are superseded, all with
  CSV ≥ 144 > 0, so `:4181` passes with the largest possible margin.
- *spine slot i* — baseline 0 (`CHILD_V2_BASELINE`, `:2217`: never funded on-chain, so
  `check_deposit`/`create_tx1` never runs). At rest 1 live (`C_i`) + 0 superseded = 1. After the next
  batch: 1 live (`SP_{i+1}`) + 1 superseded (`C_i`) = 2. A whole-coin handover of the tip adds
  exactly +1/+1, the arithmetic `child_retransfer` already relies on (`:2650-2652`).
- *piece slot* — unchanged, `flat_backups + 2 + superseded` (`:4874`).
- *replace-by-lower-timelock* — `X_m.out[0]`: 0 < 144…1440. `SP_i.out[spine]`: 0 < 1440. Leaf:
  untouched.
- *the census trap is respected* — `flat_backups` is never 0; `in_ladder_split` reads the parent's
  real chain and refuses `parent_backups.len() < PARENT_V2_BASELINE` **before** `set_spend_budget`
  (`:2080-2094`).

**RGB — this is real work, not a claim.** The coloured spine needs three additions, and the earlier
assertion that "the existing coloured root builder covers every payment" is **false**:

1. `TierRole::Spine = 0x0C` (`clients/libs/rust/src/rgb.rs:770-781`; never renumber existing tags).
2. `colored_child_txids` (`clients/libs/rust/src/tesr.rs:243-256`) and `colored_child_seals`
   (`:283-330`) hard-refuse any child with non-empty `ancestors` and emit a hard-coded 5-entry seal
   schedule. Both need an N-deep witness list and seal schedule walking `ancestors`.
3. **Per-output blinding.** `build_colored_tier` derives ONE `seal.blinding()`
   (`clients/libs/rust/src/rgb.rs:1246`) and passes it once for an `output_map` covering every
   payload; `colored_tier_seal` (`clients/libs/rust/src/tesr.rs:1634-1640`) takes parent sid, role,
   `m`, CSV — nothing child-specific. A concealed seal commits to `(method, txid, vout, blinding)`,
   so with B known and vouts enumerable, payee *j* de-conceals every sibling seal in K tries. At K=1
   this leaks the sender's change to the one payee already transacting with them; at K=19 it makes
   nineteen mutually-unrelated payees and their exact allocations linkable. Not theft — a seal is
   not spendable without the key — but **concealment across a batch is worth zero bits**, and the
   anti-collision property (rival tiers over one outpoint must not share a blinding, or their
   `BundleId`s collapse into an arbitrary hash lottery) is preserved only because `SP` and `C` differ
   in role and CSV.

Until (3) lands, **coloured K > 1 is restricted to batches whose payees already know each other**
(payroll, one merchant's own settlements); coloured K = 1 for unrelated payees.

### 4.6 The lean-leaf option (separate decision)

Hang the piece's state tier **directly** off `SP.out[j]` and drop its extension.
`build_state_from` (`lib/src/tesr.rs:383`) already roots at an arbitrary outpoint. This cuts 490
plain / 576 coloured **and 720 blocks** per piece, taking the batched floor from 1 066 to 576 and
the payee's wait to `i + 2 160`.

The cost is the piece's renewal rung — and **that cost is now real, which it was not when this
section was written.** The original argument was "the rung is unreachable today (no child rollover
exists), so this is not a capability trade *now*, only a foreclosure of one later". Child renewal
has since landed: `renew_child` / `renew_child_auto` rebuild the leaf's extension and state in place
over the same `SP.out[j]`, taking the leaf's transfer budget from 36 hops to 36 hops × 16 epochs per
depth level. Dropping the piece's extension forfeits a live capability, so the trade is now
**490 sat and 720 blocks per piece against 15 further renewal epochs**. Recommend landing CATS-B
first and taking this as a separate, argued change — with that price named.

### 4.7 Prerequisites that gate K > 1

**All five are now CLOSED.** Each is kept with the reason it was a gate, because that reason is what
the fix has to keep being true.

- ~~**P0-3** (crash-safe carve) must land first.~~ **CLOSED.** The unrecoverable window was `2K + 2`
  SE round-trips wide: at K = 20 an 8.6× increase in independent failure points that destroy the
  whole coin. `SplitJournalRecord` (`splitjrnl-`) is written complete **before** the parent's budget
  is touched, each tier's signature is journalled the instant it exists, and
  `resume_in_ladder_split` co-signs exactly the tiers still `None` — checking the journalled leg
  ROLE bidirectionally, so a `Piece` can never be resumed into a one-rung tip or the reverse.
- ~~**Idempotent re-conveyance.**~~ **CLOSED.** `in_ladder_pay_many` still conveys the K pieces
  serially after the parent is terminal, but each leg now carries a `ConveyanceStage` advanced only
  forward and journalled **before** the network call it describes — so "the call never happened" and
  "the call happened and the answer was lost" are distinguishable (by `conveyance_x1`) instead of
  being one indistinguishable loss of bundles *j..K−1*.
- ~~**Coin selection must not eat its own inventory.**~~ **CLOSED.** `Candidate` carries
  `is_inventory` and `plan_with_floor` sorts on `(!is_inventory, amount_sats)`
  (`clients/libs/rust-sdk/src/select.rs`), so every inventory candidate outranks every non-inventory
  one regardless of size — a forecast miss splits the spine tip, not the smallest piece.
- **Derived-slot budget.** `max_derived_tokens_per_statechain = 64`
  (`server/src/server_config.rs`), counted over lifetime issuance **including spent rows**
  (`count_derived_tokens` is `SELECT COUNT(*) … WHERE derived_from = $1`). K ≤ 63 per spine level —
  `DERIVED_SLOTS_PER_STATECHAIN = 64`, `MAX_BATCH_RECIPIENTS = 63`, refused up-front by
  `refuse_oversized_slot_batch` — and because each level is a **fresh** statechain, the cap is
  per-level, not global. The retry-budget collapse (31 spare attempts at K = 1, 2 at K = 20) is
  **CLOSED** the second way this bullet asked for: `take_derived_tokens` spends leftover vouchers
  from an earlier attempt first and persists the pool *before* handing any out, so a failed attempt
  costs the parent's lifetime allowance nothing.
- ~~**The watchtower cannot express the trigger.**~~ **CLOSED [S7].** The three items this bullet
  demanded are built: `WatchTrigger { watch_txid, watch_vout, csv_blocks, push_txs }` expresses the
  event, `watch_pass` evaluates it against the outpoint (`outpoint_spent`) alongside the height
  predicate and acts when **either** fires, and `WatchState::Blind` means an entry the pass could
  not *evaluate* never averages into a green `Idle`.

  Closing it surfaced a second, larger defect in the same place, which is the part worth
  remembering: **`export_watch_bundle` was not emitting leaf entries at all.** A split child is a
  `ctesr-` row and a spine tip a `SPINE_TIP_KEY_PREFIX` row; the export looked only for `tesr-`
  (ladder) and `branch-` (flat) rows, so for a leaf both reads came back empty and it took the
  `continue` written for a flat deposit — a coin with on-chain funding that no ancestor can race,
  i.e. the exact opposite of a leaf. Every leaf in the wallet was silently absent from every
  exported bundle, and the export still returned `Ok`. The in-process tower (`auto_exit_due`) covers
  leaves, which is what hid it: **delegating to a third-party tower protected the parents and left
  the children unwatched.** So the trigger type existing was never sufficient — nothing was reaching
  it.

  A leaf now arms **both** predicates, because it genuinely has both: `deadline_block = L_k −
  head_start` (its clock is the *parent's* lowest flat-backup rung — a rung belonging to the
  splitter) and the event (an ancestor spending `F`). `head_start` comes from the **bound** chain, so
  a depth-N leaf is charged all N spliced spine levels, and via the same `exit_wait_blocks` call
  `auto_exit_due` uses — the delegated tower and the owner's own tower cannot drift apart. Every
  unbuildable entry aborts the export by name rather than being dropped from it.
- **Minimum parent value** for a K-batch: `1 396K + 1 310` sat (K=1 → 2 706; K=10 → 15 270; K=20 →
  29 230). Below it, K falls back. Coloured carriers at `TOKEN_CARRIER_SATS = 17 384` support
  K ≤ 4 and must be re-sized at issue.

### 4.8 The liveness trade — state it, do not hide it

CATS is **symmetric**: zero-CSV spine tiers accelerate the honest exit and the theft identically.
That is the mechanism, not a bug. But the consequence must be published:

| | payee's total on-chain warning before a steal confirms |
|---|---|
| baseline, victim at depth 100 | `2124·100 + 1440` ≈ 213 840 blk ≈ **4.07 years** |
| CATS, victim at depth 100 | `2160 + 100` = 2 260 blk ≈ **15.7 days** |

You cannot delete the latency and keep the margin — the baseline's multi-year safety window is a
byproduct of the multi-year exit that makes it unsound. ~10–15 days of required watchtower liveness
is a normal L2 assumption (LN `to_self_delay` runs 144–2016 blocks). But "check in at least every 10
days, forever" becomes a per-payee obligation, for a $2 payee as much as a $2 000 one, and
sub-economic payees will rationally abandon. The `Δ_cap` parameter is the dial: raising it above
1440 lengthens only the sender's own final leg.

> ⚠️ **This section understates it, and the correction matters.** "Sub-economic payees will
> rationally abandon" frames the cost as **liveness**, borne by the payee. It is a **finality**
> limit, and it is the **sender's free option**. A split child has no flat backup
> (`CHILD_V2_BASELINE = 0`); the sender keeps one that spends `F` for **112 vB**
> (`lib/src/transaction.rs:116`) and pays them the whole coin — **6× to 265× cheaper** than the
> payee's `293d + 375` vB walk, cheaper than the sender's *own* ladder at every fee rate, and with
> **zero marginal cost** per additional piece voided. So an ordinary sender exiting for their own
> reasons voids every sub-economic piece they ever paid, and the admission floor does not protect
> anyone: `min_child_value` = 1 310 sat **is** the break-even function evaluated at the hardcoded
> `committed_fee_rate = 2.0` (`lib/src/tesr.rs:190`) and at no other rate. At 20 sat/vB a depth-10
> piece admitted at 1 310 costs **124 870** to defend. CATS shrinks the `d` term (§6) but leaves the
> option free and, per the table above, shortens the window in which the payee could notice.
> The band, the three enforcement buckets, and the ranked fixes are in
> SUBECONOMIC-FINALITY (retired 2026-08-15).

---

## 5. Rejected alternatives, and the attack that killed each

### 5.1 DFO — Denominated Fan-Out (one split, N self-owned leaves, then whole-leaf handovers)

Fan the deposit into N denominated leaves at claim; pay by exact-subset handover (free, depth-1
forever); handle the residue by payee-makes-change or an SSP swap.

**Killed by: an irreversible one-way commitment whose transferability depends on an artifact it has
just made un-renewable.** `in_ladder_split` calls `set_spend_budget(parent, 1)`
(`clients/libs/rust/src/tesr.rs:2096`) and `SP` consumes it, so `sign/first` and `sign/second` return
410 Gone thereafter (`server/src/endpoints/sign.rs:293-299`) and `set_sig_budget` can only tighten
(`server/src/database/deposit.rs:231-238`). There is **no second fan-out and no re-denomination**.
Meanwhile every leaf hop re-runs `validate_backup_chain_v2` against the **live** tip and fee rate
(`clients/libs/rust/src/tesr.rs:2418-2440`, called at `transfer_receiver.rs:560` and `:979`), which
rejects on a two-sided ±5 sat/vB fee band (`lib/src/transfer/receiver.rs:471-476`) — and the
`auto_refresh_before_spend()` call that renews it (`clients/libs/rust-sdk/src/transfer.rs:83`) has
no subject left, because after `denominate()` every coin is a terminal parent or a `ctesr-` child.
A fee move of >5 sat/vB in either direction makes **all N leaves simultaneously un-conveyable** with
no remedy but a full unilateral exit.

Compounding it: DFO universalises **P0-1**, so for 43% of every epoch it can hand payees provably
unexitable coins; N ≤ 63 from the derived-token lifetime cap; and the coloured lane does not merely
need work — `colored_multi_carrier_transfer` never admits children as legs
(`clients/libs/rust-sdk/src/tokens.rs:4076-4086`), so after a fan-out the wallet reports **"COLOURED
carriers hold 0 in total"** while holding the entire deposit.

Economics, even setting safety aside: with its own recommended binary ladder the ceiling is
**17.3×**, not the 69–174× claimed (those rows exceed the N·36 leaf-hop budget — a leaf survives
exactly `(1440−144)/36 = 36` hops, `clients/libs/rust/src/tesr.rs:2664-2678`). With **no leaf
return** it is **1.0×** — identical to the baseline. And the fan-out **recurs 9.2×/year** (the tree
must fully materialise before `H_deposit + initlock`, and materialisation itself takes 4 284 blocks,
leaving a 5 716-block usable window), so a 10-leaf lattice on a 1M deposit burns **9.72%/yr
regardless of payment count** — a loss for any wallet under ~46 payments/year.

### 5.2 DENOM-SWAP — fixed-denomination lattice with atomic-batch SSP reshaping

Hold a lattice of denominations; pay by exact subset; reshape via the existing N-party atomic batch
transfer with the SSP, value-conserving and coin-for-coin.

**Killed by: the batch primitive is not atomic, and the sender's veto is bypassable without a
signature.** Three independent breaks, all in shipped code:

1. **An aborted leg permanently bricks the coin, and the tree says so.**
   `presign_receiver_state` co-signs `S'` on a **clone** and does not mutate the sender's bundle
   (`clients/libs/rust/src/tesr.rs:3346-3356`), but the SE's `sig_count` increments regardless. The
   sender keeps a bundle whose census can never balance —
   `clients/libs/rust/src/transfer_sender.rs:1057-1061` ("on ROLLBACK the orphan `S'` co-sign inflates
   the reclaimed coin's `sig_count`, so a later `verify_bundle` bricks re-transfer") and
   `clients/libs/rust-sdk/src/ssp.rs:1201-1212` ("Re-transfer stays orphan-bricked until a
   `refresh()` re-anchor"). One stalled leg bricks **all K′** of the user's outgoing coins; recovery
   is K′ on-chain re-anchors; and for **coloured** coins recovery arrived only with CR-D
   (`colored_reanchor`, `clients/libs/rust-sdk/src/refresh.rs`) and only for a coin whose ladder is
   itself coloured — when this was written there was none at all, and for an allocation on a plain
   ladder there still is none. Worse, the SSP then holds a co-signed `S'` at
   `csv − δ` while the user's retained `S` sits at `csv` — the SSP's rival matures **first**. The
   tree states the bar (`ssp.rs:1206-1208`): "the SSP holds the broadcastable `S'` and is trusted not
   to race it." That is operator trust, not atomicity.
2. **The one change it calls "one line of SDK, zero server" deletes the audit-[16] guard.**
   `post_paymenthash` validates only that the caller signed for its **own** `statechain_id`
   (`server/src/endpoints/lightning_latch.rs:61-107`) — no check that it is entitled to `batch_id`.
   That is contained today only because `create_pre_image` mints a fresh UUID client-side
   (`clients/libs/rust/src/lightning_latch.rs:16`). Make `batch_id` caller-supplied and anyone who
   learns one self-registers into it and wedges every honest leg.
3. **Theft.** `post_paymenthash_external` accepts any `batch_id` with an attacker-chosen
   `payment_hash` (`server/src/endpoints/lightning_latch.rs:181-207`); `unlock_by_preimage` then
   enumerates **every** `statechain_id` in the batch by `batch_id` alone
   (`server/src/database/lightning_latch.rs:166-170`) and clears `locked2` — the **sender's veto** —
   with no signature from those senders (`server/src/database/transfer_receiver.rs:259-265`). The
   SSP knows the `batch_id` by construction. It can clear the veto, unlock its own legs, create no
   outbound legs, and claim every coin the user put in.

The endpoint holes pre-date the design, but DENOM-SWAP makes exactly that configuration the
universal payment path. Additionally: its recommended `b = 2 000` is below the **maintenance bound**
— `reanchor` refuses unless `amount − ceil(112·r) ≥ 330` (`clients/libs/rust-sdk/src/refresh.rs:165-181`),
so a 2 000-sat coin is unmaintainable above 14.9 sat/vB — and a defensible `b` of 10 000–20 000 makes
the off-lattice rounding residual (`E[b/2]`) **worse than the 2 046-sat baseline**.

### 5.3 Batched SP alone (no spine)

Not rejected — **absorbed**. Batching is half of the recommendation. On its own it divides depth by
K but leaves the leading term at 2 124 blocks per level, so at K = 20 a 1000-payment history still
costs 103 txs and **2.06 years** to exit. The spine is what removes the 2 124.

### 5.4 Change at root level

Killed by [B1] before any cost analysis: see §4.0.

---

## 6. Cost table

Plain lane, `r = 2.0`, mainnet params. "Locked" = committed fee + P2A in un-broadcast tiers;
identical to burned, since it is recoverable only by broadcasting.

| | per payment | setup delta | locked after N | exit @ N=10 | exit @ N=100 | exit @ N=1000 |
|---|---:|---:|---:|---|---|---|
| **baseline (pre-CATS)** | 2 046 | 0 | 1 470 + 2 046N | 23 tx / 3 305 vB / 162 d | 203 tx / 29 675 vB / **4.08 yr** | 2 003 tx / 293 375 vB / **40.4 yr** |
| **CATS, K=1** | 1 556 | **0** | 1 470 + 1 556N | 13 tx / 2 055 vB / **15.1 d** | 103 tx / 17 175 vB / **15.7 d** | 1 003 tx / 168 375 vB / **21.9 d** |
| **CATS-B, K=10** | 1 115 | 0 | 1 470 + 1 115N | 4 tx / 930 vB / **15.0 d** | 13 tx / 5 925 vB / **15.0 d** | 103 tx / 55 875 vB / **15.7 d** |
| **CATS-B, K=20** | 1 091 | 0 | 1 470 + 1 091N | 4 tx / 1 360 vB / **15.0 d** | 8 tx / 5 300 vB / **15.0 d** | 53 tx / 49 625 vB / **15.3 d** |
| **CATS-B K=20 + lean leaf** | 601 | 0 | 1 470 + 601N | same | same | same, payee −720 blk |
| *DFO (rejected)* | 0 on-grid / `E[u/2]` off-grid | **1 066N − 86, recurring 9.2×/yr** | 1 384 + 1 066N | 5 tx / 1 012 vB / 29.8 d | *unreachable — tree is terminal* | — |
| *DENOM-SWAP (rejected)* | 0 on-lattice / `E[b/2]` off | K onboarding tokens + 11·43·r vB | K · 1 800 (flat) | 3 tx / 375 vB / 14.75 d | same | same |

> **The N = 100 and N = 1000 columns are now bounded by a rule none of these rows model.** The
> exit-chain length cap admits 23 transactions on mainnet, so `CATS, K=1` at N = 100 (103 txs) and
> the N = 1000 entry of **every baseline/CATS row** (2 003, 1 003, 103 and 53 txs) are refused at
> build time by `enforce_split_depth_cap_shaped` — the payment forces a batch instead. The two
> *rejected*-alternative rows are not covered by that claim and must not be read as if they were:
> DENOM-SWAP's exit is a flat 3 tx / 375 vB at every N, comfortably inside the cap (it is rejected
> for the reasons in §5.2, not by this rule), and DFO has no N = 1000 entry to refuse. What the
> columns still show correctly is the *ordering*: the cap binds K=1
> at ~20 payments and K=20 at ~400, which is the same ratio the table reports in latency and fees.
> The rows are the cost model, not an admissibility claim.

Coloured per payment: baseline **2 390** (capped at N=1); CATS K=1 **1 814**; CATS-B K=10 **1 296**;
K=20 **1 267** — and, unlike the baseline, **repeatable**.

Realised exit cost at a live fee rate (external CPFP top-up, N = 100 payments, ~152-vB child per
tier because TRUC admits one unconfirmed child and the CSVs are sequential):

| live rate | baseline | CATS K=1 | CATS-B K=20 |
|---:|---:|---:|---:|
| 5 sat/vB | 194 585 | 105 085 | **20 060** |
| 20 sat/vB | 1 102 550 | 597 550 | **117 800** |
| 50 sat/vB | 2 918 480 | 1 582 480 | **313 280** |

**This is the largest number in the document.** On a 1 000 000-sat deposit the baseline's exit is
insolvent above ~15 sat/vB; CATS-B's is solvent to well past 50.

Tail reach, V = 1 000 000 at 10 000/payment: baseline **90 payments** then refusal (dead zone
1 310–3 686, residue 3 570 worth 2 590 on exit). CATS spine-tip floors are lower — exitable at
**820**, splittable at **2 706** (coloured 3 050) — so the reach extends to ~94 payments at K = 1 and
further under batching, with 147 734 sat (14.8%) of reserve versus the baseline's 185 610 (18.6%).

---

## 7. What does NOT improve

Be explicit; none of this is fixed by CATS, batching, denominations, or swaps.

**The ~69-day root epoch survives untouched.** The depositor holds a flat backup maturing at
`H_deposit + lockheight_init` — **10 000 blocks ≈ 69.4 days on every mainnet-schedule network**
(`TesrParams::flat_ladder_params`: `bitcoin`/`testnet`/`signet` = 10 000/100, regtest = 1 000/10;
`docker-compose-main.yml` sets `LOCKHEIGHT_INIT: 10000`). `T` is
un-timelocked and spends `F`, so strictly the obligation is that **`T` confirm before the earliest
live flat-backup locktime** — once `T` confirms, `F` is spent and every flat backup is dead, and the
remainder of the chain is relative-only with no absolute deadline.

**One on-chain re-anchor per tree per epoch is unavoidable — and for a split tree it is not one
transaction.** For a coin that has never been split, `refresh()` → `reanchor()` is a clean 1-tx /
112-vB reset. For a tree that has made even one partial payment it does not exist: the root is
terminal (`set_spend_budget(…,1)` consumed by `SP`), so the SE refuses to co-sign
(`server/src/endpoints/sign.rs:293-299`) and `withdraw` has no confirmed outpoint to spend. The only
re-anchor is **full unilateral materialisation followed by a fresh deposit**. Under the baseline at
N = 100 that is 203 txs and 4.08 years — the mandatory epoch obligation is *unreachable*, which is a
break, not an expense. CATS-B makes it 8 txs and 15 days: comfortably inside a 10 000-block epoch.
**CATS-B makes the unavoidable re-anchor affordable; it does not remove it, and depth resets only
there.**

To actually move it you would need one of:

- **Raise `lockheight_init`.** It lengthens the depositor's clawback window and therefore the trust
  window — and since [D8(f)] it is **no longer a per-deployment dial**: clients compile in
  `TesrParams::flat_ladder_params(network)` and refuse any coordinator whose `initlock`/`interval`
  disagree, and the coordinator itself **panics at boot** rather than serve a mismatched pair
  (`server/src/server_config.rs`). Changing it is a protocol change shipped on both sides at once,
  not a compose edit. (Earlier drafts of this document cited a **"mainnet profile of 50 000"** as a
  live dial, and that was TRUE when written: `docker-compose-main.yml` carried `LOCKHEIGHT_INIT:
  50000` / `LH_DECREMENT: 6` until D8(f) — commit **9f5ce80**, "D27/D8(f): compile in the flat
  ladder" — aligned it to the compiled-in **10 000/100**. `server_config.rs`'s own D8(f) comment
  keeps the record: *"This check found three disagreeing configs when it was written:
  `docker-compose-main.yml` (50000/6), `docker-compose-test.yml` (1100/1) and this repo's
  `Settings.toml`"*. What changed is not that the profile never existed but that it is no longer
  reachable: a coordinator configured 50 000/6 today panics at boot. That is a superseded deployment
  baseline, not a drafting error. SUBECONOMIC-FINALITY (retired 2026-08-15)'s 50 000 is a different thing again and
  is **correct**: a row of an A-1 sensitivity table its own lead-in labels *"design space, not
  deployment"*, with **10 000 (shipped)** bolded two rows above it.)
- **A co-operative de-trigger for terminal trees** — the SE co-signing a fresh spend of `F` after the
  tree is terminal. Requires raising the spend budget on a terminalized statechain *and* a protocol
  for invalidating every live child with its holder's consent. Hard; not designed.
- **A child re-anchor primitive.** Structurally impossible as posed: a child's funding `SP.out[j]` is
  un-broadcast, so there is no confirmed outpoint to spend, and producing one *is* the on-chain
  transaction you were trying to avoid.

**Also unchanged:**

- **The payee-borne 980 sat** (1 152 coloured) per received piece. Only the lean-leaf variant (§4.6)
  touches it, halving it to 490. Batching and the spine do not.
- **Depth still never RESETS** — but it is no longer unbounded, and neither CATS nor batching is what
  bounds it. `enforce_split_depth_cap_shaped` refuses past `max_split_depth` (10 on mainnet) and past
  `max_exit_txs` (23 transactions), the latter evaluated above the latency rule precisely because a
  spine level is cheap in blocks and not in transactions. CATS makes each level cost one tx and one
  block; batching divides the level count by K; the cap is what turns "unbounded" into "priced".
- **A child can never be RE-ANCHORED** — `refresh()` routes a `ctesr-` coin through `withdraw` to
  `unilateral_exit`, because `SP.out[j]` is un-broadcast and there is no confirmed outpoint to
  co-operatively spend. **It can now be RENEWED**, which is the part this bullet used to get wrong:
  `renew_child` / `renew_child_auto` rebuild `child_extension` + `child_state` in place over the
  same `SP.out[j]` — +2 co-signatures, +2 superseded entries, census unchanged — for zero on-chain
  bytes and no depth. The refusal string now names it (`child_supersede_csv`: "RENEW IT:
  `renew_child` …"), so it no longer instructs the user to do something the code refuses.
- **A coloured carrier CAN be re-anchored, if its ladder is coloured.** CR-D landed:
  `colored_reanchor` (`clients/libs/rust-sdk/src/refresh.rs`) broadcasts the trigger if it is not
  already on chain and then a co-signed **coloured de-trigger** carrying a valid state transition —
  two transactions, no SE change. What remains dead is the crossed pair, and both lanes refuse it by
  name: a coloured carrier down the plain `refresh` (which would destroy the allocation) and an RGB
  allocation sitting on a **plain** ladder, which CR-D cannot help with — a coloured de-trigger needs
  coloured material to build from, so such a coin must be moved off-carrier first. That residue, not
  the whole coloured lane, is what dies at its root epoch.
- **The 36-hop CSV budget, now renewable.** A child survives `(1440−144)/36 = 36` whole-coin
  handovers per epoch (`child_supersede_csv`), and `renew_child_auto` steps the extension one rung
  down and resets the state to `state_csv(0)`, so the budget is **36 hops × 16 epochs**
  (`m_max + 1`) per depth level rather than 36 and then never again. A leaf that has itself made a
  partial payment is TERMINAL at the SE and cannot renew — `renew_child` refuses that by name,
  pre-flight, before burning a co-signature. `CoinInfo` (`clients/libs/rust-sdk/src/types.rs`) still
  exposes no `hops_remaining` (no such field exists anywhere in the tree), so no wallet can warn a
  user that a received coin is one hop from needing a renewal it may not be entitled to.
- **Nothing is offline.** Every payment needs an authenticated derived-token draw and SE co-signs.
  CATS-B buys depth, latency and fees — not availability.

---

## 8. Build order

| phase | content | gates | status |
|---|---|---|---|
| **0** | P0-1 … P0-6 (§3.0) | none — these were live defects; P0-1 and P0-3 were exploitable | **DONE** — all six, §3.0 |
| **1** | CATS spine, plain lane, K = 1: V1–V5 (§4.5) + watchtower event trigger (§4.7) | Phase 0; adversarial E2E on **sender-declared segment shape**, not the race check | **DONE** — §4 build status |
| **2** | Batching K > 1 on the spine tier (plain) | crash-safe carve, idempotent re-conveyance, `is_inventory` in `Candidate`, lazy slot minting | **DONE** — all four gates closed (§4.7); `spine_batch_split` |
| **2b** | Coloured spine: `TierRole::Spine`, N-deep seal schedule, **per-output blinding** | Phase 2; until blinding lands, coloured K > 1 only for mutually-known payees | **PART** — role, seal schedule, coloured change tip and the coloured BATCH itself (`colored_spine_batch_pay` → `build_colored_spine_batch` / `cosign_colored_spine_batch`) all landed; what is open is **blinding**. The [S4b] both-coloured refusal is in the PLAIN driver `spine_batch_split_colored` (no callers) and is a lane guard, not the missing batch |
| **3** | Opt-in self-carve inventory (Mode B) for fixed-amount books | utilisation gate `> 0.685K + 0.315` enforced in the planner | open |
| **4** | Lean leaf (§4.6) — separate, argued decision | forecloses child renewal — and child renewal now EXISTS (`renew_child`), so this is a live capability to forfeit, not a theoretical one | open, and **more expensive than when it was written** |

Everything before Phase 3 is unconditional. Phase 3 is the only place denominations appear, it is
opt-in, and it is **not** gated on an SSP swap — which cannot be built safely until the
`lightning_latch` holes in §5.2 are closed.
