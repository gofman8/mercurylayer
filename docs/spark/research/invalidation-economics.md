# Invalidation economics — pricing entry, exit, size and time

Companion to [learn/invalidation.md](../learn/invalidation.md) (mechanism comparison),
[learn/invalidation-deep-dive.md](../learn/invalidation-deep-dive.md) (long-form explainer),
[learn/exits.md](../learn/exits.md) (exit flows), [SPEC.md](../SPEC.md) (normative REQ/INV/ERR)
and [INVALIDATION-SPEC.md](../INVALIDATION-SPEC.md) (normative IVL-REQ/IVL-INV).
This page prices the invalidation design: what it costs to enter, to hold, to transact, and to
leave — cooperatively or unilaterally — and how those costs scale with feerate, tree depth, coin
size and time.

**Measured anchors** (everything below derives from these; anything else is labelled *model*):

| Anchor | Value | Source |
|---|---|---|
| Backup tx vsize (1-in-1-out P2TR keyspend) | **112 vB** | `lib/src/transaction.rs:116` (`BACKUP_TX_SIZE`), unit tests `clients/libs/rust-sdk/src/types.rs:158-179`, `SDK_E2E=7` |
| Split branch tx vsize (1-in-2-out P2TR) | **155 vB** | measured, `types.rs:168`, `SDK_E2E=7` |
| Depth-1 exit total | **267 vB** | 155 + 112, measured (`types.rs:170-174`) |
| Cooperative withdraw tx | **~111–140 vB** | measured range, 1-in-1-out P2TR spend |
| Split fee reserve | **`clamp(parent_sats/100, 300, 2000)` sats** | `clients/libs/rust-sdk/src/transfer.rs:596-599` (INV-10) |
| Dust floor on every split output & backup output | **330 sats** | `lib/src/transaction.rs:119` (`DUST_LIMIT`), audit [9] |
| Exit fee formula | `fee = ceil(total_vbytes × rate)` | `types.rs:136-138` (INV-17) |

**Profiles.** A = deployed `initlock 1000 / interval 10` (`server/Settings.toml:2-3`);
B = code default `10000 / 100` (`server/src/server_config.rs:69-70`). Both give
`initlock/interval = 100` transfer hops. Wall clock: ~144 blocks/day.

**Units.** Sats everywhere. USD figures, where given, are parametric at an illustrative
**$100,000/BTC ⇒ 1 sat = $0.001** — scale linearly for any other price.

---

## 1. The exit cost model

A unilateral exit of a depth-N sub-coin broadcasts **N branch txs + 1 backup tx**; a flat
(deposit) coin broadcasts the backup only (`wallet.rs:672-773`).

```
total_vbytes(N) = 112 + Σ branch_vsize_i          ≈ 112 + 155·N   (split-only chain)
fee_at(r)       = ceil(total_vbytes × r)          (types.rs:136-138, INV-17)
wait_blocks     = max(0, backup_locktime − tip)   (wallet.rs:603-605)
backup_locktime = h_cosign + initlock − interval·k   (lib/src/transaction.rs:148-163)
```

Combine hops are larger — roughly +57.5 vB per extra P2TR input (*model*; `SDK_E2E=26` measures
split-only chains per depth — combine chains remain unmeasured). `estimate_exit_cost` (`wallet.rs:573-632`) does not model anything: it
decodes the actual pre-signed txs and sums their real vsizes.

**Who pre-pays what, and when:**

| Cost component | Fixed when | Paid by | Amount |
|---|---|---|---|
| Branch tx *i* fee | at split time | the **splitter** (deducted from parent: `change = parent − piece − reserve`, INV-10) | `clamp(parent_i/100, 300, 2000)` sats |
| Backup tx fee | at co-sign time (deposit, and each transfer hop re-signs a fresh backup) | the coin itself (deducted from the swept amount) | `ceil(112 × min(SE-quoted rate, client max_fee_rate))`; `max_fee_rate` defaults to **1.0 sat/vB** (`clients/libs/rust/src/client_config.rs:70`, overridable in client settings) |
| CPFP top-up (fee spike) | at exit time | the exiting owner, from the backup's own output | see §3(b) |
| Cooperative withdraw fee | at withdraw time | the withdrawing owner | caller-chosen rate, else `min(SE quote, max_fee_rate)` (`clients/libs/rust/src/withdraw.rs:67-68`) |

