# Invalidation economics — pricing entry, exit, size and time

Companion to [learn/invalidation.md](../learn/invalidation.md) (mechanism comparison),
[learn/invalidation-deep-dive.md](../learn/invalidation-deep-dive.md) (long-form explainer),
[learn/exits.md](../learn/exits.md) (exit flows), [SPEC.md](../SPEC.md) (normative REQ/INV/ERR)
and [INVALIDATION-SPEC.md](../INVALIDATION-SPEC.md) (normative IVL-REQ/IVL-INV). Partial-amounts
pricing (split bounds, token packaging, fragmentation) **extends** this page in
[granularity-economics.md](granularity-economics.md).
This page prices the invalidation design: what it costs to enter, to hold, to transact, and to
leave — cooperatively or unilaterally — and how those costs scale with feerate, tree depth, coin
size and time.

> **Scope — one protocol, two coin shapes.** There is now ONE protocol: `claim()` establishes a
> TES-R ladder (trigger `T` → extension `X_m` → state `S`, relative CSV, all un-broadcast) for every
> fresh confirmed ROOT coin, unconditionally. Two coin *shapes* coexist under it, both current:
>
> - **LADDERED** — every plain deposit. Priced elsewhere: idle coins never age (relative timelocks
>   only start counting once `T` confirms, and `T` is never broadcast), **0 vB/yr rent**, renewal is
>   pure off-chain. See [PROTOCOL.md §5.9 (exit costs)](../PROTOCOL.md) and
>   [§7 (footprint economics)](../PROTOCOL.md).
> - **UN-LADDERED** — an RGB **carrier** is deliberately never laddered (a plain tier spend would
>   destroy the allocation — terminal-freeze, [PROTOCOL.md §5.10](../PROTOCOL.md); asserted by
>   `sdk52`), and a split sub-coin whose funding is un-broadcast cannot root a trigger [B0]. Both
>   keep the signed-once absolute-locktime backup and transfer by backup-chain handover. This path
>   is load-bearing for RGB tokens, not dead code.
>
> **Everything on this page prices the UN-LADDERED shape** — the absolute-locktime backup chain
> (`initlock − k·interval`) plus split *branch* txs. Sections whose conclusion inverts for a
> laddered coin are flagged inline. Do not read "un-laddered" as "legacy": it is the shape RGB
> carriers and un-broadcast-funded sub-coins have today.

**Measured anchors** (everything below derives from these; anything else is labelled *model*):

| Anchor | Value | Source |
|---|---|---|
| Backup tx vsize (1-in-1-out P2TR keyspend) | **112 vB** | `lib/src/transaction.rs:116` (`BACKUP_TX_SIZE`) — a live code constant. It was measured on-chain by `SDK_E2E=7`, which has since been **retired**; no live E2E re-measures it (the `types.rs:227` `exit_cost_math` constants are unit-test illustrations, not measurements) |
| Split branch tx vsize (1-in-2-out P2TR) | **155 vB** | measured by the retired `SDK_E2E=7` / `SDK_E2E=26` runs (figures below are those recordings). The branch path itself is still exercised end-to-end on-chain by **`SDK_E2E=39`** (depth-2 colored branch exit, root-first broadcast, allocation preserved) — which asserts the exit *works*, not its vsize |
| Depth-1 exit total | **267 vB** | 155 + 112, from the same retired-run recordings |
| Cooperative withdraw tx | **~111–140 vB** | measured range, 1-in-1-out P2TR spend |
| Split fee reserve | **`clamp(parent_sats/100, 300, 2000)` sats** | `clients/libs/rust-sdk/src/transfer.rs:1416` (`split_fee_reserve`, INV-10) |
| Dust floor on every split output & backup output | **330 sats** | `lib/src/transaction.rs:120` (`DUST_LIMIT`), audit [9] |
| Exit fee formula | `fee = ceil(total_vbytes × rate)` | `types.rs:196-201` (`fee_sats_at`, INV-17) |

⚠️ **Coverage note.** The vsize anchors are *historical recordings*, not live assertions: the E2Es
that measured them (`sdk07`, `sdk26`) were retired with the protocol unification and nothing
replaced their measurement role. The constants they measured are still in the code and still govern
the un-laddered path, and the path is still driven end-to-end (`sdk39`, `sdk31`, `sdk55`), but if
you need a *current* number, re-measure. The laddered analogues are live and measured:
`TIER_VBYTES = 124` (`lib/src/tesr.rs:44`), `P2A_VALUE = 240`, exercised by `sdk50`.

**Profiles.** A = deployed `initlock 1000 / interval 10` (`server/Settings.toml:2-3`);
B = code default `10000 / 100` (`server/src/server_config.rs:82-83`). Both give
`initlock/interval = 100` transfer hops. Wall clock: ~144 blocks/day.

**Units.** Sats everywhere. USD figures, where given, are parametric at an illustrative
**$100,000/BTC ⇒ 1 sat = $0.001** — scale linearly for any other price.

