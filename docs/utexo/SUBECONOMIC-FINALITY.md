# Sub-economic finality — the sender's free option over a piece too small to defend

> ⚠️ **[D53] CORRECTION, 2026-08-14 — the depth cap in this document is STALE.**
> Every statement here of `max_split_depth = 10` / **23 transactions** (mainnet) or
> `max_split_depth = 68` / **139 transactions** (regtest) was measured against the BARE latency rule
> `exit_wait_blocks <= epoch`. The rule a conveyed child is actually ADMITTED by adds
> `exit_slack_margin`, so the shipped caps are **depth 8 / 19 transactions** (mainnet) and
> **depth 54 / 111 transactions** (regtest). Depths 9 and 10 were unadoptable at every tip. The
> 9 383-block / 65.2-day figure remains correct as the WAIT of a depth-10 walk — it is not the cap.
> Read `DECISIONS.md` D53 before carrying any depth figure from this document into the spec.


> **Status:** normative finding, re-verified at `feat/spark` `a0c19fb`. The gap is live. Section 6
> ranks what to build.
>
> **One recommendation has since been built, in part.** R-4's missing primitive — a transaction that
> spends a P2A anchor — shipped as `mercurylib::wallet::p2a_fee_child::build_p2a_fee_child`
> (`lib/src/wallet/p2a_fee_child.rs`, D31/#123) and is wired into the **whole-coin** passes only
> (`watch_pass_with_bump`, `exit_pass_with_bump`, driven from the SDK's `unilateral_exit` when a
> `fee_bump` source is configured). The **piece's** lane, `exit_child_pass`, has no bump variant, and
> neither does the spine tip's. §C-2 and R-4 are written around that split. R-0, R-1, R-2, R-3 and
> R-5 are unbuilt.
>
> **Citations are by SYMBOL**; the line numbers are as of `a0c19fb` and are an aid, not an address.
> `clients/libs/rust/src/tesr.rs` has moved by thousands of lines since the first draft
> (`verify_conveyed_child` 4152 → 7278), so a number that no longer lands means the file moved, not
> that the symbol went away.

## 0. The finding in one paragraph

A split piece's only route to Bitcoin is its tier chain. It has **no flat backup** —
`CHILD_V2_BASELINE = 0` (`clients/libs/rust/src/tesr.rs:6778`), because a child slot is an
SE-registered key that is never funded on-chain, so `check_deposit`/`create_tx1` never run for it.
The **sender**, holding the parent, keeps a flat backup that spends the on-chain funding output `F`
directly and pays them the whole coin. Walking the piece's chain costs the payee `3 + 2d`
transactions and `293d + 375` vB (`tesr_exit_txs`/`tesr_exit_vbytes`,
`clients/libs/rust-sdk/src/config.rs:174,189`); broadcasting the flat backup costs the sender
**one 112-vB transaction** (`BACKUP_TX_SIZE`, `lib/src/transaction.rs:116`). The protocol admits a
piece at **1 310 sat** (`min_child_value(2.0, 330)`, `lib/src/tesr.rs:158-166`, asserted `:1197`).
At 20 sat/vB a depth-10 piece costs **124 870 sat** to defend. Two admission gates now stand at that
door and neither is about value: `check_exit_headroom_with_margin`
(`lib/src/transfer/receiver.rs:920`, [D40.3]) checks the exit fits in **time**, with a slack margin
derived from the walk (`exit_slack_margin`, `:908`); the [P0-3] length cap
(`max_exit_txs`/`ExitChainTooLong`, `:1016`/`:1035`, enforced by `enforce_exit_chain_length`) checks
it fits in **work** — 23 transactions on mainnet. Nothing checks it is **worth doing**.

**It needs no malice.** After `in_ladder_split`, `set_spend_budget(parent, 1)` is consumed by `SP`
and the SE returns 410 Gone for any further co-sign (`server/src/endpoints/sign.rs:104-116`), so the
sender's two remaining routes out are: walk their own ladder (`293d + 375` vB), or broadcast the flat
backup (112 vB). **The backup is cheaper at every fee rate: 6.0× at depth 1, and 29.5× at depth 10 —
the deepest piece mainnet admits (§2.1).** One backup voids the
entire tree, so the marginal cost of taking each additional piece along is exactly **zero**. An
ordinary sender exiting for their own reasons voids every sub-economic piece they ever paid.

This is sharper than `PARTIAL-PAYMENT-ECONOMICS.md` §4.8, which frames sub-economic payees as a
**liveness** cost borne by the payee. It is a **finality** limit, and it is the **sender's** option.

---

## 1. The break-even function

Constants, all read from the tree:

| symbol | value | source |
|---|--:|---|
| `TIER_VBYTES` | 125 | `lib/src/tesr.rs:93` |
| `P2TR_OUT_VBYTES` | 43 | `lib/src/tesr.rs:606` |
| `P2A_VALUE` | 240 | `lib/src/tesr.rs:41` |
| `DUST_LIMIT` | 330 | `lib/src/transaction.rs:120` |
| `BACKUP_TX_SIZE` | 112 | `lib/src/transaction.rs:116` |
| `COLORED_TIER_VBYTES` | 168 (211 two-payload) | `clients/libs/rust/src/rgb.rs:931`, `colored_tier_vbytes` `:940` |
| `committed_fee_rate` **c** | **2.0** | `lib/src/tesr.rs:256` (mainnet), `:261` (regtest) |
| `committed_fee(r)` | `ceil(125·r)` | `lib/src/tesr.rs:103-105` |
| rung = `committed_fee(c) + P2A` | **490** (coloured **576**) | `lib/src/tesr.rs:133-145` (`tier_out_value`) |
| `N(d)` = exit transactions | `3 + 2d` | `clients/libs/rust-sdk/src/config.rs:174` |
| `V_walk(d)` = exit vbytes | `293d + 375` | `clients/libs/rust-sdk/src/config.rs:189` |
| `V_walk_col(d)` = coloured exit vbytes | `379d + 504` | same shape, `colored_tier_vbytes` per tier |

**The CPFP child.** The economics doc prices a "~152-vB child"
(`PARTIAL-PAYMENT-ECONOMICS.md:650-651`), derivable with the same byte accounting the tier uses
(`lib/src/tesr.rs`, the tier-vsize note): P2A input 41 B + P2TR key-spend input 41 B + one P2TR
output 43 B + header/counts 10 B = **135 B** base; witness = 2 (marker/flag) + 1 (P2A empty stack) +
66 (P2TR) = **69 B**; weight `135·3 + 204 = 609` WU ⟹ **152.25 vB**, floored to 152.

**The tree now has its own figure, and it is 153.** `estimate_child_vsize()`
(`lib/src/wallet/p2a_fee_child.rs:108-110`) returns `11 + 41 + 58 + 43 = 153`, and a real signed
child measured 153 vB on regtest (`clients/libs/rust/tests/live_p2a_package_rescue.rs`). Every table
below is computed at **152**, because that is what reproduces the economics doc's §6 numbers to the
sat and the reproduction is what validates the model. The tables are therefore low by
`N(d)·(ceil(153r) − ceil(152r))` ≈ `N(d)·r` sats — 460 sat at d=10, r=20. Use 153 when pricing a
real rescue.

**The top-up.** At `r ≤ c` it is **zero** — `TIER_VBYTES`'s own doc comment
(`lib/src/tesr.rs:63-71`) states that the committed fee exists so the tier "relays and confirms with
no P2A child attached" at that rate. Above `c`, each tier needs a package at rate `r`, and TRUC's
one-unconfirmed-child rule plus the sequential relative CSVs forbid batching those children:

```
TOPUP(d, r) = 0                                                     r ≤ c
            = ceil((r − c)·V_walk(d))  +  N(d)·(ceil(152·r) − 240)   r > c
```

The `− 240` is the P2A value the child recovers. **This model reproduces the doc's own §6 table to
the sat**, which is what validates it:

```
topup(V_walk(100), N(100),  5.0) == 194 585    # PARTIAL-PAYMENT-ECONOMICS.md:655
topup(V_walk(100), N(100), 20.0) == 1 102 550  # :656
topup(V_walk(100), N(100), 50.0) == 2 918 480  # :657
```

**Break-even.** A piece minted at value `V` on `SP.out[j]` spends `2·rung` on its own extension and
state (`establish_child`), so its final reachable output is `V − 980`. The payee funds `TOPUP`
externally. Defend iff `V − 980 − TOPUP(d, r) ≥ 0`:

```
V_min(d, r) = 2·rung + max(DUST_LIMIT, TOPUP(d, r))
```

**And here is the sharpest fact in this document:**

```
V_min(d, 2.0) = 980 + 330 = 1310 = min_child_value(2.0, 330)      lib/src/tesr.rs:158-166
```

`min_child_value` is **not** a floor that ignores economics. It *is* `V_min`, evaluated at
`r = committed_fee_rate`. It is exactly correct at 2.0 sat/vB and at no other rate. The defect is
therefore not "a missing check" — it is **a correct check frozen to a hardcoded constant**
(`TesrParams::mainnet().committed_fee_rate`, `lib/src/tesr.rs:256`). That is a much narrower, and
much more fixable, statement.

---

## 2. The band

`V_min(d, r)` in sats, plain lane, live shape (CATS-B change 1 landed). Legend: `=` the floor is
exactly right · `!` floor too low but a 10 000-sat payment still survives · `*` exceeds the doc's own
worked payment size of 10 000 (§2:126) — the typical payment is dead · **`X`** exceeds 150 000, the
top of the entire simulated payment range (§3.1:177) — the lane is dead for every payment the design
models.

| d | txs | walk vB | r=2 | r=5 | r=10 | r=20 | r=50 | r=100 |
|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| 1 | 5 | 668 | 1 310 `=` | 5 584 `!` | 12 724 `*` | 27 004 `*` | 69 844 `*` | 141 244 `*` |
| 2 | 7 | 961 | 1 310 `=` | 7 503 `!` | 17 628 `*` | 37 878 `*` | 98 628 `*` | 199 878 **`X`** |
| 5 | 13 | 1 840 | 1 310 `=` | 13 260 `*` | 32 340 `*` | 70 500 `*` | 184 980 **`X`** | 375 780 **`X`** |
| 10 | 23 | 3 305 | 1 310 `=` | 22 855 `*` | 56 860 `*` | 124 870 `*` | 328 900 **`X`** | 668 950 **`X`** |
| 100 | 203 | 29 675 | 1 310 `=` | 195 565 **`X`** | 498 220 **`X`** | 1 103 530 **`X`** | 2 919 460 **`X`** | 5 946 010 **`X`** |

Every cell at `r > 2` is a cell where the protocol admits at 1 310 a piece it should refuse. As a
multiple of the admission floor:

```
d=  1:  r5    4.3x   r10    9.7x   r20   20.6x   r50    53.3x   r100   107.8x
d= 10:  r5   17.4x   r10   43.4x   r20   95.3x   r50   251.1x   r100   510.6x
d=100:  r5  149.3x   r10  380.3x   r20  842.4x   r50  2228.6x   r100  4538.9x
```

Coloured (rung 576, floor `colored_child_floor(2.0, 330) = 1 482`,
`clients/libs/rust/src/tesr.rs:3372`) is **not** the same table shifted by the rung difference. At
`r = c` it is — `1 482 = 1 310 + 172`, and +172 at every `r=2` cell — but above `c` the *walk* is
dearer too, because every coloured tier carries an `opret` output: `colored_tier_vbytes`
(`clients/libs/rust/src/rgb.rs:940`) is 168 vB one-payload and 211 two-payload, so the coloured walk
is `379d + 504` against `293d + 375` — **883 vB at depth 1 against 668**, 1.32×. Re-running the same
model on the coloured walk:

| d | coloured walk vB | `V_min@20` | `V_min@50` |
|--:|--:|--:|--:|
| 1 | 883 | 31 046 (plain 27 004) | 80 336 (plain 69 844) |
| 10 | 4 294 | 142 844 (plain 124 870) | 376 544 (plain 328 900) |

**Colour does not change the shape of the finding, and it is not free either**: it lifts the band by
~15% at every rate above `c`, and the top-up still dominates the rung by an order of magnitude.

A 10 000-sat piece, net after defending (`V − 980 − TOPUP`):

| | r=5 | r=10 | r=20 | r=50 |
|---|--:|--:|--:|--:|
| d=1 | **+4 416** | −2 724 | −17 004 | −59 844 |
| d=10 | −12 855 | −46 860 | −114 870 | −318 900 |

**A depth-1 10 000-sat piece goes under water between 5 and 10 sat/vB.** That is the modal payment
in the design's own simulation.

### 2.1 The reachable ceiling

Depth 100 cannot be minted on mainnet, and neither can depth 11. `enforce_split_depth_cap`
(`clients/libs/rust/src/tesr.rs:6472`, deciding in `split_cap_decision` `:6692`) evaluates
`max_split_depth` (`lib/src/transfer/receiver.rs:948`) on the live schedule, with
`epoch_blocks = info.initlock`:

```
base      = [None, 720, 0, 720, 1440]   -> exit_wait_blocks = 2885
per_level = [720, 0]                    -> exit_wait_blocks =  722
initlock = 10 000 (mainnet/testnet/signet)  -> max depth 10
initlock =  1 000 (regtest, its own schedule: E0=12, D0=24)  -> max depth 68
```

**`initlock` is not a free parameter, and 10 000 is the only mainnet answer.** [D8(f)]
`TesrParams::flat_ladder_params` (`lib/src/tesr.rs:344-371`) compiles in `(10 000, 100)` for
bitcoin/testnet/signet and `(1 000, 10)` for regtest; a coordinator configured with anything else
**panics at boot** (`server/src/server_config.rs:225-242`), and a client refuses any coordinator that
reports a different pair (`info_config`, `clients/libs/rust/src/utils.rs:52-61`) — the client uses its
own compiled-in value even for this cap. Any depth quoted against another epoch length is arithmetic
on a configuration that cannot run; the sensitivity table in §A-1 keeps those rows deliberately, as
design space rather than deployment.

A second, independent cap bounds the same shape in transactions: `enforce_exit_chain_length`
(`clients/libs/rust/src/tesr.rs:6639`) refuses any chain longer than
`max_exit_txs = 3 + 2·max_split_depth` = **23** on mainnet, at build (`split_cap_decision`) and at
admission (`verify_conveyed_child` `:7533`, `transfer_receiver.rs:670,1432`). So the d=100 row in §2
is a model extreme, not a mintable or adoptable shape.

So the number to quote is:

```
d= 10 (mainnet, the cap):  23 tx / 3 305 vB -> V_min@50 = 328 900 sat = 0.00329 BTC (251x the floor)
```

### 2.2 The sender's side, and the ratio

The flat backup is one 1-in-1-out P2TR: 112 vB. Its fee is committed at signing and every receiver
re-validates it inside a ±5 sat/vB band (`fee_rate_tolerance = 5.0`,
`clients/libs/rust/src/client_config.rs:66`; enforced in `verify_transaction_signature`,
`lib/src/transfer/receiver.rs:599-604`), so it is normally already at rate. If it needs a bump,
`create_cpfp_tx` (`lib/src/wallet/cpfp_tx.rs:46`) adds a second 1-in-1-out P2TR — a 224-vB package
worst case.

The vbyte ratio is the one number that does not depend on the fee rate:

```
d=  1:    668/112 =   6.0x      5 tx vs 1
d=  2:    961/112 =   8.6x      7 tx vs 1
d=  5:  1 840/112 =  16.4x     13 tx vs 1
d= 10:  3 305/112 =  29.5x     23 tx vs 1   <- the mainnet ceiling (§2.1)
d=100: 29 675/112 = 265.0x    203 tx vs 1   <- model extreme; refused at build and at admission
```

**6× is the best case for the payee, and 29.5× is the worst case that can actually exist on
mainnet.** The ratio is 6× at depth 1 and degrades linearly with the sender's payment history until
the 23-transaction length cap stops it.

The payee cannot wait the sender out: once the flat backup confirms, `F` is spent and the whole tree
is dead. The payee must push `T` first, at its own cost — the full walk is the payee's, never shared.
And the payee is handed the evidence: the sender's backup chain is conveyed as
`ChildTesrBundle::parent_flat_backups` (`clients/libs/rust/src/tesr.rs:396`), so the payee holds
proof that the sender retains the cheap exit, and can do nothing with it except race.

---

## 3. Why the operator cannot simply fix this — the blind-SE boundary, stated once

Every proposal below is measured against this. **The SE sees no value, anywhere:**

* `SignFirstRequestPayload` is `{ statechain_id, signed_statechain_id }` — no amount, no sighash
  (`lib/src/transaction.rs:11-14`).
* `PartialSignatureRequestPayload` is `{ statechain_id, negate_seckey, session,
  signed_statechain_id, server_pub_nonce }` (`lib/src/transaction.rs:44-50`). The session carries a
  message to sign; no output is deserialised anywhere server-side.
* `DepositMsg1` is `{ auth_key, token_id, signed_token_id, single_use, epoch_deadline,
  user_public_key }` (`lib/src/deposit/mod.rs:55-75`) — no amount.
* **There is no `amount` column in the server database or in any migration**
  (`grep -rn amount server/src/database/ server/migrations/` returns nothing).
* `/info/config` serves `{ initlock, interval, batchtimeout, version }` and nothing else
  (`server/src/endpoints/utils.rs:186-202`).

So the SE cannot distinguish a flat backup from a tier, a piece from change, a 1 310-sat coin from a
1 BTC coin. It sees a statechain id and an authenticated request to co-sign an opaque session.

### 3.1 Proposals that are WRONG, named so they are not re-proposed

* **"The SE refuses to co-sign `SP` unless every piece clears a viability floor."** Rejected: the SE
  cannot read `SP`'s outputs. It never deserialises a transaction.
* **"The client declares each piece's value in the `sign/first` payload and the coordinator checks
  it."** Rejected twice over. The declaring party *is* the attacker, and a declared field is not
  provenance — `ADMISSION-INPUTS.md:132` states the rule: *"**declared** — a serde field. **Not
  admissible.**"* The coordinator also has no chain access with which to check it.
* **"The SE refuses to co-sign the sender's flat backup after a split."** Rejected as *already true
  and insufficient*: `set_spend_budget` makes the parent terminal and `sign/first` returns 410 Gone
  (`server/src/endpoints/sign.rs:104-116`), re-checked at `sign/second`
  (`server/src/endpoints/sign.rs:293-301`). The sender cannot mint a **new** backup. The backups
  they **already hold** are pre-signed, valid Bitcoin transactions. **No operator can un-sign them.**
  This is the residual, and it is structural.
* **"The operator escrows the flat backups and refuses to serve them."** Rejected: the sender
  assembled and stored them locally at deposit and at every transfer hop. There is nothing to
  withhold.
* **"The SE refuses to co-sign a tier below `min_child_value`."** Rejected: same blindness, and the
  floor it would need is a function of the *live fee rate*, which the SE also does not have — it
  serves no fee rate at all (`server/src/endpoints/utils.rs:186-202`); the client computes it from
  its own Electrum backend (`estimate_fee(3) · 100 000`, `clients/libs/rust/src/utils.rs:63-71`).

---

## 4. The three buckets

### A. What the operator CAN enforce, without seeing a single amount

Four levers exist. **None of them is a fix**; each is a value-blind bound on the *shape* of the
exposure, and they are worth stating because they are the whole of what the operator side can do.
Since this list was first written, one of the four (A-1) has been taken out of the operator's hands
entirely — which is the right outcome, and is recorded there rather than by dropping the row.

#### A-1. `lockheight_init` is a direct ceiling on the payee's worst-case defence cost — but it is no longer an operator dial

The epoch length feeds `enforce_split_depth_cap` (`clients/libs/rust/src/tesr.rs:6472`), which caps
depth, which caps `V_walk(d)`, which caps `V_min`. That chain is real and it is the reason this lever
is listed at all.

**What has changed is who holds it.** [D8(f)] pinned `initlock`/`interval` to a compiled-in
per-network pair (`TesrParams::flat_ladder_params`, `lib/src/tesr.rs:344-371`): a coordinator
configured otherwise panics at boot (`server/src/server_config.rs:225-242`), and a client refuses one
that reports otherwise and then uses its **own** value for the cap
(`clients/libs/rust/src/utils.rs:52-71`). So on mainnet the top of the band is 10 000 → depth 10, for
everyone, and the table below is **design space, not deployment**: changing any row means changing
the constant in `flat_ladder_params` and shipping both sides, not editing a compose file.

| `lockheight_init` (design space) | max depth | exit txs | exit vB | `V_min@20` | `V_min@50` | × floor @50 |
|--:|--:|--:|--:|--:|--:|--:|
| 3 000 | 1 | 5 | 668 | 27 004 | 69 844 | 53× |
| 5 000 | 3 | 9 | 1 254 | 48 752 | 127 412 | 97× |
| 7 500 | 7 | 17 | 2 426 | 92 248 | 242 548 | 185× |
| **10 000 (shipped)** | **10** | **23** | **3 305** | **124 870** | **328 900** | **251×** |
| 20 000 | 24 | 51 | 7 407 | 277 106 | 731 876 | 559× |
| 50 000 | 66 | 135 | 19 713 | 733 814 | 1 940 804 | 1 482× |

**Direction of failure, per `ADMISSION-INPUTS.md`'s rule for operator-supplied terms:** an inflated
`initlock` makes `epoch_expiry_height` larger, `check_exit_headroom` more permissive, and deeper
pieces mintable — i.e. it moves the band **up**, against the payee. A deflated one refuses coins and
forces re-anchoring, i.e. it fails toward liveness cost, not theft. **Lower is safer for this
finding**, which is the opposite of the direction `check_exit_headroom` wants. That asymmetry is
exactly why D8(f) taking the term out of the operator's hands was the right closure and not merely a
config-hygiene fix — but the trade-off did not go away, it moved into the constant. Whoever changes
`flat_ladder_params` should be told it prices two things at once.

#### A-2. `max_derived_tokens_per_statechain` bounds one parent's LIFETIME slot issuance — not a per-level fan-out

`64` (`server/src/server_config.rs:75`, default `:103`), enforced at
`POST /deposit/get_derived_token` (`server/src/endpoints/deposit.rs:134-205`) as a **lifetime**
allowance per parent statechain, counted by `count_derived_tokens` over `derived_from` with **spent
rows included** (`:182-204`), fail-closed on a read error (`:188-194`), and `0` disables the endpoint
outright (`:142-147`).

**Read it as what it is.** It is 64 slots for that parent's whole life — pieces *and* change,
summed across every split it ever performs — not 63 pieces per level. And it does **not** bound the
subtree: each piece is itself a statechain with its own 64-slot allowance, so what one 112-vB backup
voids is the entire subtree beneath `F`, bounded only by the depth cap (10) and by how many slots the
sender bothered to mint. It is a **count**, so it is perfectly value-blind, and it bounds *how many*
sub-economic pieces exist under one parent — never *how small* they are.

#### A-3. The terminality and census gates are already maximal, and they do not touch this

`set_spend_budget` (`server/src/endpoints/lightning_latch.rs:295`) with `remaining ∈ {0,1}` (`:297`),
`is_single_use`, the epoch deadline, the pending-transfer lock, and the `num_sigs` census are all
enforced at **both** `sign/first` and `sign/second` (`server/src/endpoints/sign.rs:88-140` and
`:279-315`), all fail-closed. They prevent a **second conflicting signature**. The sender's flat
backup is not a second signature — it is the **first** one, issued at deposit, long before the split
existed. There is no gate to add here.

#### A-4. The SSP — the one operator-run component that DOES see values

This is the real bucket-A answer, and it is real because the SSP is **not the SE**. It is an
SDK client with its own wallet, its own Electrum backend, and full sight of every value.

* **Pre-pay (`SspService::execute_pay`, `clients/libs/rust-sdk/src/ssp.rs:380`).** It already runs a
  census gate (`ladder_census_ok`, `:451-464`) and a value gate (`check_latched_coins`, `:292-315`,
  called `:554`) on `PendingTransferInfo::amount` — which for a child bundle is the **census-bound**
  exit value returned by `prepay_child_census`, overriding the branch-derived one and only after the
  bundle is bound to the latched sid (`clients/libs/rust/src/transfer_receiver.rs:939-978`).
  **What to add:** in the same block, refuse any latched coin whose
  `child_state.out_value − TOPUP(d, r_live)` is below a configured margin, with `d` from
  `cb.ancestors.len() + 1` and `r_live` from the SSP's own estimator. One `if`, in a function that
  already refuses by name.
* **Receive (`SspService::create_receive`, `:618`, and `largest_laddered_coin`, `:686`).** The SSP
  **mints** the piece here. It must not mint one it would refuse to accept. Same predicate, applied
  to the leg it is about to build.

This is an operator **policy**, not a protocol rule: a market participant declining inventory it
cannot redeem, exactly as a merchant declines dust. It protects the SSP and every user who is paid
*by* the SSP. It protects nobody in a user-to-user payment. **Say so plainly** — an operator who
ships A-4 and calls the finding closed has closed one lane out of two.

### B. What only the CLIENT can enforce — the real fix

The gate belongs **beside `check_exit_headroom_with_margin`, in `verify_conveyed_child`**, at
`clients/libs/rust/src/tesr.rs:7546`, inside the function at `:7278` and next to the length cap at
`:7533`. It is the same shape as the existing two gates: receiver-derived, fail-closed, refusing by
name.

**What it computes:**

```
V      = cb.child_state.out_value          the value the payee can actually reach on chain
d      = cb.ancestors.len() + 1
N, vB  = tesr_exit_txs(d), tesr_exit_vbytes(d)     config.rs:174,189
TOPUP  = 0                                  r_live <= c
       = ceil((r_live − c)·vB) + N·(ceil(estimate_child_vsize()·r_live) − 240)   // 153 vB
refuse if  V < TOPUP + margin
```

**Every input, marked per `ADMISSION-INPUTS.md`'s taxonomy:**

| term | provenance | why |
|---|---|---|
| `V` = `cb.child_state.out_value` | **signature** | Bound to `st_out0.value()` by the `[value-gate spoof]` check (`clients/libs/rust/src/tesr.rs:12458-12471`), chained hop-by-hop back to `f_out.value` fetched from chain by the receiver (`:7355`), and forced by `link_pays_taproot_key` (`:181`) to be the output paying the **receiver's own key**. This is already the function's return value (`:7606`). **Safe.** |
| `d` | **structural** | `verify_child_bundle` links every tier to the outpoint above it, so a segment cannot be dropped to shorten the walk (`ADMISSION-INPUTS.md:104-107`). **Safe.** |
| `N`, `vB` | **config** | `const fn`s in the receiver's own SDK. **Safe.** |
| `c` | **config** | `TesrParams::for_network(&cc.network).committed_fee_rate` — already read this way just above the gate, precisely so the sender cannot move the yardstick (`:7450-7461`). **Never `cb.parent.fee_rate`.** **Safe.** |
| `r_live` | **backend** | `info_config(cc).fee_rate_sats_per_byte`, which despite the endpoint's name is **not** operator-supplied: the coordinator serves no fee rate at all, and the client fills the field from `estimate_fee(3)` on its own Electrum node (`clients/libs/rust/src/utils.rs:63-71`). Already fetched at `:7395`. **Safe.** |
| `margin` | **config** | The receiver's own policy. |
| 153 | **constant** | `estimate_child_vsize()` (`lib/src/wallet/p2a_fee_child.rs:108`) — a named constant now exists and this gate must call it rather than inline the §1 figure. |

**Three traps, all of which the existing code has already stepped in once:**

1. **Do NOT use `cb.parent.fee_rate`.** It is a `f64` on the conveyed bundle. `VALUE-CONSERVATION`
   V5 (`VALUE-CONSERVATION-SWEEP.md:300-314`) documents what happens: *"every floor scales with the
   lie, so nothing downstream catches it."* The sender would set the top-up to zero. The field is now
   pinned by exact equality against the receiver's own preset (`clients/libs/rust/src/tesr.rs:7450-7461`),
   which is a reason to read the preset directly — not a reason to trust the conveyed copy.
2. **Do NOT use `current_fee_rate` as computed at `:7425`.** That variable is
   `min(info_config.fee_rate, cc.max_fee_rate)`, and `max_fee_rate` is **1.0**
   (`clients/libs/rust/src/client_config.rs:70`) — it caps the flat-**backup** fee and is *below* the
   2.0 every ladder commits. Capping the viability rate downward makes the gate compute a top-up of
   zero at every rate. The existing code carries a comment warning about exactly this confusion
   (`clients/libs/rust/src/tesr.rs:7446-7449`: *"Two different quantities with similar names; the
   first draft of this check used the wrong one."*) — this gate must read the **uncapped** estimate.
   The cap itself is not the defect and is not this document's to remove: it is applied identically
   on every flat-backup path (deposit `:52`, withdraw `:67`, both transfer sides, `broadcast_backup_tx`
   `:56`), so it binds every holder of a flat backup symmetrically.
3. **Do NOT price at today's rate and call it done.** The payee exits after `2885 + 722d` blocks
   (≥ 20 days at depth 1). `r_live` is the rate now; the exit happens at a rate nobody knows.

   The obvious repair — price at `max(r_live, cc.stress_fee_rate)` — **has since been argued down in
   code, and the argument is good.** [D40.3] chose a walk-derived slack margin instead
   (`exit_slack_margin`, `lib/src/transfer/receiver.rs:908`) and states why in its own doc comment: a
   `stress_fee_rate` adds a number nobody can derive to the trust base, and *"at stress = 20 / depth
   10 it puts the minimum admissible piece above 125 000 sat"* — which is §2's own `V_min(10,20)` of
   **124 870**, i.e. it would refuse the entire payment range this document is trying to protect.
   That number is the reason, and it is this document's number.

   So the residual stands but the fix does not: a viability gate must be honest that it prices at
   `r_live` and that C-1 is therefore open, rather than buying a false sense of coverage with an
   exogenous stress constant. If a forward-looking term is wanted it must be **derived** — from the
   walk, from the epoch remainder — the way D40.3's margin is.

**What the gate would refuse today**, at the piece values the tree actually mints:

| piece V | d | r | reachable (V−980) | top-up | net | verdict |
|--:|--:|--:|--:|--:|--:|:--|
| 1 310 | 1 | 2 | 330 | 0 | +330 | admit |
| 1 310 | 1 | 10 | 330 | 11 744 | −11 414 | **refuse** |
| 1 310 | 10 | 20 | 330 | 123 890 | −123 560 | **refuse** |
| 10 000 | 1 | 5 | 9 020 | 4 604 | +4 416 | admit |
| 10 000 | 1 | 10 | 9 020 | 11 744 | −2 724 | **refuse** |
| 10 000 | 10 | 20 | 9 020 | 123 890 | −114 870 | **refuse** |
| 100 000 | 10 | 50 | 99 020 | 327 920 | −228 900 | **refuse** |

**A second, separable client bug this exposes.** The receiver books the **funding** value, not the
reachable one. On the live v4 claim path it books `new_key_info.amount`
(`clients/libs/rust/src/transfer_receiver.rs:1662`), which `get_new_key_info` fills from the SP
output (`tx0_output.value`, `lib/src/transfer/receiver.rs:1324`); the legacy v3 arm books the same
number directly (`coin.amount = Some(sp_out.value as u32)`, `:1591`). `verify_conveyed_child`'s
returned exit value is available at the claim site and thrown away there (`:1204`, called with `?`
and no binding).

**The pre-pay path is the counter-example, and it is the one to copy.** `peek_pending_transfers`
does use the returned value — `let amount = child_amount.unwrap_or(amount)` (`:978`) — but only after
`prepay_child_census` has bound the bundle to the latched sid. So the wallet already knows how to
book the censused number; the claim path simply does not. A piece is credited `V`, is worth `V − 980`
even in the best case, and is worth **nothing** below the band. That is worth fixing whether or not
the gate ships.

**Symmetric sender-side refusal.** `split_output_floors`
(`clients/libs/rust-sdk/src/transfer.rs:3220-3241`) is where the sender applies `min_child_value`
today, now via `SplitLegRole::Piece.min_value(rate, DUST_LIMIT)` and a shape-exhaustive match that
gives the change leg its own (lower, one-rung) floor. An honest sender should not *mint* a piece it
knows the recipient must refuse. This is the
same predicate on the same numbers, and it is the difference between a payment that fails at
`transfer()` and one that fails at `claim()` after the parent is already terminal.

### C. What NOTHING can enforce — the residual, stated

#### C-1. The rate rises after adoption

The gate in §B is evaluated **once**, at claim. The exit happens weeks later. A piece that was
economic when accepted is not economic now, and no admission check can reach backwards. The rate at
which an already-adopted piece goes under water:

| piece V | d | break-even `r*` (sat/vB) |
|--:|--:|--:|
| 1 310 | 1 | **2.01** |
| 10 000 | 1 | 8.09 |
| 10 000 | 10 | 3.11 |
| 100 000 | 10 | 16.34 |
| 1 000 000 | 66 | 26.61 |

**A piece minted at the admission floor is unrecoverable if the fee rate rises by one half of one
percent.** That is the plainest statement of the defect available.

**What mitigates it, honestly:**

* **Raising `c` (`committed_fee_rate`)** — §6 R-1. Genuinely moves the whole band, because below `c`
  the top-up is *zero*, not merely small. Cost: real sats burned in every rung, and it is a hard fork
  for existing ladders.
* **A prepaid exit reserve** — the sender hands over a separate funded UTXO alongside the piece.
  Does not exist; would be a new conveyed artifact with its own theft surface (the sender could spend
  it).
* **Watchtower-funded CPFP** — an operator-run tower bumps on the payee's behalf and bills them.
  Turns a protocol guarantee into a **service**, which is honest but must be labelled as such.
  **[D31, 2026-08-11] — decided, and now built for the whole-coin lane.** It is the OPTIONAL
  funded-tower variant: `watch_pass_with_bump` takes the capability as an explicit argument, so a
  tower cannot acquire it by accident, and the default `watch_pass` keeps its keyless meaning and
  reports a fee-stuck tier as a stated limit. The normative funder is the **owner**. None of this
  reaches a split piece — see C-2.
* **Accepting that a piece is a claim, not a bearer instrument** — the design choice. Below the band
  a piece is redeemable **cooperatively** and not otherwise. This is defensible; what is not
  defensible is failing to say it.

**What does NOT mitigate it, and must not be offered as if it did:**

* **A bigger margin at admission.** It shifts the threshold; it does not change that the threshold is
  evaluated at the wrong time.
* **CATS-B.** See §5.
* **`check_exit_headroom`, including D40.3's slack margin.** Both are time checks. A coin can have
  all the headroom in the world, and a quarter of its walk in spare margin, and still not be worth
  the fee.
* **The watchtower's event trigger.** `WatchTrigger` now exists
  (`clients/libs/rust-sdk/src/watchtower.rs:75-85`, and split leaves get their own bundle entry
  carrying both predicates, `:87-` onward), closing the §4.7 silent-degradation gap — a tower can now
  *notice* the race start on a piece. What it cannot do is act on it: the funded-bump path exists
  only on the whole-coin passes (C-2), so for a leaf the trigger fires into a keyless pass that can
  only re-broadcast at the committed rate.

#### C-2. The **piece's** lane still cannot fee-bump — the whole-coin lane now can

This is a hard implementation fact, not an economic one, and it changed in one direction only.

**What shipped (D31/#123).** `build_p2a_fee_child` (`lib/src/wallet/p2a_fee_child.rs:128`) builds a
v3 1P1C child that spends the parent's P2A anchor (input 0, no witness) plus an **owner-funded**
input (input 1), credits the parent's already-committed fee, and refuses rather than mis-relays when
the child would exceed TRUC's 1 000 vB, when the funding input cannot cover the fee, or when the
change would fall below `CHILD_CHANGE_DUST = 330` (`:143-174`). It is wired into `broadcast_tier`
(`clients/libs/rust/src/tesr.rs:9743`) behind an explicit `BumpCapability` (`:9643`), reachable from
`watch_pass_with_bump` (`:9872`) and `exit_pass_with_bump` (`:10060`), which the SDK's
`unilateral_exit` selects when a `fee_bump` source is configured
(`clients/libs/rust-sdk/src/wallet.rs:3534`, capability assembled at `:1607`). Absent that config the
call is the keyless one and a stuck tier is reported as a **stated limit**, not a retryable blip.

**What did not.** The arm immediately below that one — the split **CHILD** claim — calls plain
`exit_child_pass` (`clients/libs/rust/src/tesr.rs:7097`, dispatched at
`clients/libs/rust-sdk/src/wallet.rs:3567`), which broadcasts each pre-signed tier raw and stops at
the first that will not relay. It never builds a child, and there is no `exit_child_pass_with_bump`.
The spine tip's `exit_spine_tip_pass` is in the same position. `create_cpfp_tx`
(`lib/src/wallet/cpfp_tx.rs:46`) is no help and never was: it is v2, and it **rejects any transaction
with more than one output** (`:53`), while every tier has at least two (payload + P2A anchor).

So the asymmetry this document is about now has a second edge. **The sender's whole coin can be
rescued at market rate; the piece cannot be rescued at any price.** Above 2.0 sat/vB the payee's
tiers sit in the mempool at their committed rate and confirm only when the fee market falls back —
inside a window bounded by `epoch_expiry_height`, after which the sender's backup takes everything.
**The band in §2 is therefore the optimistic case for the piece**: it prices a child the piece's own
exit path cannot build, using a builder that exists two modules away.

#### C-3. The whole-coin lane has the same hole from the other end

`VALUE-CONSERVATION-SWEEP.md:526` scoped it out explicitly: *"Dust floors on the final exit leg,
relay/TRUC package limits, and fee-bumping economics were out of scope even where they bound the same
attacks."* There is no receiver-side viability floor anywhere — every floor in the tree
(`min_child_value`, `min_spine_tip_value`, `min_split_output`, `colored_child_floor`,
`colored_spine_tip_floor`) is applied by the **sender**, at mint time (`split_output_floors`,
`clients/libs/rust-sdk/src/transfer.rs:3220-3241`).

---

## 5. Does CATS-B change this? — magnitude yes, the option no, warning window worse

**Change 1 (landed) is orthogonal.** `SPINE_CSV = 0` (`clients/libs/rust/src/tesr.rs:6803`) enters
only `tesr_exit_wait_blocks` (`clients/libs/rust-sdk/src/config.rs:209`). `tesr_exit_txs` and
`tesr_exit_vbytes` are untouched. Change 1 moved **time**, not **cost**. Every number in §2 is
post-change-1.

**Change 2 has since landed too**, so what follows is a comparison of two SHIPPED shapes, not a
forecast. The spine batch is `spine_batch_split` (`clients/libs/rust/src/tesr.rs:5955`), its levels
are charged as one tier each (`SplitLevelShape::Spine`, `:6498-6505`), its change leg is a one-cap tip
(`min_spine_tip_value` = 820 sat at 2 sat/vB, `lib/src/tesr.rs:184`), and it is selected by the SDK
whenever the parent is a spine tip (`ManyRoute::SpineBatch`,
`clients/libs/rust-sdk/src/transfer.rs:822`). The two-tier child
lane priced in §2 is still live for root and child parents, so **both columns below are real coins
today** — the question is only which lane a given payment lands in.

Under the doc's §4.4 shape — payee's chain `[T, X_m, SP_1..SP_i, ext, state]`, `i + 4` transactions,
`500 + i·(125 + 43K)` vB:

| d | K=1 txs/vB | `V_min@20` | `V_min@50` | vs the two-tier lane @50 |
|--:|--:|--:|--:|--:|
| 1 | 5 / 668 | 27 004 | 69 844 | **identical** |
| 2 | 6 / 836 | 32 828 | 85 268 | −14% |
| 5 | 9 / 1 340 | 50 300 | 131 540 | −29% |
| 10 | 14 / 2 180 | 79 420 | 208 660 | −37% |
| 100 | 104 / 17 300 | 603 580 | 1 596 820 | −45% |

At K=20 the level count falls 20× but each `SP` grows to `125 + 43·20 = 985` vB. Net at r=50,
batch-depth 5 (≈100 payments): **327 620** vs the two-tier lane's **2 919 460** — an **8.9×**
improvement, matching the doc's §6 claim. (The d=100 row here, like §2's, is past the
23-transaction length cap — a model extreme, not a mintable shape; see §2.1.)

**Verdict:**

* **Helps** on magnitude, and only at depth ≥ 2. It attacks the `d` term. It cannot attack `2·rung`
  or the per-tier CPFP child, which is why d=1 is bit-identical under both
  (`500 + 168 = 668 = 375 + 293`).
* **Orthogonal** to the option. The sender's backup stays 112 vB and one transaction;
  `CHILD_V2_BASELINE` stays 0. The asymmetry at the depth most payees sit at (1) is unchanged at
  **5.96×**.
* **Hurts** the payee's ability to notice. §4.8:531-534 already publishes it: the on-chain warning
  before a steal confirms collapses from ~4.07 years to ~15.7 days. Under a *finality* reading that
  is not "a normal L2 assumption" — it is the window in which a sub-economic payee, who by
  construction will **not** spend `V_min > V` to defend, must nonetheless be watching. **CATS-B makes
  the free option faster to exercise while leaving it free.**

---

## 6. Recommendation, ranked by cost to build

### R-0. Say it. *(cost: this document)*

The band belongs in `TRUST-MODEL.md` as a named boundary and in the SDK's user-facing docs. A piece
below `V_min(d, r)` is **cooperatively redeemable, not unilaterally exitable**. Everything else here
narrows the band; nothing removes it. Shipping R-1..R-4 without R-0 replaces a known limit with a
smaller unknown one.

### R-1. Raise `committed_fee_rate`. *(cost: one constant + a migration story; the single largest effect)*

`TesrParams::mainnet().committed_fee_rate` (`lib/src/tesr.rs:256`, regtest `:261`). Below `c` the
top-up is **zero**, so raising `c` does not shrink the problem — it deletes it up to the new rate:

| c | rung | `min_child_value` | `V_min(1,20)` | `V_min(1,50)` | `V_min(10,20)` | `V_min(10,50)` |
|--:|--:|--:|--:|--:|--:|--:|
| **2 (shipped)** | 490 | 1 310 | 27 004 | 69 844 | 124 870 | 328 900 |
| 5 | 865 | 2 060 | 25 750 | 68 590 | 115 705 | 319 735 |
| 10 | 1 490 | 3 310 | 23 660 | 66 500 | 100 430 | 304 460 |
| **20** | 2 740 | 5 810 | **5 810** | 62 320 | **5 810** | 273 910 |
| 50 | 6 490 | 13 310 | 13 310 | **13 310** | 13 310 | **13 310** |

At `c = 20`, a depth-10 piece at 20 sat/vB drops from **124 870** to **5 810** — a **21×**
improvement — because no CPFP is needed at all.

**The cost, stated:** each rung burns `committed_fee(c) + 240`, paid at mint from the coin.

```
c= 2: 2 rungs =   980 sat  ( 9.8% of a 10 000-sat piece)
c= 5: 2 rungs = 1 730 sat  (17.3%)
c=10: 2 rungs = 2 980 sat  (29.8%)
c=20: 2 rungs = 5 480 sat  (54.8%)
```

**And it is a hard fork.** `verify_conveyed_child` enforces exact equality against the receiver's own
preset (`clients/libs/rust/src/tesr.rs:7450-7461`), so a client at `c=20` refuses every ladder built
at `c=2`. `c = 10` is the defensible compromise: 30% of a 10 000-sat piece, zero top-up up to
10 sat/vB. **Do not raise it without a same-commit answer for existing coins.**

### R-2. The receiver-side viability gate. *(cost: one function beside `check_exit_headroom_with_margin`, one error type, unit tests)*

Exactly §B. `ViabilityShortfall` alongside `ExitHeadroomShortfall` and `ExitChainTooLong` in
`lib/src/transfer/receiver.rs`, called from `clients/libs/rust/src/tesr.rs:7546` — the third gate in
a row that already refuses on time (with D40.3's margin) and on work. Pure arithmetic on terms
already in scope at that line; testable without a stack. Mirror it in `split_output_floors`
(`clients/libs/rust-sdk/src/transfer.rs:3220`) so the sender refuses to mint what the receiver will
refuse to take. **This closes admission. It does not close C-1.**

### R-3. Fix the booked value. *(cost: two lines + the tests that assert the old number)*

The claim path books the SP output value on both arms — `new_key_info.amount`
(`clients/libs/rust/src/transfer_receiver.rs:1662`, filled from `tx0_output.value`) on the live v4
path, `coin.amount = Some(sp_out.value as u32)` (`:1591`) on the legacy v3 one — while
`verify_conveyed_child`'s return is thrown away at `:1204`. Book that return instead, exactly as the
pre-pay path already does at `:978`. Every piece in every wallet is currently overstated by at least
`2·rung`.

### R-4. Give the **payee** a way to bump — the builder already exists. *(cost: a bump variant of one pass, plus the SDK arm that calls it)*

C-2. `build_p2a_fee_child` ships and is wired into `watch_pass_with_bump` / `exit_pass_with_bump`;
what is missing is the same wiring on `exit_child_pass` (and `exit_spine_tip_pass`) and the SDK arm
at `clients/libs/rust-sdk/src/wallet.rs:3567` that dispatches them. Until that lands, the P2A output
on every **piece** tier is 240 sat of pure overhead, the §2 band is unreachable in either direction
for a piece, and the whole-coin lane the sender holds is the only one that can pay its way out of a
fee spike. This is no longer the largest build in this list — it is a wiring job — and it is the one
that makes R-1's trade-off tunable rather than permanent.

### R-5. SSP policy. *(cost: one `if` in each of two functions)*

§A-4. Protects the SSP and everyone the SSP pays. **Does not protect a user-to-user payment**, and
must not be described as if it did.

### Explicitly NOT recommended

* **Any "the operator enforces a value" design.** §3.1. The SE is blind; a declared value is the
  attacker's own number.
* **Lowering `lockheight_init` as the primary fix.** It does bound the band (A-1) but it prices two
  independent things — the depth cap *and* `check_exit_headroom`'s available window — in opposite
  directions. Use it as a bound, not a fix. Since D8(f) it is not even reachable as config: it is a
  compiled-in constant on both sides, so proposing it is proposing a protocol change.
* **Treating CATS-B as the answer.** §5. It shrinks `d`, leaves the option free, and shortens the
  warning window — and it has now landed, so the option is free on a shipped lane, not a planned one.

---

## 7. See also

* [PARTIAL-PAYMENT-ECONOMICS.md](PARTIAL-PAYMENT-ECONOMICS.md) — §1.1 the per-payment ledger, §2 the
  cost curve, §4.8 the liveness trade (which this document reclassifies), §6 the CATS table this
  model reproduces.
* [ADMISSION-INPUTS.md](ADMISSION-INPUTS.md) — the provenance taxonomy §B is written against. **Read
  it before implementing R-2.**
* [VALUE-CONSERVATION-SWEEP.md](VALUE-CONSERVATION-SWEEP.md) — §7 (`:526`) scopes fee-bumping
  economics out; §8 records what the value laws do close.
* [TRUST-MODEL.md](TRUST-MODEL.md) — where R-0 belongs.
* [CHILDREN.md](CHILDREN.md) — the child bundle whose `parent_flat_backups` field hands the payee
  proof of the sender's cheaper exit.