Consequences of "fees are frozen at signing time":

- **No RBF, ever.** Branch and backup txs are fully-signed MuSig2 2-of-2 spends; re-signing at a
  higher fee needs the SE — at which point you would just withdraw cooperatively. Documented as a
  known limitation in [SPEC.md §14](../SPEC.md#14-known-limitations-adversarial-review).
- **CPFP is possible but not automated.** The backup pays the user's *solely-owned* P2TR key
  (`get_user_backup_address`), so a child spend at any feerate can bump the whole package — but
  only once the backup's locktime has passed and it is in the mempool. The SDK does not build the
  child; the user's wallet must.
- **Before backup maturity there is no fee lever on the branch.** Branch txs sit in the mempool at
  their pre-committed reserve rate (~1.9–12.9 sat/vB effective, see §5). Eviction is recoverable —
  `unilateral_exit` is re-callable (REQ-25) — but if the branch is being actively **raced**
  (`ExitBranchConflict`, audit H1 fix, `wallet.rs:551-557`), the only fast lever is a cooperative
  SE co-sign at market rate, which requires the SE to be alive and honest.

## 2. Onboarding (entering)

### 2a. On-chain deposit

The user funds a P2TR aggregate address from their own wallet; the funding-tx size depends on the
wallet's input type (all rows except P2TR *model* estimates from standard input/output weights):

| Funding tx shape | vB | @1 | @2 | @5 | @10 | @30 | @100 | @300 sat/vB |
|---|---|---|---|---|---|---|---|---|
| P2TR 1-in-1-out (sweep) | 111 | 111 | 222 | 555 | 1,110 | 3,330 | 11,100 | 33,300 |
| P2TR 1-in-2-out (with change) | 154 | 154 | 308 | 770 | 1,540 | 4,620 | 15,400 | 46,200 |
| P2WPKH 1-in-2-out (*model*) | ~153 | 153 | 306 | 765 | 1,530 | 4,590 | 15,300 | 45,900 |
| P2WPKH 2-in-2-out (*model*) | ~221 | 221 | 442 | 1,105 | 2,210 | 6,630 | 22,100 | 66,300 |
| P2PKH legacy 1-in-2-out (*model*) | ~236 | 236 | 472 | 1,180 | 2,360 | 7,080 | 23,600 | 70,800 |

(USD @$100k: the 154 vB row spans $0.15 → $46.20 across 1→300 sat/vB.)

Each deposit also consumes a **deposit token** from the SE's token server (anti-spam). Its price
is SE-configured, not protocol-fixed; when payment is required the SDK surfaces
`TokenPaymentRequired { fee_sats, deposit_address }` (ERR-6, `types.rs:193-199`). Treat it as a
per-deposit constant `T` sats in any TCO below.

### 2b. Off-chain receive — zero on-chain cost

Receiving a coin or sub-coin off-chain (`SDK_E2E=16`, zero-footprint onboarding) puts **nothing
on-chain**. The receiver holds, locally, everything needed to exit without the SE:

- the full signed **exit branch** (`branch-<id>`, root-first) down from an on-chain outpoint,
- the **latest backup tx** (lowest locktime in the ladder — theirs beats every prior owner's),
- the **ancestor id list** (`parents-<id>`) used to verify every structural parent terminal at
  the SE — subject to the blind-SE substitution caveat: the ids are not cryptographically bound
  to the branch outpoints ([SPEC.md §14](../SPEC.md#14-known-limitations-adversarial-review)).

The deferred liability is the future exit: `112 + 155·depth` vB at whatever feerate then
prevails, **plus** the wait — a freshly received sub-coin's backup matures ≈ `initlock` blocks
after the *split* that minted it (fresh ladder, `transfer.rs:438-449`), i.e. up to ~6.9 days
(profile A) / ~69 days (B) if you exit immediately after receiving (§6).

**Peer comparison:** Spark onboarding also needs an L1 deposit (node+refund pre-signing) or an
SSP swap; Ark needs joining a round tx (your vout in a shared on-chain tx, amortized but
mandatory-per-round); LN needs a channel open (~150–300 vB, *model*) plus inbound liquidity.
Off-chain receive here and in Spark is the only genuinely 0-vB entry; Ark re-enters on-chain
every round; LN amortizes over the channel's life. See §8.

## 3. Offboarding (leaving)

### 3a. Cooperative withdraw

One SE co-signed on-chain tx **per coin**, no timelock wait (`wallet.rs:473-522`). Sub-coins
materialize their branch first (adds the branch vB — same figures as §3b, minus the wait).

| Withdraw tx | vB | @1 | @2 | @5 | @10 | @30 | @100 | @300 sat/vB |
|---|---|---|---|---|---|---|---|---|
| lower anchor | 111 | 111 | 222 | 555 | 1,110 | 3,330 | 11,100 | 33,300 |
| upper anchor | 140 | 140 | 280 | 700 | 1,400 | 4,200 | 14,000 | 42,000 |

**N coins cost N txs today.** Plain-BTC combine is *not* a shipped SDK operation — a colored
combine exists at lib level only (`create_colored_combine_tx`,
`clients/libs/rust/src/rgb.rs:330`, exercised by rgb02/05/08), so "combine N coins, withdraw
once" is a future optimization, not a current path.

### 3b. Unilateral exit — full pricing

`fee_at(r)` for the whole chain (what the package must carry to confirm at market rate r).
Depths 0–4 are **measured**; the depth-8 row is linear extrapolation (the per-hop delta measured
by sdk26 is exactly 155 vB at every level: deltas `[155, 155, 155, 155]`).

| Depth | Txs | Total vB | @1 | @2 | @5 | @10 | @30 | @100 | @300 | Source |
|---|---|---|---|---|---|---|---|---|---|---|
| 0 (flat) | 1 | 112 | 112 | 224 | 560 | 1,120 | 3,360 | 11,200 | 33,600 | measured (`types.rs`, sdk07) |
| 1 | 2 | 267 | 267 | 534 | 1,335 | 2,670 | 8,010 | 26,700 | 80,100 | measured (sdk07, sdk26) |
| 2 | 3 | 422 | 422 | 844 | 2,110 | 4,220 | 12,660 | 42,200 | 126,600 | **measured (sdk26)** |
| 4 | 5 | 732 | 732 | 1,464 | 3,660 | 7,320 | 21,960 | 73,200 | 219,600 | **measured (sdk26**, incl. a full on-chain exit: 4-tx branch broadcast instantly, backup after 997-block wait**)** |
| 8 | 9 | 1,352 | 1,352 | 2,704 | 6,760 | 13,520 | 40,560 | 135,200 | 405,600 | model (linear; exact per sdk26's constant 155 vB/hop) |

Width is cheaper than depth: sdk26's 3-recipient fan-out minted 3 pieces + change in ONE
co-signed 241-vB split (4 outputs) — ~60 vB of branch per piece instead of 155, and every piece
sits at depth 1.

(USD @$100k: depth-1 exit spans $0.27 → $80.10 across 1→300 sat/vB.)

Of this, the **pre-committed** part is `Σ reserves + backup fee` — between `300·N + 112` and
`2000·N + ceil(112·r_b)` sats, where `r_b` is the backup co-sign rate, capped at `max_fee_rate`
(default **1.0 sat/vB** ⇒ 112 sats). The *branch* txs carry an effective **~1.9–12.9 sat/vB**
(§5); the whole package, including the 112-vB backup, sits lower — **~1.5–7.9 sat/vB at depth 1**
(412/267 to 2,112/267 at the default backup fee) — converging to the branch rate with depth. When
the market is below the package rate, exit costs *nothing at broadcast time*; above it, the
shortfall is CPFP (below).

**The wait dimension.** Fees buy confirmation; the *backup* additionally waits for its locktime:

| Situation | wait_blocks | Profile A (1000/10) | Profile B (10000/100) |
|---|---|---|---|
| Flat coin, day 0, no hops | `initlock` | 1,000 blk ≈ 6.9 d | 10,000 blk ≈ 69.4 d |
| Flat coin, day 6, 50 hops (taken within the first ~3.5 d — the pace bound, §4a) | `initlock − 50·interval − 864` | 1000−500−864 → **0 blk (ready)** | 10000−5000−864 = 4,136 blk ≈ 28.7 d |
| Fresh sub-coin, exit right after receiving | ≈ `initlock` (fresh ladder from `h_split`) | ≈ 6.9 d | ≈ 69.4 d |

The branch is locktime-free and confirms immediately (INV-4); only the *sweep to your key* waits.
A just-received sub-coin therefore shows value on-chain within a block but is not spendable-as-BTC
for up to ~7 days (A) / ~69 days (B). Wait is bounded above by `initlock` always, and shrinks
`interval` per hop already taken on the coin's *own* ladder plus ~144 blocks per elapsed day.

**Worst-case fee spike (reserve exhausted).** A flat backup was co-signed at 2 sat/vB
(fee 224 sats on 112 vB) — an above-default premise, stated explicitly: default-config backups
are capped at `max_fee_rate = 1.0` sat/vB, i.e. **112 sats** (`client_config.rs:70`), so 2 sat/vB
models an operator that raised the cap. Market is now 100 sat/vB. RBF is impossible; the rescue
is a CPFP child from the backup's own output (~111 vB, 1-in-1-out P2TR, *model* — the measured
**112 vB** backup figure is the fee-calc constant `BACKUP_TX_SIZE` with ceiling rounding; modeled
1-in-1-out P2TR rows throughout use the standard 111 vB weight):

```
r_child = (r_market·(v_parent + v_child) − fee_parent) / v_child
        = (100·(112 + 111) − 224) / 111  =  22,076 / 111  ≈  199 sat/vB
```

The child pays **22,076 sats** (~$22) — about **2× the market rate** on its own vbytes, because it
drags the underpaying parent. For a deeper chain the child must lift the whole package: depth-2 at
the reserve floor (prepaid 300·2 + 224 = 824 sats) needs
`ceil((100·(422+111) − 824)/111) ≈ 473 sat/vB` from the child — **52,476 sats**. On a 50,000-sat
coin this is not merely >100% of value — it is a **cliff**: the backup output holds only
~49,776 sats, less than the required child fee, so the CPFP rescue is *infeasible*, not just
expensive, until fees fall. On a 0.01 BTC coin it is ~5%. Fee spikes punish small coins
disproportionately (§5) — and remember the child can only be attached *after* the backup matures.

**Branch confirmation before the deadline (fee spike × deadline).** For a sub-coin the branch
must not merely be broadcast but **confirmed** before `exit_deadline_block` (§9, [17]). Until the
exiting owner's own backup matures — which is never earlier than the root deadline, since
`h_split ≥ H_deposit` — every output in the chain is a 2-of-2 aggregate, so **no honest party has
a fee lever on the branch**: it rides at its pre-committed ~1.9–12.9 sat/vB, full stop. The
asymmetry to price in: a *matured hostile stale backup* pays its holder's own address and **is**
CPFP-bumpable by them — past the deadline the attacker holds the fee lever the honest exiter
lacks. Three regimes for "how many blocks before the deadline must I broadcast at feerate X":

- **Market ≤ reserve rate:** the branch confirms promptly, and before the deadline it is
  *uncontested* — the stale backup is non-final and cannot even enter the mempool. `SDK_E2E=14`
  demonstrates exactly this (deadline known up front, normal fee, no race premium) and exits with
  a 40-block safety margin.
- **Market above the reserve rate:** the branch sits unconfirmed until the market dips below its
  rate, while the deadline keeps approaching. Broadcast **early**: the required lead time is the
  expected time-to-dip below ~1.9–12.9 sat/vB, which in a sustained spike can exceed the whole
  `initlock` window.
- **Mempool min-relay above the reserve rate:** the branch is refused relay or evicted outright;
  re-broadcast is free (`unilateral_exit` is re-callable, REQ-25) but confirmation waits for the
  purge to end.

Miss the deadline through all three and the fallback is the ladder race (sdk13/sdk14) under the
CPFP asymmetry above — where, per the worked example, a small coin may be unable to fund the
rescue at all.

## 4. Costs over time

### 4a. Ladder capacity: hops vs wall clock

Each transfer burns `interval` blocks of ladder headroom; the chain tip burns ~144 blocks/day
regardless — and the two burns are **additive**, not whichever-first: receiver validation rejects
any transfer whose newest backup locktime is at or below the tip (`LocktimeTooLow`,
`lib/src/transfer/receiver.rs:461`), so at a sustained hop rate ρ hops/day the off-chain
(transferable) life is exactly

```
life = initlock / (144 + interval·ρ)   days
```

The last two columns below are the *limits* of that formula (ρ→0 and ρ→∞); in the mid-range use
the formula — at the break-even rate itself, life is `initlock/288`: **≈3.5 d (A) / ≈34.7 d (B)**,
half the idle horizon.

| Profile | Burn per hop | Burn per day | Break-even hop rate | Limit ρ→0 (time-dominated) | Limit ρ→∞ (hop-dominated) |
|---|---|---|---|---|---|
| A 1000/10 | 10 blk | ~144 blk | **14.4 hops/day** | life → 6.9 days | life → 100/ρ days |
| B 10000/100 | 100 blk | ~144 blk | **1.44 hops/day** | life → 69.4 days | life → 100/ρ days |

Most real coins are time-dominated: the horizon (initlock/144 days) binds long before the 100-hop
budget does. Sub-coins get a *fresh* leaf ladder at split time (`h_split + initlock`,
`transfer.rs:438-449`) — depth does not consume interval — but the **tree's** off-chain life stays
bounded by the root deadline `H_deposit + initlock` (`wallet.rs:634-666`, audit [10]); see the
open-item caveat in §9.

### 4b. Price of each lifetime-extension option

There is **no ladder-renewal endpoint** (verified: none exists). The options and their prices:

| Option | On-chain now | On-chain later | Locked/burned now | What it actually extends |
|---|---|---|---|---|
| **Self-split** | 0 vB | +155 vB on the future exit (branch grows one hop) | fee reserve 300–2,000 sats (becomes the branch fee) | the **leaf ladder** only (fresh 100 hops); the root deadline `H_deposit+initlock` is *not* moved |
| **Withdraw + redeposit** | 2 txs ≈ 294 vB (140 + 154) + deposit token `T` | — | — | everything: brand-new coin, new ladder, new root |
| **Branch materialization** | branch txs (155·depth vB, fee = pre-paid reserves) | — | — | converts the sub-coin to a flat coin; the governing deadline becomes the leaf's own ladder (`h_split + initlock`) |

Materialization is the cheap renewal (155 vB per horizon vs 294 vB + token), with two catches:
its feerate was frozen at split time (reserve ⇒ ~1.9–12.9 sat/vB — slow to confirm in a high-fee
regime), and a "self-split then materialize each horizon" rolling loop is a *derived strategy*,
not a tested SDK flow (*model*).

### 4c. Worked one-year TCO

Assumptions: funding 154 vB, withdraw 140 vB, deposit token `T` sats, USD @$100k/BTC.

**(i) Merchant coin — 50 hops, then cooperative exit.** Hops are free; total on-chain =
deposit 154 + withdraw 140 = **294 vB + T**. Per payment: **~5.9 vB**.
At 1 sat/vB: 294 sats total (~$0.29), **~6 sats/payment**; at 10 sat/vB: 2,940 sats (~$2.94),
**~59 sats/payment** (~$0.059). Constraint — §4a's *additive* burn, enforced by receiver
validation rejecting `locktime ≤ tip`: hop k must satisfy `144·t_k < initlock − k·interval`, so
all 50 hops must complete within `(initlock − 50·interval)/144` days — **~3.5 days on profile A
(≥ ~14.4 hops/day)**, ~34.7 days on B (≥ ~1.44 hops/day). (For k = capacity/2 the required rate
is exactly §4a's break-even rate.) A profile-A merchant coin must therefore turn over in ~3.5
days, not 6.9 — a real constraint for slow tills. The failure mode is graceful: a coin that
falls behind pace stops being transferable (receivers reject it) but remains cooperatively
withdrawable at any time — the roll just amortizes over fewer free hops, e.g. stalling at hop 33
(the 7.2 hops/day pace) costs ~8.9 vB/payment instead of ~5.9.

**(ii) Saver — must roll every horizon (withdraw + redeposit).** Rolls/year =
`ceil(365.25 / (initlock/144)) − 1`:

| Profile | Horizon | Rolls/yr | On-chain vB/yr | @1 sat/vB | @10 sat/vB | Tokens |
|---|---|---|---|---|---|---|
| A 1000/10 | 6.9 d | **52** | 154 + 52·294 = 15,442 | 15,442 sats (~$15.44) | 154,420 sats (~$154) | 53·T |
| B 10000/100 | 69.4 d | **5** | 154 + 5·294 = 1,624 | 1,624 sats (~$1.62) | 16,240 sats (~$16.24) | 6·T |

The saver profile is where `initlock` matters most: profile B is ~10× cheaper per year to hold,
at the price of a ~10× longer worst-case exit wait (§7). The self-split+materialize loop (*model*,
§4b) would cut the A-profile saver to ~52·155 ≈ 8,060 vB/yr with fees pre-capped at
300–2,000/roll, no tokens — untested as a loop.

**(iii) Exact-amount payment via split.** The headline granularity feature
([learn/invalidation.md](../learn/invalidation.md): exact amounts are a native off-chain
operation) is **not free the way whole-coin hops are**. End-to-end, one split-based payment
costs:

- **Sender:** the fee reserve `clamp(parent_sats/100, 300, 2000)` sats, deducted from the parent —
  and *burned*, not escrowed: the branch tx carrying it is broadcast on every exit path,
  cooperative or unilateral (`wallet.rs:473-522` materializes the branch before any withdraw).
  Regressive below ~30k-sat parents (§5: 30% of a 1,000-sat parent, 0.002% of 1 BTC).
- **Receiver:** **+155 vB / +1 depth** inherited on the future exit, plus a fresh ≈`initlock`
  unilateral wait on the new leaf ladder (§6).

Integrator rule of thumb: a whole-coin hop is genuinely 0-cost; a split is a 300–2,000-sat +
155-vB event. Prefer coin selection over splitting whenever the wallet holds a coin of (close
to) the right size.

## 5. Size effects

**Fee reserve as % of coin value** (`clamp(parent/100, 300, 2000)`, `transfer.rs:596-599`):

| Parent size | Reserve | % of value |
|---|---|---|
| 1,000 sats | 300 | **30%** |
| 10,000 sats | 300 | 3% |
| 30,000 sats | 300 | 1% |
| 100,000 sats | 1,000 | 1% |
| 200,000 sats (0.002 BTC) | 2,000 | 1% |
| 1,000,000 sats (0.01 BTC) | 2,000 | 0.2% |
| 10,000,000 sats (0.1 BTC) | 2,000 | 0.02% |
| 100,000,000 sats (1 BTC) | 2,000 | **0.002%** |

The clamp makes splitting regressive below ~30k sats (flat 300-sat floor) and progressive-to-flat
above 200k (2,000-sat cap ⇒ effective branch feerate 2000/155 ≈ 12.9 sat/vB; at the floor,
300/155 ≈ 1.9 sat/vB — the origin of the ~1.9–12.9 pre-paid range in §3b).

**Floors and caps — why the practical limits are what they are:**

- **Minimum splittable parent ≈ 960 sats:** every split output must clear dust
  (`piece ≥ 330`, `change ≥ 330`, audit [9]) and the reserve floor is 300, so
  `330 + 330 + 300 = 960`. At 1,000 sats the piece can only be 330–370 sats.
- **Minimum exit-viable coin = `330 + ceil(112·r_backup)`:** the backup's output must clear dust
  after its fee (`lib/src/transaction.rs:123-131`, else `FeeTooLow`). At the default
  `max_fee_rate = 1.0` that is **442 sats**; a coin co-signed at 5 sat/vB needs ≥ 890.
- **Micro-pieces are exit-fragile:** a 1,000-sat sub-coin's depth-1 exit costs 267–534 sats at
  just 1–2 sat/vB — 27–53% of value; any fee spike (§3b) wipes it out. Economic floor for pieces
  intended to be *exitable alone* is ~10k sats at low fees, higher in a high-fee regime.
- **Maximum coin ≈ 42.9 BTC (u32), not uniformly guarded:** amounts are booked as `u32` sats
  ([SPEC.md §14](../SPEC.md#14-known-limitations-adversarial-review)); several paths cast with
  `as u32` and would silently truncate above 4,294,967,295 sats
  (`clients/libs/rust/src/coin_status.rs:229`), while the split path errors via `u32::try_from`
  (`transfer.rs:421`). Stay well below.

## 6. Time-to-money (UX timing)

Wall-clock time until value is spendable BTC at the destination, assuming the paid feerate clears
in the next block (~10 min); add mempool time when paying below market. Fees per §2–3 anchors.

| Flow | Profile A (1000/10) | Profile B (10000/100) | On-chain vB | Fee @1 / @10 / @30 sat/vB |
|---|---|---|---|---|
| On-chain deposit → spendable coin | ~10 min + watcher poll (config confirmations) | same | 111–154 | 154 / 1,540 / 4,620 (154 vB) |
| Off-chain send or receive | seconds | seconds | 0 | 0 |
| Cooperative withdraw | ~10 min | ~10 min | 111–140 | 140 / 1,400 / 4,200 (140 vB) |
| Unilateral exit, flat coin, day 0 | **6.9 d** + 10 min | **69.4 d** + 10 min | 112 | 112 / 1,120 / 3,360 |
| Unilateral exit, flat coin, day 6 (no hops) | ~22.6 h (136 blk) | ~63.4 d (9,136 blk) | 112 | 112 / 1,120 / 3,360 |
| Unilateral exit, freshly received depth-1 sub-coin | branch ~10 min; sweep **≈6.9 d** | branch ~10 min; sweep **≈69.4 d** | 267 | 267 / 2,670 / 8,010 |

The asymmetry to design UX around: cooperative paths are minutes at any depth; unilateral paths
are *days-to-months* dominated by `wait_blocks`, not fees. A wallet should surface
`estimate_exit_cost().wait_blocks` (and `exit_deadline_block` — the *safety* deadline, a different
number) whenever the SE looks unhealthy.

## 7. Parameter sensitivity (initlock / interval)

All rows keep the 100-hop ratio. "Forced-unilateral exposure" = time locked out of funds if a
coin is forced down the unilateral path on day 0 — e.g. the SE dies, or (historically) the audit-
[15] replay brick, closed in AUDIT UPDATE 3 by single-use endpoint-bound auth on
`set_spend_budget`/`withdraw/complete` — the owner waits the full fresh ladder.

| initlock/interval | Horizon (initlock/144) | Hop capacity | Exclusive exit window (interval) | Day-0 forced-unilateral exposure |
|---|---|---|---|---|
| 500 / 5 | 3.5 d | 100 | 5 blk ≈ 50 min | 3.5 d |
| 1000 / 10 (deployed) | 6.9 d | 100 | 10 blk ≈ 100 min | 6.9 d |
| 2000 / 20 | 13.9 d | 100 | 20 blk ≈ 3.3 h | 13.9 d |
| 10000 / 100 (default) | 69.4 d | 100 | 100 blk ≈ 16.7 h | 69.4 d |

The trade-off, in one line each:

- **initlock up** ⇒ longer holding horizon, ~linearly fewer saver rolls/year (§4c) — but equally
  longer worst-case unilateral wait, fresh-sub-coin wait (§6) and forced-unilateral exposure.
- **interval up** (at fixed ratio) ⇒ a wider exclusive window in which *only* the current owner's
  backup is valid — 50 minutes (500/5) is uncomfortably tight against a fee spike + full mempool;
  16.7 h (10000/100) comfortably covers confirmation variance — but each hop then burns more
  headroom, so hop-dominated (high-frequency) coins die sooner in wall-clock terms.
- The deployed 1000/10 favours payments (cheap horizon management is irrelevant for coins that
  turn over in days); 10000/100 favours savings; nothing in the code prevents running SEs at both
  points for different products.

## 8. Cross-protocol comparison

Quantitative where repo sources allow ([protocol-notes.md](protocol-notes.md),
[learn/invalidation.md](../learn/invalidation.md)); *model* otherwise.

| System | On-chain per payment | Amortized entry+exit | Unilateral exit | Exit wait / renewal clock |
|---|---|---|---|---|
| **This system** | **0 vB** per hop | (deposit 154 + withdraw 140) / N payments ≈ 294/N vB | 1 tx flat (112 vB); depth-N: N+1 txs, 112+155N vB, fees §3b | wait ≤ initlock (6.9 d A / 69.4 d B); renewal = on-chain roll (no renew endpoint), 155–294 vB per horizon (§4b) |
| **Spark** | 0 per transfer | L1 deposit pre-signs node+refund; coop exit needs SSP connector tx (extra on-chain tx on SSP's side) | node-tx chain to the leaf (tree depth) + refund tx; anchor output per node adds vB (*model*: sizes comparable per-tx, no measured figure in repo) | relative timelock 2000, −100/transfer, **renew_leaf at ≤300 needs the SO** — off-chain renewal (0 vB) but SO-liveness-dependent (`protocol-notes.md:17-19`) |
| **Ark / Second** | 0 within a round | must join a shared round tx **every round** — amortized share of one on-chain tx per round, mandatory | broadcast your branch of the round tree (~log₂(participants) txs, *model*) | miss the round/expiry ⇒ funds sweep to the server — the only design here where lateness is loss, not delay |
| **Lightning** | 0 per HTLC | channel open + close ≈ 2 txs, ~300–450 vB total (*model*), amortized over the channel's payments | force-close tx + CSV `to_self_delay` (~1–14 d typical, *model*) + per-HTLC sweeps | no expiry; but capacity is pairwise and inbound liquidity is a separate cost |
| **On-chain baseline** | 111–154 vB *every* payment | — | — | none |

Reading the table: this design and Spark have the same steady-state shape (0 vB per transfer,
linear-in-depth unilateral exit). The economic difference is the renewal clock — Spark renews
off-chain for free but *must* reach its operators within 17 transfers of headroom (2000 → 300 at
−100/hop); here renewal costs 155–294 vB on-chain per horizon but never needs the SE for exit,
and — unlike
Ark — missing every deadline degrades to a timelocked wait plus a race (sdk13/sdk14), never a
sweep. Splitting grants children a fresh timelock budget in *both* systems — here a fresh
`h_split + initlock` leaf ladder, in Spark a fresh 2000 behind a zero-timelock split node
([protocol-notes.md](protocol-notes.md)) — so that is not a differentiator; the split economics
differ only in who pre-pays the branch fee (here: the splitter's 300–2,000-sat reserve).

## 9. Open items that move these numbers

Honesty section — status per [AUDIT-2026-07.md](../AUDIT-2026-07.md) remediation UPDATE 2 (all
11 HIGH findings fixed + verified; mainnet still gated on the SGX rebuild + re-audit):

- **[17] (half-closed): the true safety deadline can be earlier than the implemented one.** The
  implemented `exit_deadline_block = H_deposit + initlock` (`wallet.rs:634-666`) is exact for
  unsplit-fresh parents but **late by k·interval** when the parent had k transfer hops before the
  split — the splitter's own retained backup matures at `H_deposit + initlock − k·interval`. An
  online receiver is unaffected (broadcast the locktime-free branch immediately and the point is
  moot). Batch 5 shipped the auto-exit half: `auto_exit_due(margin_blocks)` force-broadcasts the
  branch within a margin of the deadline; the conveyed-ancestor-locktimes half remains open, so
  the §4 "root deadline" figures are upper bounds and the margin must absorb `k·interval`.
  The exactness domain and the binding mitigation are stated normatively in
  [INVALIDATION-SPEC.md §6.2](../INVALIDATION-SPEC.md) (IVL-INV-10 / IVL-REQ-16: eager
  materialization, or a conservative `M·interval` margin via `auto_exit_due`).
- **[15] (closed, AUDIT UPDATE 3): griefing brick** — the replay that could force a coin down
  the unilateral path (full `initlock` wait, no theft) is no longer possible on the irreversible
  endpoints; §7's forced-unilateral exposure column survives as the price of an SE failure, not
  of an attack.
- **sdk26 (`SDK_E2E=26`, `sdk26_invalidation_scale`): measurements folded into §3b** (2026-07-07
  run): per-hop branch delta exactly 155 vB at depths 1–4; depth-4 totals 732 vB; 3-wide fan-out
  split 241 vB; full depth-4 unilateral exit executed on regtest (branch instant, backup after
  997-block wait, funds at the owner's backup address). Combine-hop sizes (§1) remain unmeasured.
- **Batching (future):** plain-BTC combine-then-withdraw would turn §3a's N-tx cost into ~1 tx;
  today it does not exist as an SDK operation.