---

## 1. The exit cost model

A unilateral exit of an un-laddered depth-N sub-coin broadcasts **N branch txs + 1 backup tx**; a
flat un-laddered coin broadcasts the backup only (`wallet.rs:1028`, `unilateral_exit`). The same
entry point routes a **laddered** coin down its tier chain instead (trigger → extension → state, as
each relative CSV matures) — 3 pre-signed txs ≈ 372 vB, no absolute-locktime backup broadcast at
all; that shape is priced in [PROTOCOL.md §5.9](../PROTOCOL.md) and driven by `sdk50`.

```
total_vbytes(N) = 112 + Σ branch_vsize_i          ≈ 112 + 155·N   (split-only chain)
fee_at(r)       = ceil(total_vbytes × r)          (types.rs:196-201, INV-17)
wait_blocks     = max(0, backup_locktime − tip)   (wallet.rs:961)
backup_locktime = h_cosign + initlock − interval·k   (lib/src/transaction.rs:153-167)
```

Combine hops are larger — roughly +57.5 vB per extra P2TR input (*model*; the per-depth split-only
figures come from the retired `SDK_E2E=26` run; a single 2-input colored combine is measured **live**
by `sdk31` (~255 vB, `sdk31_token_combine.rs:204`), but multi-hop combine chains remain unmeasured).
`estimate_exit_cost` (`wallet.rs:929`) does not model anything: it decodes the actual pre-signed txs
and sums their real vsizes.

**Who pre-pays what, and when:**

| Cost component | Fixed when | Paid by | Amount |
|---|---|---|---|
| Branch tx *i* fee | at split time | the **splitter** (deducted from parent: `change = parent − piece − reserve`, INV-10) | `clamp(parent_i/100, 300, 2000)` sats |
| Backup tx fee | at co-sign time (deposit, and each transfer hop re-signs a fresh backup) | the coin itself (deducted from the swept amount) | `ceil(112 × min(SE-quoted rate, client max_fee_rate))`; `max_fee_rate` defaults to **1.0 sat/vB** (`clients/libs/rust/src/client_config.rs:70`, overridable in client settings) |
| CPFP top-up (fee spike) | at exit time | the exiting owner, from the backup's own output | see §3(b) |
| Cooperative withdraw fee | at withdraw time | the withdrawing owner | caller-chosen rate, else `min(SE quote, max_fee_rate)` (`clients/libs/rust/src/withdraw.rs:67-68`) |

(The laddered shape freezes fees too, but differently: each tier bakes a **committed fee** at
`committed_fee_rate` (2 sat/vB default) so it relays standalone, and carries a **P2A anchor**
(240 sats, `lib/src/tesr.rs:38-44`) that lets *anyone* — owner, tower, operator — attach a live-rate
fee child during a spike. That is the fee lever the un-laddered branch below does not have.)

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
  (`ExitBranchConflict`, audit H1 fix, `wallet.rs:907`), the only fast lever is a cooperative
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
`TokenPaymentRequired { fee_sats, deposit_address }` (ERR-6, `types.rs:63-68`). Treat it as a
per-deposit constant `T` sats in any TCO below.

### 2b. Off-chain receive — zero on-chain cost

Receiving a coin or sub-coin off-chain (`SDK_E2E=16`, zero-footprint onboarding) puts **nothing
on-chain**. For an un-laddered coin the receiver holds, locally, everything needed to exit without
the SE:

- the full signed **exit branch** (`branch-<id>`, root-first) down from an on-chain outpoint,
- the **latest backup tx** (lowest locktime in the ladder — theirs beats every prior owner's),
- the **ancestor id list** (`parents-<id>`) used to verify every structural parent terminal at
  the SE — subject to the blind-SE substitution caveat: the ids are not cryptographically bound
  to the branch outpoints ([SPEC.md §14](../SPEC.md#14-known-limitations-adversarial-review)).

The deferred liability is the future exit: `112 + 155·depth` vB at whatever feerate then
prevails, **plus** the wait — a freshly received sub-coin's backup matures ≈ `initlock` blocks
after the *split* that minted it (fresh ladder, `transfer.rs:1378-1389`), i.e. up to ~6.9 days
(profile A) / ~69 days (B) if you exit immediately after receiving (§6).

**Laddered receive is also 0 vB, and stays 0 vB across hops.** A received in-ladder split **child**
is first-class ([CHILDREN.md](../CHILDREN.md)): the claim completes the
standard SE key handover, so the receiver co-owns `A_child` (invariant across the rotation, which is
what keeps the pre-signed exit chain valid) and the sender is permanently locked out. The child can
then be paid onward off-chain **whole** (`child_retransfer`) or **split** (`child_in_ladder_pay`, a
depth-2 `ancestors` chain). The marginal cost of a hop is not block space at all: it is **exactly one
co-signature**, disclosing **exactly one** superseded state, which the receiver's census counts and
proves out-raced. `sdk60` drives alice→bob→carol with the funding outpoint unspent throughout;
`sdk17` covers a partial second hop. What the receiver holds locally is the tier bundle (extension +
state, plus its ancestors' disclosed sets), not a backup chain.

**Peer comparison:** Spark onboarding also needs an L1 deposit (node+refund pre-signing) or an
SSP swap; Ark needs joining a round tx (your vout in a shared on-chain tx, amortized but
mandatory-per-round); LN needs a channel open (~150–300 vB, *model*) plus inbound liquidity.
Off-chain receive here and in Spark is the only genuinely 0-vB entry; Ark re-enters on-chain
every round; LN amortizes over the channel's life. See §8.

## 3. Offboarding (leaving)

### 3a. Cooperative withdraw

One SE co-signed on-chain tx **per coin**, no timelock wait (`wallet.rs:804`). Sub-coins
materialize their branch first (adds the branch vB — same figures as §3b, minus the wait). This row
is shape-independent: a laddered coin's cooperative withdraw is also one fresh co-signed tx
(PROTOCOL.md §5.9) — the ladder is simply never broadcast.

| Withdraw tx | vB | @1 | @2 | @5 | @10 | @30 | @100 | @300 sat/vB |
|---|---|---|---|---|---|---|---|---|
| lower anchor | 111 | 111 | 222 | 555 | 1,110 | 3,330 | 11,100 | 33,300 |
| upper anchor | 140 | 140 | 280 | 700 | 1,400 | 4,200 | 14,000 | 42,000 |

**N plain-BTC coins cost N txs today.** A multi-carrier **colored** combine is now a shipped SDK
operation — `UtexoWallet::colored_combine_transfer` (`clients/libs/rust-sdk/src/tokens.rs:771`,
`SDK_E2E=31`) combines N carriers of an asset into one SE-co-signed colored combine tx (registered
via `register_combine_subcoins`, `transfer.rs:1199`), over the lib-level primitive
`create_colored_combine_tx` (`clients/libs/rust/src/rgb.rs:330`, exercised by rgb02/05/08).
**Plain-BTC** combine, however, is still *not* a shipped SDK operation, so "combine N BTC coins,
withdraw once" remains a future optimization, not a current path.

### 3b. Unilateral exit — full pricing

`fee_at(r)` for the whole chain (what the package must carry to confirm at market rate r).
Depths 0–4 were **measured** on the 2026-07-07 run of `sdk26` (since retired — see the coverage note
at the top); the depth-8 row is linear extrapolation (the per-hop delta measured then was exactly
155 vB at every level: deltas `[155, 155, 155, 155]`).

| Depth | Txs | Total vB | @1 | @2 | @5 | @10 | @30 | @100 | @300 | Source |
|---|---|---|---|---|---|---|---|---|---|---|
| 0 (flat) | 1 | 112 | 112 | 224 | 560 | 1,120 | 3,360 | 11,200 | 33,600 | `BACKUP_TX_SIZE` (live constant); recorded by the retired sdk07 |
| 1 | 2 | 267 | 267 | 534 | 1,335 | 2,670 | 8,010 | 26,700 | 80,100 | recorded (retired sdk07, sdk26) |
| 2 | 3 | 422 | 422 | 844 | 2,110 | 4,220 | 12,660 | 42,200 | 126,600 | recorded (retired sdk26); the depth-2 branch *exit* is still driven live by **sdk39** |
| 4 | 5 | 732 | 732 | 1,464 | 3,660 | 7,320 | 21,960 | 73,200 | 219,600 | recorded (retired sdk26, incl. a full on-chain exit: 4-tx branch broadcast instantly, backup after 997-block wait) |
| 8 | 9 | 1,352 | 1,352 | 2,704 | 6,760 | 13,520 | 40,560 | 135,200 | 405,600 | model (linear; exact per the recorded constant 155 vB/hop) |

Width is cheaper than depth: sdk26's 3-recipient fan-out minted 3 pieces + change in ONE
co-signed 241-vB split (4 outputs) — ~60 vB of branch per piece instead of 155, and every piece
sits at depth 1.

**Laddered contrast.** A laddered coin never grows this chain: its flat unilateral exit is 3
pre-signed tiers ≈ 372 vB (self-relaying via committed fees; 372–828 vB with all three P2A fee
children in a spike), and a depth-`d` in-ladder sub-coin is `3 + 2d` txs ≈ `124·(3+2d)` vB — because
a child hangs only an extension + a state off the split state `SP`, with no trigger of its own
(PROTOCOL.md §5.9). Depth costs *two* tiers per level there instead of one 155-vB branch tx, but the
wait is relative-CSV rather than absolute-locktime. Driven live by `sdk50` (flat) and `sdk58`
(in-ladder split, 11 adversarial cases).

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
  *uncontested* — the stale backup is non-final and cannot even enter the mempool. (The E2E that
  demonstrated exactly this — deadline known up front, normal fee, no race premium, 40-block safety
  margin — was `SDK_E2E=14`, now retired. The margin-driven force-exit it exercised is the
  `auto_exit_due(margin_blocks)` API, `wallet.rs:693`, still shipped and still the binding mitigation
  for [17]; the *defence-before-the-deadline* property is now carried by `sdk45` (a keyless tower
  drives the exit for an offline owner) and `sdk51` (the owner's current state, carrying the
  strictly-lowest CSV, matures first against a hostile trigger). Neither re-asserts the 40-block
  number.)
- **Market above the reserve rate:** the branch sits unconfirmed until the market dips below its
  rate, while the deadline keeps approaching. Broadcast **early**: the required lead time is the
  expected time-to-dip below ~1.9–12.9 sat/vB, which in a sustained spike can exceed the whole
  `initlock` window.
- **Mempool min-relay above the reserve rate:** the branch is refused relay or evicted outright;
  re-broadcast is free (`unilateral_exit` is re-callable, REQ-25) but confirmation waits for the
  purge to end.

Miss the deadline through all three and the fallback is the ladder race under the CPFP asymmetry
above — where, per the worked example, a small coin may be unable to fund the rescue at all. (The
race was covered by `sdk13`/`sdk14`, both retired. Live coverage of "a stale state loses" is
`sdk40` PART 2 — a stale ladder dies at consensus and griefing collapses to a priced nuisance —
plus `sdk51` (watchtower defends against a hostile trigger) and `sdk45` (keyless tower). Those
exercise the *laddered* race. On the un-laddered path the live defence is upstream of the race:
`sdk55` rejects a **padded or inverted** backup chain at receiver validation (INV-5,
`ladder_decrements_by_interval`), which is what makes "the current owner's backup matures first"
true at all. No live E2E races two absolute-locktime backups on-chain, so the un-laddered race
*timing* economics here are model, not measurement.)

## 4. Costs over time

### 4a. Ladder capacity: hops vs wall clock

> **Un-laddered only.** Everything in §4a–§4c is the *absolute*-locktime clock: the tip races the
> backup because the backup names a block height. A **laddered** coin has no such clock — BIP-68
> relative timelocks only start counting once the parent tier confirms, and `T` is never broadcast,
> so an **idle laddered coin never ages** (`sdk30` asserts a laddered coin is still `CONFIRMED`
> after 300 idle blocks; `sdk51` asserts the watchtower pass is a no-op while it sits). Read §4a–§4c
> as the cost of holding an RGB carrier or an un-broadcast-funded sub-coin, not of holding BTC.

Each transfer burns `interval` blocks of ladder headroom; the chain tip burns ~144 blocks/day
regardless — and the two burns are **additive**, not whichever-first: receiver validation rejects
any transfer whose newest backup locktime is at or below the tip (`LocktimeTooLow`,
`lib/src/transfer/receiver.rs:536`), so at a sustained hop rate ρ hops/day the off-chain
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
`transfer.rs:1378-1389`) — depth does not consume interval — but the **tree's** off-chain life stays
bounded by the root deadline `H_deposit + initlock` (`deposit_anchored_exit_deadline`, `wallet.rs:996`;
pure fn `deposit_anchored_deadline`, `wallet.rs:1295`; audit [10]); see the
open-item caveat in §9.

### 4b. Price of each lifetime-extension option

For an **un-laddered** coin there is **no off-chain ladder-renewal**; resetting it costs an on-chain
touch. (For a **laddered** coin this inverts completely: renewal replaces the extension tier
horizontally for **0 vB and 2 co-signs**, and when the extension budget is exhausted the coin
*rolls over off-chain too* — the current state becomes a self-split paying `A` and a fresh level
hangs off it, giving unbounded off-chain lifetime with zero on-chain bytes. `sdk43` drives
renew → rollover → renew-again → reload-from-disk → exit through the whole deep chain; `sdk40`
PART 3 shows `X_1` superseding `X_0` at consensus level. The mandatory on-chain touch is gone
there, which is why the §4c saver table does not apply to BTC holdings.) The un-laddered options
and their prices:

| Option | On-chain now | On-chain later | Locked/burned now | What it actually extends |
|---|---|---|---|---|
| **Refresh (re-anchor)** | **1 tx ≈ 112 vB** (SE-co-signed spend → fresh aggregate) + deposit token `T` | — | fee 112 sats @1 sat/vB (user-paid, or **operator-rebated off-chain** so your total ends ≥ whole — see the rebate-floor note below) | everything: brand-new coin, fresh full ladder, fresh root; old outpoint spent (old backups die). Measured, `SDK_E2E=30`. |
| **Withdraw + redeposit** | 2 txs ≈ 294 vB (140 + 154) + token `T` | — | — | same as refresh, but two txs — refresh supersedes it |
| **Self-split** | 0 vB | +155 vB on the future exit (branch grows one hop) | fee reserve 300–2,000 sats (becomes the branch fee) | the **leaf ladder** only (fresh 100 hops); the root deadline `H_deposit+initlock` is *not* moved |
| **Branch materialization** | branch txs (155·depth vB, fee = pre-paid reserves) | — | — | converts the sub-coin to a flat coin; the governing deadline becomes the leaf's own ladder (`h_split + initlock`) |

**Refresh is the cheapest full reset** (112 vB in one tx vs 294 vB across two for withdraw+redeposit),
and its fee can be shifted to an operator (off-chain rebate — the blind SE holds no funds, so it
cannot co-fund the tx). Self-split/materialization stay useful for *leaf-only* extension without a
full re-anchor. A "refresh each horizon" rolling loop costs ~112 vB + token per horizon.

**Rebate-floor note (defect D2, fixed).** The 112-sat refresh fee is sub-dust, so it cannot be
rebated *exactly* off-chain; the sponsor must send the smallest off-chain-payable amount ≥ the fee.
`refresh_sponsored` used to size that at the old backup-fee floor `fee + 330 = 442` — but the
operator's own sponsor coin is **laddered**, so the rebate is paid by an in-ladder split whose child
must fund its own extension + state tier before clearing dust: `min_child_value` =
`2·(committed_fee + P2A) + dust` = `2·(248 + 240) + 330` = **1,306 sats** at the default 2 sat/vB.
Sizing into that dead window made the sponsored refresh fail with `FeeTooHigh` *after* the user had
already paid the on-chain re-anchor fee. The rebate is now
`max(fee_sats + DUST_LIMIT, min_child_value)` (`refresh.rs:96-107`); the operator absorbs the
difference and the user ends strictly better than whole — for a 40,000-sat coin, 40,000 − 112 +
1,306 = **41,194**, asserted by `SDK_E2E=30` part (c). Budget the *sponsor's* cost per rebated
refresh at ~1,306 sats, not ~442.

### 4c. Worked one-year TCO

Assumptions: funding 154 vB, withdraw 140 vB, deposit token `T` sats, USD @$100k/BTC.

**(i) Merchant coin — 50 hops, then cooperative exit.** Hops are free; total on-chain =
deposit 154 + withdraw 140 = **294 vB + T**. Per payment: **~5.9 vB**.
At 1 sat/vB: 294 sats total (~$0.29), **~6 sats/payment**; at 10 sat/vB: 2,940 sats (~$2.94),
**~59 sats/payment** (~$0.059). Constraint — §4a's *additive* burn, enforced by receiver
validation rejecting `locktime ≤ tip`: hop k must satisfy `144·t_k < initlock − k·interval`, so
all 50 hops must complete within `(initlock − 50·interval)/144` days — **~3.5 days on profile A
(≥ ~14.4 hops/day)**, ~34.7 days on B (≥ ~1.44 hops/day). (For k = capacity/2 the required rate
is exactly §4a's break-even rate.) A profile-A **un-laddered** merchant coin must therefore turn
over in ~3.5 days, not 6.9 — a real constraint for slow tills, and now specifically a constraint on
merchants circulating an RGB carrier. A **laddered** merchant coin has no pace requirement at all:
its hop budget decrements only when *it* transfers, the tip never eats into it, and the budget is
replenished off-chain (§4b) — the slow till is simply slow. The un-laddered failure mode is
graceful: a coin that falls behind pace stops being transferable (receivers reject it) but remains
cooperatively withdrawable at any time — the roll just amortizes over fewer free hops, e.g. stalling
at hop 33 (the 7.2 hops/day pace) costs ~8.9 vB/payment instead of ~5.9.

**(ii) Saver — must roll every horizon (withdraw + redeposit).** *Un-laddered holdings only* — a
laddered coin has no horizon to roll (0 vB/yr idle; renewal and rollover are both off-chain, §4b).
This row therefore prices holding an **RGB carrier** long-term, which is exactly the case that
cannot be laddered. Rolls/year = `ceil(365.25 / (initlock/144)) − 1`:

| Profile | Horizon | Rolls/yr | On-chain vB/yr | @1 sat/vB | @10 sat/vB | Tokens |
|---|---|---|---|---|---|---|
| A 1000/10 | 6.9 d | **52** | 154 + 52·294 = 15,442 | 15,442 sats (~$15.44) | 154,420 sats (~$154) | 53·T |
| B 10000/100 | 69.4 d | **5** | 154 + 5·294 = 1,624 | 1,624 sats (~$1.62) | 16,240 sats (~$16.24) | 6·T |

The saver profile is where `initlock` matters most: profile B is ~10× cheaper per year to hold,
at the price of a ~10× longer worst-case exit wait (§7). The self-split+materialize loop (*model*,
§4b) would cut the A-profile saver to ~52·155 ≈ 8,060 vB/yr with fees pre-capped at
300–2,000/roll, no tokens — untested as a loop.

**(iii) Exact-amount payment via an un-laddered branch split.** The headline granularity feature
([learn/invalidation.md](../learn/invalidation.md): exact amounts are a native off-chain
operation) is **not free the way whole-coin hops are** on this path. End-to-end, one branch-split
payment costs:

- **Sender:** the fee reserve `clamp(parent_sats/100, 300, 2000)` sats, deducted from the parent —
  and *burned*, not escrowed: the branch tx carrying it is broadcast on every exit path,
  cooperative or unilateral (`wallet.rs:804` — `broadcast_branch_if_any` at `:882` runs
  before any withdraw). Regressive below ~30k-sat parents (§5: 30% of a 1,000-sat parent,
  0.002% of 1 BTC).
- **Receiver:** **+155 vB / +1 depth** inherited on the future exit, plus a fresh ≈`initlock`
  unilateral wait on the new leaf ladder (§6).

Integrator rule of thumb: an un-laddered whole-coin hop is genuinely 0-cost; an un-laddered split is
a 300–2,000-sat + 155-vB event. Prefer coin selection over splitting whenever the wallet holds a
coin of (close to) the right size.

**(iv) Exact-amount payment on a laddered coin — the in-ladder split.** A laddered coin does not
take the branch route at all: `in_ladder_pay` (and `child_in_ladder_pay` one level down) mints the
piece inside a **split state `SP`** that spends `X_m.out[0]`, so it descends from the trigger
instead of racing it. Its fee model is entirely separate from `split_fee_reserve`:

- the split state's fee is `committed_fee_for_outputs(n_payload, rate)` — `TIER_VBYTES` (124) plus
  `P2TR_OUT_VBYTES` (43) per extra child, times the committed rate — and the children share
  `tier_out_total(X_m.out[0], n, rate)` **exactly** (value conservation is checked in the builder,
  `lib/src/tesr.rs:369-390`, not trusted);
- each child then hangs its **own** extension + state, so the admission floor is
  `min_child_value(rate, dust)` = **1,306 sats** at 2 sat/vB — not the 442-sat backup-fee floor.

⚠️ **Defect D1 (fixed).** The in-ladder split's admission guard used the old 442-sat backup-fee
floor. Because `establish_child` runs *after* the parent's spend budget is consumed and `SP` is
co-signed, a child admitted between 442 and 1,306 sats **terminalized the parent and then failed**,
stranding it at unilateral-exit-only. The guard now takes
`max(min_split_output, min_child_value)` (`transfer.rs:705`, `:820`). Budget an in-ladder payment's
minimum piece at 1,306 sats, and note the per-hop cost is otherwise **0 vB**: no reserve is burned
and no branch tx is added — the cost is one co-signature and one disclosed superseded state (§2b).
Covered by `sdk58` (11 adversarial cases), `sdk59`, `sdk60`, `sdk17`.

## 5. Size effects

**Fee reserve as % of coin value** (`clamp(parent/100, 300, 2000)`, `split_fee_reserve`,
`transfer.rs:1416`) — the un-laddered branch split. (An in-ladder split has no reserve at all; its
size effect is the flat 1,306-sat child floor, §4c(iv).)

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
- **Minimum exit-viable un-laddered coin = `330 + ceil(112·r_backup)`:** the backup's output must
  clear dust after its fee (`lib/src/transaction.rs:123-131`, else `FeeTooLow`). At the default
  `max_fee_rate = 1.0` that is **442 sats** (`min_split_output`, `transfer.rs:1437`); a coin
  co-signed at 5 sat/vB needs ≥ 890.
- **Minimum in-ladder child = `2·(committed_fee + 240) + 330`:** **1,306 sats** at the default
  2 sat/vB (`min_child_value`, `lib/src/tesr.rs:75`) — three times the un-laddered floor, because
  the child funds two tiers rather than one backup. This is the number that binds on every laddered
  payment path (and on a sponsored refresh's rebate, §4b); using the 442 floor there was defect D1.
- **Micro-pieces are exit-fragile:** a 1,000-sat sub-coin's depth-1 exit costs 267–534 sats at
  just 1–2 sat/vB — 27–53% of value; any fee spike (§3b) wipes it out. Economic floor for pieces
  intended to be *exitable alone* is ~10k sats at low fees, higher in a high-fee regime.
- **Maximum coin ≈ 42.9 BTC (u32):** amounts are booked as `u32` sats
  ([SPEC.md §14](../SPEC.md#14-known-limitations-adversarial-review)). The truncation hole is now
  closed at the entry point — the deposit path refuses to book a UTXO above `u32::MAX` rather than
  casting it (`clients/libs/rust/src/coin_status.rs:50-57`) — and the split path errors via
  `u32::try_from` (`transfer.rs:389`, `:400`, `:585`, `:593`). Stay well below anyway.

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

The unilateral rows are the un-laddered shape. A **laddered** coin's wait is not `initlock` at all
but the sequential relative CSVs `E_m + Δ_k` — worst ~2,160 blocks (~15 days) fresh at the mainnet
defaults (`D0 = 1440`, `E0 = 720`, `lib/src/tesr.rs:120-121`), *decreasing* 36 blocks per hop or
renewal, and only starting once the trigger confirms. A depth-`d` in-ladder sub-coin worst-cases at
`(d+1)·2,160` blocks (PROTOCOL.md §5.9, default depth cap 3).

The asymmetry to design UX around is the same in both shapes: cooperative paths are minutes at any
depth; unilateral paths are *days-to-months* dominated by `wait_blocks`, not fees. A wallet should
surface `estimate_exit_cost().wait_blocks` (and `exit_deadline_block` — the *safety* deadline, a
different number) whenever the SE looks unhealthy.

⚠️ **Defect D4 (fixed)** lives in this UX seam: a *child* routed to a unilateral exit was booked
`WITHDRAWING`, but a unilateral exit produces no withdrawal transaction, so status polling errored
forever. The child now reports the unilateral shape it actually has (PROTOCOL.md §5.9).

## 7. Parameter sensitivity (initlock / interval)

*Un-laddered dial.* The laddered analogue is `TesrParams` (`lib/src/tesr.rs:92-121`) — `D0`/`δ`
(state head start and per-transfer decrement), `E0`/`δE` (extension budget and per-renewal
decrement), the floors, and `m_max` before a forced off-chain rollover; mainnet defaults
`1440/36/144, 720/36/144, m_max 15, 2 sat/vB`. Those trade exit-wait length against hop budget in
the same shape as the table below, but with **no** wall-clock burn term: the tip does not consume a
relative timelock, so there is no "hops vs days" additivity to balance. `sdk44` pins the schedule.

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
| **This system — laddered** (every plain deposit) | **0 vB** per hop | (deposit 154 + withdraw 140) / N payments ≈ 294/N vB | 3 pre-signed tiers ≈ 372 vB flat (372–828 vB with P2A fee children in a spike); depth-d: 3+2d txs ≈ 124·(3+2d) vB | idle coins **never age** (0 vB/yr rent); wait = relative `E_m + Δ_k`, worst ~2,160 blk (~15 d) fresh, −36 blk per hop/renewal; renewal **and** rollover are off-chain, **0 vB** (§4b, `sdk43`) |
| **This system — un-laddered** (RGB carriers; un-broadcast-funded sub-coins) | **0 vB** per hop | same 294/N vB | 1 tx flat (112 vB); depth-N: N+1 txs, 112+155N vB, fees §3b | wait ≤ initlock (6.9 d A / 69.4 d B); renewal = on-chain roll (no renew endpoint), 155–294 vB per horizon (§4b) |
| **Spark** | 0 per transfer | L1 deposit pre-signs node+refund; coop exit needs SSP connector tx (extra on-chain tx on SSP's side) | node-tx chain to the leaf (tree depth) + refund tx; anchor output per node adds vB (*model*: sizes comparable per-tx, no measured figure in repo) | relative timelock 2000, −100/transfer, **renew_leaf at ≤300 needs the SO** — off-chain renewal (0 vB) but SO-liveness-dependent (`protocol-notes.md:17-19`) |
| **Ark / Second** | 0 within a round | must join a shared round tx **every round** — amortized share of one on-chain tx per round, mandatory | broadcast your branch of the round tree (~log₂(participants) txs, *model*) | miss the round/expiry ⇒ funds sweep to the server — the only design here where lateness is loss, not delay |
| **Lightning** | 0 per HTLC | channel open + close ≈ 2 txs, ~300–450 vB total (*model*), amortized over the channel's payments | force-close tx + CSV `to_self_delay` (~1–14 d typical, *model*) + per-HTLC sweeps | no expiry; but capacity is pairwise and inbound liquidity is a separate cost |
| **On-chain baseline** | 111–154 vB *every* payment | — | — | none |

Reading the table: this design and Spark have the same steady-state shape (0 vB per transfer,
linear-in-depth unilateral exit). **The renewal clock is no longer the differentiator it was.** The
laddered shape now matches Spark's free off-chain renewal and goes further — at budget exhaustion it
rolls over off-chain rather than requiring an on-chain touch (`sdk43`), so an idle coin is exactly
0 vB/yr and never needs to reach anyone. It keeps the property Spark's `renew_leaf` lacks: exit
never needs the SE. The *un-laddered* shape still pays 155–294 vB on-chain per horizon — that is now
the price of holding an RGB carrier, not the price of holding BTC.

Unlike Ark, missing every deadline degrades to a timelocked wait plus a race, never a sweep — a
stale state loses at consensus (`sdk40` PART 2), a hostile trigger is out-raced by the owner's
lower-CSV state (`sdk51`), and a keyless tower can drive that defence for an offline owner
(`sdk45`). Splitting grants children a fresh timelock budget in *both* systems — here a fresh
child extension + state hanging off the split state `SP` (or, un-laddered, a fresh
`h_split + initlock` leaf ladder), in Spark a fresh 2000 behind a zero-timelock split node
([protocol-notes.md](protocol-notes.md)) — so that is not a differentiator. The split economics
differ in who pre-pays: un-laddered, the splitter's 300–2,000-sat reserve; in-ladder, the committed
tier fees already baked into `SP` and the child's own two tiers (the 1,306-sat floor, §4c(iv)).

## 9. Open items that move these numbers

Honesty section — status per [AUDIT-2026-07.md](../AUDIT-2026-07.md) remediation UPDATE 2 (all
11 HIGH findings fixed + verified; mainnet still gated on the SGX rebuild + re-audit):

- **[17] (half-closed, and now scoped to un-laddered coins): the true safety deadline can be
  earlier than the implemented one.** The implemented
  `exit_deadline_block = H_deposit + initlock` (`deposit_anchored_deadline`, `wallet.rs:1295`) is
  exact for
  unsplit-fresh parents but **late by k·interval** when the parent had k transfer hops before the
  split — the splitter's own retained backup matures at `H_deposit + initlock − k·interval`. An
  online receiver is unaffected (broadcast the locktime-free branch immediately and the point is
  moot). Batch 5 shipped the auto-exit half: `auto_exit_due(margin_blocks)` (`wallet.rs:693`)
  force-broadcasts the branch within a margin of the deadline; the conveyed-ancestor-locktimes half
  remains open, so the §4 "root deadline" figures are upper bounds and the margin must absorb
  `k·interval`. This whole item is an *absolute*-locktime artefact and does not arise on a laddered
  coin, where no ancestor holds a maturing backup at all — its blast radius is now RGB carriers and
  un-broadcast-funded sub-coins.
  The exactness domain and the binding mitigation are stated normatively in
  [INVALIDATION-SPEC.md §6.2](../INVALIDATION-SPEC.md) (IVL-INV-10 / IVL-REQ-16: eager
  materialization, or a conservative `M·interval` margin via `auto_exit_due`).
- **[15] (closed, AUDIT UPDATE 3): griefing brick** — the replay that could force a coin down
  the unilateral path (full `initlock` wait, no theft) is no longer possible on the irreversible
  endpoints; §7's forced-unilateral exposure column survives as the price of an SE failure, not
  of an attack.
- **sdk26 measurements are now a historical record, not live coverage.** The 2026-07-07 run of
  `SDK_E2E=26` (`sdk26_invalidation_scale`) produced the §3b figures — per-hop branch delta exactly
  155 vB at depths 1–4; depth-4 total 732 vB; 3-wide fan-out split 241 vB; a full depth-4 unilateral
  exit on regtest (branch instant, backup after a 997-block wait, funds at the owner's backup
  address). **That test has been retired** along with `sdk27`: under TES-R the absolute-locktime
  invalidation ladder it was scaling is no longer the shape a plain deposit takes — idle coins never
  age, and the ladder plus terminality subsume what sdk26/27 were measuring. The numbers above are
  kept because they still describe the un-laddered branch chain, which is still in the code and
  still the RGB-carrier path — but **nothing re-measures them**, and no live E2E asserts a branch or
  backup vsize. What *is* live: `sdk39` drives a depth-2 branch exit end-to-end, `sdk31` measures a
  single 2-input colored combine (`combine_tx.vsize()`, `sdk31_token_combine.rs:204`, ~255 vB),
  `sdk55` enforces the backup chain's decrement invariant, and `sdk50`/`sdk58` drive the laddered
  exit and in-ladder split. Multi-hop combine CHAINS (§1) remain unmeasured under either shape.
- **Batching (future):** plain-BTC combine-then-withdraw would turn §3a's N-tx cost into ~1 tx;
  today it does not exist as an SDK operation.
- **Retired coverage elsewhere on this page, for the record:** `sdk07`/`sdk08`/`sdk10`
  (exit/terminal) → `sdk50` + `sdk58`; `sdk13`/`sdk14` (stale-state race) → `sdk51`, `sdk40` PART 2,
  `sdk45`; `sdk28` (sats granularity) → obsolete, the in-ladder split has its own fee model
  (§4c(iv)); `sdk33` (auto-refresh) → obsolete, a laddered coin has no floor to approach (0 vB rent)
  and unbounded off-chain renewal is `sdk43`; `sdk35` (trust boundaries) → `sdk45` (the bundle
  carries **no** key material, and a second independent tower is idempotent — both back-filled
  there), `sdk52` (a carrier is never laddered), `sdk51`, `sdk46`/`sdk47`/`sdk54` (the R′ census).
  Every security claim those tests backed is preserved above; only the evidence pointer moved.
