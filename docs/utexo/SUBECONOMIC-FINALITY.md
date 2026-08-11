# Sub-economic finality — the sender's free option over a piece too small to defend

> **Status:** normative finding. Describes a live gap at `feat/spark` `fbfd985`. **Nothing in this
> document is built.** Section 6 ranks what to build.
>
> Line numbers in `clients/libs/rust/src/tesr.rs` are as of `fbfd985`; that file is under concurrent
> edit (CATS-B change 2). Every citation elsewhere is against the working tree.

## 0. The finding in one paragraph

A split piece's only route to Bitcoin is its tier chain. It has **no flat backup** —
`CHILD_V2_BASELINE = 0` (`clients/libs/rust/src/tesr.rs:3891`), because a child slot is an
SE-registered key that is never funded on-chain, so `check_deposit`/`create_tx1` never run for it.
The **sender**, holding the parent, keeps a flat backup that spends the on-chain funding output `F`
directly and pays them the whole coin. Walking the piece's chain costs the payee `3 + 2d`
transactions and `293d + 375` vB (`clients/libs/rust-sdk/src/config.rs:166,181`); broadcasting the
flat backup costs the sender **one 112-vB transaction** (`BACKUP_TX_SIZE`,
`lib/src/transaction.rs:116`). The protocol admits a piece at **1 310 sat**
(`min_child_value(2.0, 330)`, `lib/src/tesr.rs:114-120`, asserted `:953`). At 20 sat/vB a depth-10
piece costs **124 870 sat** to defend. `check_exit_headroom` (`lib/src/transfer/receiver.rs:732`)
checks the exit fits in **time**. Nothing checks it is **worth doing**.

**It needs no malice.** After `in_ladder_split`, `set_spend_budget(parent, 1)` is consumed by `SP`
and the SE returns 410 Gone for any further co-sign (`server/src/endpoints/sign.rs:105-113`), so the
sender's two remaining routes out are: walk their own ladder (`293d + 375` vB), or broadcast the flat
backup (112 vB). **The backup is cheaper at every fee rate, by 6× to 265×.** One backup voids the
entire tree, so the marginal cost of taking each additional piece along is exactly **zero**. An
ordinary sender exiting for their own reasons voids every sub-economic piece they ever paid.

This is sharper than `PARTIAL-PAYMENT-ECONOMICS.md` §4.8, which frames sub-economic payees as a
**liveness** cost borne by the payee. It is a **finality** limit, and it is the **sender's** option.

---

## 1. The break-even function

Constants, all read from the tree:

| symbol | value | source |
|---|--:|---|
| `TIER_VBYTES` | 125 | `lib/src/tesr.rs:73` |
| `P2TR_OUT_VBYTES` | 43 | `lib/src/tesr.rs:439` |
| `P2A_VALUE` | 240 | `lib/src/tesr.rs:41` |
| `DUST_LIMIT` | 330 | `lib/src/transaction.rs:120` |
| `BACKUP_TX_SIZE` | 112 | `lib/src/transaction.rs:116` |
| `COLORED_TIER_VBYTES` | 168 | `clients/libs/rust/src/rgb.rs:916` |
| `committed_fee_rate` **c** | **2.0** | `lib/src/tesr.rs:190` (mainnet), `:195` (regtest) |
| `committed_fee(r)` | `ceil(125·r)` | `lib/src/tesr.rs:83-85` |
| rung = `committed_fee(c) + P2A` | **490** (coloured **576**) | `lib/src/tesr.rs:89-101` (`tier_out_value`) |
| `N(d)` = exit transactions | `3 + 2d` | `clients/libs/rust-sdk/src/config.rs:166-168` |
| `V_walk(d)` = exit vbytes | `293d + 375` | `clients/libs/rust-sdk/src/config.rs:181-188` |

**The CPFP child.** No constant for it exists in the tree; the only published figure is the
"~152-vB child" at `PARTIAL-PAYMENT-ECONOMICS.md:650-651`. Derived with the same byte accounting the
tier uses (`lib/src/tesr.rs:44-58`): P2A input 41 B + P2TR key-spend input 41 B + one P2TR output
43 B + header/counts 10 B = **135 B** base; witness = 2 (marker/flag) + 1 (P2A empty stack) + 66
(P2TR) = **69 B**; weight `135·3 + 204 = 609` WU ⟹ **152.25 vB**, floored to 152.

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
V_min(d, 2.0) = 980 + 330 = 1310 = min_child_value(2.0, 330)      lib/src/tesr.rs:114-120
```

`min_child_value` is **not** a floor that ignores economics. It *is* `V_min`, evaluated at
`r = committed_fee_rate`. It is exactly correct at 2.0 sat/vB and at no other rate. The defect is
therefore not "a missing check" — it is **a correct check frozen to a hardcoded constant**
(`lib/src/tesr.rs:190`). That is a much narrower, and much more fixable, statement.

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

Coloured (rung 576, floor `colored_child_floor(2.0, 330) = 1482`,
`clients/libs/rust/src/tesr.rs:2251`) is the same table shifted by **+172** at every cell: d=1 r=20 →
27 176; d=10 r=50 → 329 072. **Colour is irrelevant to this finding** — the top-up dominates the
rung.

A 10 000-sat piece, net after defending (`V − 980 − TOPUP`):

| | r=5 | r=10 | r=20 | r=50 |
|---|--:|--:|--:|--:|
| d=1 | **+4 416** | −2 724 | −17 004 | −59 844 |
| d=10 | −12 855 | −46 860 | −114 870 | −318 900 |

**A depth-1 10 000-sat piece goes under water between 5 and 10 sat/vB.** That is the modal payment
in the design's own simulation.

### 2.1 The reachable ceiling

Depth 100 cannot be minted on mainnet. `enforce_split_depth_cap`
(`clients/libs/rust/src/tesr.rs:3819`) evaluates `max_split_depth`
(`lib/src/transfer/receiver.rs:759-770`) on the live schedule, with `epoch_blocks = info.initlock`
(`:3832`):

```
base      = [None, 720, 0, 720, 1440]   -> exit_wait_blocks = 2885
per_level = [720, 0]                    -> exit_wait_blocks =  722
lockheight_init = 10 000 (server/src/server_config.rs:82)  -> max depth 10
lockheight_init = 50 000 (compose profile)                 -> max depth 66
```

So the numbers to quote are:

```
d= 10 (default epoch):  23 tx /  3 305 vB -> V_min@50 =   328 900 sat = 0.00329 BTC (  251x the floor)
d= 66 (compose epoch): 135 tx / 19 713 vB -> V_min@50 = 1 940 804 sat = 0.01941 BTC ( 1482x the floor)
```

### 2.2 The sender's side, and the ratio

The flat backup is one 1-in-1-out P2TR: 112 vB. Its fee is committed at signing and every receiver
re-validates it inside a ±5 sat/vB band (`fee_rate_tolerance = 5.0`,
`clients/libs/rust/src/client_config.rs:66`; enforced `lib/src/transfer/receiver.rs:471-476`), so it
is normally already at rate. If it needs a bump, `create_cpfp_tx` (`lib/src/wallet/cpfp_tx.rs:46`)
adds a second 1-in-1-out P2TR — a 224-vB package worst case.

The vbyte ratio is the one number that does not depend on the fee rate:

```
d=  1:    668/112 =   6.0x      5 tx vs 1
d=  2:    961/112 =   8.6x      7 tx vs 1
d=  5:  1 840/112 =  16.4x     13 tx vs 1
d= 10:  3 305/112 =  29.5x     23 tx vs 1
d=100: 29 675/112 = 265.0x    203 tx vs 1
```

**6× is the best case for the payee.** It is the ratio at depth 1, and it degrades linearly with the
sender's payment history.

The payee cannot wait the sender out: once the flat backup confirms, `F` is spent and the whole tree
is dead. The payee must push `T` first, at its own cost — the full walk is the payee's, never shared.
And the payee is handed the evidence: the sender's backup chain is conveyed as
`ChildTesrBundle::parent_flat_backups` (`clients/libs/rust/src/tesr.rs:374`), so the payee holds
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
  user_public_key }` (`lib/src/deposit/mod.rs:55-76`) — no amount.
* **There is no `amount` column in the server database or in any migration**
  (`grep -rn amount server/src/database/ server/migrations/` returns nothing).
* `/info/config` serves `{ initlock, interval, batchtimeout, version }` and nothing else
  (`server/src/endpoints/utils.rs:120-137`).

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
  (`server/src/endpoints/sign.rs:108-117`), re-checked at `sign/second`
  (`server/src/endpoints/sign.rs:292-300`). The sender cannot mint a **new** backup. The backups
  they **already hold** are pre-signed, valid Bitcoin transactions. **No operator can un-sign them.**
  This is the residual, and it is structural.
* **"The operator escrows the flat backups and refuses to serve them."** Rejected: the sender
  assembled and stored them locally at deposit and at every transfer hop. There is nothing to
  withhold.
* **"The SE refuses to co-sign a tier below `min_child_value`."** Rejected: same blindness, and the
  floor it would need is a function of the *live fee rate*, which the SE also does not have — it
  serves no fee rate at all (`server/src/endpoints/utils.rs:120-137`); the client computes it from
  its own Electrum backend (`clients/libs/rust/src/utils.rs:24`).

---

## 4. The three buckets

### A. What the operator CAN enforce, without seeing a single amount

Four levers exist. **None of them is a fix**; each is a value-blind bound on the *shape* of the
exposure, and they are worth stating because they are the whole of what the operator side can do.

#### A-1. `lockheight_init` is a direct ceiling on the payee's worst-case defence cost

The epoch length is operator config (`server/src/server_config.rs:82`, served as `initlock`). It
feeds `enforce_split_depth_cap` (`clients/libs/rust/src/tesr.rs:3832`), which caps depth, which
caps `V_walk(d)`, which caps `V_min`. The operator therefore chooses the top of the band:

| `lockheight_init` | max depth | exit txs | exit vB | `V_min@20` | `V_min@50` | × floor @50 |
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
finding**, which is the opposite of the direction `check_exit_headroom` wants. Whoever sets this
number should be told it prices two things at once.

#### A-2. `max_derived_tokens_per_statechain` bounds the fan-out per parent

`64` (`server/src/server_config.rs:103`), enforced as a **lifetime** per-parent allowance at
`POST /deposit/get_derived_token` (`server/src/endpoints/deposit.rs:134-205`), fail-closed on a read
error (`:186-194`), and `0` disables the endpoint outright (`:142-147`). This caps K ≤ 63 pieces plus
change per split level. It is a **count**, so it is perfectly value-blind, and it bounds *how many*
sub-economic pieces one 112-vB backup can void — never *how small* they are.

#### A-3. The terminality and census gates are already maximal, and they do not touch this

`set_spend_budget` (`server/src/endpoints/lightning_latch.rs:295`) with `remaining ∈ {0,1}` (`:297`),
`is_single_use`, the epoch deadline, the pending-transfer lock, and the `num_sigs` census are all
enforced at **both** `sign/first` and `sign/second` (`server/src/endpoints/sign.rs:74-135` and
`:255-330`), all fail-closed. They prevent a **second conflicting signature**. The sender's flat
backup is not a second signature — it is the **first** one, issued at deposit, long before the split
existed. There is no gate to add here.

#### A-4. The SSP — the one operator-run component that DOES see values

This is the real bucket-A answer, and it is real because the SSP is **not the SE**. It is an
SDK client with its own wallet, its own Electrum backend, and full sight of every value.

* **Pre-pay (`SspService::execute_pay`, `clients/libs/rust-sdk/src/ssp.rs:380`).** It already runs a
  census gate (`ladder_census_ok`, `:456-464`) and a value gate (`check_latched_coins`, `:292-315`,
  called `:519`) on `PendingTransferInfo::amount` — which for a child bundle is the **census-bound**
  exit value, not a sender hint (`clients/libs/rust/src/transfer_receiver.rs:255-257`).
  **What to add:** in the same block, refuse any latched coin whose
  `child_state.out_value − TOPUP(d, r_live)` is below a configured margin, with `d` from
  `cb.ancestors.len() + 1` and `r_live` from the SSP's own estimator. One `if`, in a function that
  already refuses by name.
* **Receive (`SspService::create_receive`, `:583`, and `largest_laddered_coin`, `:651`).** The SSP
  **mints** the piece here. It must not mint one it would refuse to accept. Same predicate, applied
  to the leg it is about to build.

This is an operator **policy**, not a protocol rule: a market participant declining inventory it
cannot redeem, exactly as a merchant declines dust. It protects the SSP and every user who is paid
*by* the SSP. It protects nobody in a user-to-user payment. **Say so plainly** — an operator who
ships A-4 and calls the finding closed has closed one lane out of two.

### B. What only the CLIENT can enforce — the real fix

The gate belongs **beside `check_exit_headroom`, in `verify_conveyed_child`**, at
`clients/libs/rust/src/tesr.rs:4332` (the `check_exit_headroom` call site), inside the function at
`:4152`. It is the same shape as the existing gate: receiver-derived, fail-closed, refusing by name.

**What it computes:**

```
V      = cb.child_state.out_value          the value the payee can actually reach on chain
d      = cb.ancestors.len() + 1
N, vB  = tesr_exit_txs(d), tesr_exit_vbytes(d)     config.rs:166,181
TOPUP  = 0                                  r_live <= c
       = ceil((r_live − c)·vB) + N·(ceil(152·r_live) − 240)
refuse if  V < TOPUP + margin
```

**Every input, marked per `ADMISSION-INPUTS.md`'s taxonomy:**

| term | provenance | why |
|---|---|---|
| `V` = `cb.child_state.out_value` | **signature** | Bound to `st_out0.value()` by the `[value-gate spoof]` check (`clients/libs/rust/src/tesr.rs:7430`), chained hop-by-hop back to `f_out.value` fetched from chain by the receiver (`:4184`), and forced by `link_pays_taproot_key` to be the output paying the **receiver's own key** (`:7415`). This is already the function's return value (`:4382`). **Safe.** |
| `d` | **structural** | `verify_child_bundle` links every tier to the outpoint above it, so a segment cannot be dropped to shorten the walk (`ADMISSION-INPUTS.md:104-107`). **Safe.** |
| `N`, `vB` | **config** | `const fn`s in the receiver's own SDK. **Safe.** |
| `c` | **config** | `TesrParams::for_network(&cc.network).committed_fee_rate` — already read this way just above the gate, precisely so the sender cannot move the yardstick (`:4276-4286`). **Never `cb.parent.fee_rate`.** **Safe.** |
| `r_live` | **backend** | `info_config(cc).fee_rate_sats_per_byte`, which despite the endpoint's name is **not** operator-supplied: it is `client_config.electrum_client.estimate_fee(3)` from the receiver's own node (`clients/libs/rust/src/utils.rs:23-31`). Already fetched at `:4245`. **Safe.** |
| `margin` | **config** | The receiver's own policy. |
| 152 | **constant** | Derived in §1. Should be introduced as a named constant beside `TIER_VBYTES`, not written inline. |

**Three traps, all of which the existing code has already stepped in once:**

1. **Do NOT use `cb.parent.fee_rate`.** It is a `f64` on the conveyed bundle. `VALUE-CONSERVATION`
   V5 (`VALUE-CONSERVATION-SWEEP.md:300-314`) documents what happens: *"every floor scales with the
   lie, so nothing downstream catches it."* The sender would set the top-up to zero.
2. **Do NOT use `current_fee_rate` as computed at `:4251`.** That variable is
   `min(info_config.fee_rate, cc.max_fee_rate)`, and `max_fee_rate` is **1.0**
   (`clients/libs/rust/src/client_config.rs:70`) — it caps the flat-**backup** fee and is *below* the
   2.0 every ladder commits. Capping the viability rate downward makes the gate compute a top-up of
   zero at every rate. The existing code carries a comment warning about exactly this confusion
   (`clients/libs/rust/src/tesr.rs:4270-4274`: *"Two different quantities with similar names; the
   first draft of this check used the wrong one."*) — this gate must read the **uncapped** estimate.
3. **Do NOT price at today's rate and call it done.** The payee exits after `2885 + 722d` blocks
   (≥ 20 days at depth 1). `r_live` is the rate now; the exit happens at a rate nobody knows. The
   gate should price at `max(r_live, cc.stress_fee_rate)` with a receiver-configured stress rate.
   This narrows §C but does not close it.

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
reachable one: `coin.amount = Some(sp_out.value as u32)`
(`clients/libs/rust/src/transfer_receiver.rs:1325`), while `verify_conveyed_child`'s returned exit
value is discarded at the claim site (`:979`). So a piece is credited `V`, is worth `V − 980` even in
the best case, and is worth **nothing** below the band. The wallet's own balance overstates every
piece by at least two rungs. That is worth fixing whether or not the gate ships.

**Symmetric sender-side refusal.** `split_output_floors`
(`clients/libs/rust-sdk/src/transfer.rs:2634-2641`) is where the sender applies `min_child_value`
today. An honest sender should not *mint* a piece it knows the recipient must refuse. This is the
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
  **[D31, 2026-08-11]** This is now a decided position rather than an option: it is offered as the
  OPTIONAL funded-tower variant, and it is labelled as such. The default tower is keyless and
  **cannot** bump; the normative funder is the **owner**.
* **Accepting that a piece is a claim, not a bearer instrument** — the design choice. Below the band
  a piece is redeemable **cooperatively** and not otherwise. This is defensible; what is not
  defensible is failing to say it.

**What does NOT mitigate it, and must not be offered as if it did:**

* **A bigger margin at admission.** It shifts the threshold; it does not change that the threshold is
  evaluated at the wrong time.
* **CATS-B.** See §5.
* **`check_exit_headroom`.** It is a time check. A coin can have all the headroom in the world and
  still not be worth the fee.
* **The watchtower's event trigger.** `WatchTrigger` now exists
  (`clients/libs/rust-sdk/src/watchtower.rs:55-65`, wired at `:88`), closing the §4.7
  silent-degradation gap — a
  tower can now *notice* the race start. It still has no funds and no code to bump.

#### C-2. The payee cannot fee-bump a tier **at all**, with the shipped client

This is a hard implementation fact, not an economic one. `exit_child_pass`
(`clients/libs/rust/src/tesr.rs:4093-4118`) broadcasts each pre-signed tier raw and stops at the
first that will not relay. It never builds a child. The only fee-bumping code in the tree,
`create_cpfp_tx` (`lib/src/wallet/cpfp_tx.rs:46`), takes a `BackupTx` and **rejects any transaction
with more than one output** (`:53`) — and every tier has at least two (payload + P2A anchor). So
**no code path in this repository can spend a P2A anchor.**

The 152-vB child priced throughout §1–§2 is a **design figure from the economics doc, not shipped
behaviour**. Today, above 2.0 sat/vB the payee's tiers sit in the mempool at their committed rate
and confirm only when the fee market falls back — inside a window bounded by
`epoch_expiry_height`, after which the sender's backup takes everything. **The band in §2 is
therefore the optimistic case.** The pessimistic case is that the payee cannot pay at any price.

#### C-3. The whole-coin lane has the same hole from the other end

`VALUE-CONSERVATION-SWEEP.md:526` scoped it out explicitly: *"Dust floors on the final exit leg,
relay/TRUC package limits, and fee-bumping economics were out of scope even where they bound the same
attacks."* There is no receiver-side viability floor anywhere — every floor in the tree
(`min_child_value`, `min_spine_tip_value`, `min_split_output`, `colored_child_floor`) is applied by
the **sender**, at mint time (`clients/libs/rust-sdk/src/transfer.rs:2634-2641`).

---

## 5. Does CATS-B change this? — magnitude yes, the option no, warning window worse

**Change 1 (landed) is orthogonal.** `SPINE_CSV = 0` (`clients/libs/rust/src/tesr.rs:3916`) enters
only `tesr_exit_wait_blocks` (`clients/libs/rust-sdk/src/config.rs:201-207`). `tesr_exit_txs` and
`tesr_exit_vbytes` are untouched. Change 1 moved **time**, not **cost**. Every number in §2 is
post-change-1.

**Change 2 (+3)**, under the doc's §4.4 shape — payee's chain `[T, X_m, SP_1..SP_i, ext, state]`,
`i + 4` transactions, `500 + i·(125 + 43K)` vB:

| d | K=1 txs/vB | `V_min@20` | `V_min@50` | vs live @50 |
|--:|--:|--:|--:|--:|
| 1 | 5 / 668 | 27 004 | 69 844 | **identical** |
| 2 | 6 / 836 | 32 828 | 85 268 | −14% |
| 5 | 9 / 1 340 | 50 300 | 131 540 | −29% |
| 10 | 14 / 2 180 | 79 420 | 208 660 | −37% |
| 100 | 104 / 17 300 | 603 580 | 1 596 820 | −45% |

At K=20 the level count falls 20× but each `SP` grows to `125 + 43·20 = 985` vB. Net at r=50,
batch-depth 5 (≈100 payments): **327 620** vs the live **2 919 460** — an **8.9×** improvement,
matching the doc's §6 claim.

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

`lib/src/tesr.rs:190`. Below `c` the top-up is **zero**, so raising `c` does not shrink the problem —
it deletes it up to the new rate:

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
preset (`clients/libs/rust/src/tesr.rs:4276-4286`), so a client at `c=20` refuses every ladder built
at `c=2`. `c = 10` is the defensible compromise: 30% of a 10 000-sat piece, zero top-up up to
10 sat/vB. **Do not raise it without a same-commit answer for existing coins.**

### R-2. The receiver-side viability gate. *(cost: one function beside `check_exit_headroom`, one error type, unit tests)*

Exactly §B. `ViabilityShortfall` alongside `ExitHeadroomShortfall` in
`lib/src/transfer/receiver.rs`, called from `clients/libs/rust/src/tesr.rs:4332`. Pure arithmetic on
terms already in scope at that line; testable without a stack. Mirror it in `split_output_floors`
(`clients/libs/rust-sdk/src/transfer.rs:2634`) so the sender refuses to mint what the receiver will
refuse to take. **This closes admission. It does not close C-1.**

### R-3. Fix the booked value. *(cost: one line + the tests that assert the old number)*

`coin.amount = Some(sp_out.value as u32)` (`clients/libs/rust/src/transfer_receiver.rs:1325`) should
book `verify_conveyed_child`'s return — the value already computed and discarded at `:979`. Every
piece in every wallet is currently overstated by at least `2·rung`.

### R-4. Give the payee a way to bump. *(cost: a real feature — a P2A spender, coin selection, tests)*

C-2: nothing in this repository can spend a P2A anchor. Until that exists, the P2A output on every
tier is 240 sat of pure overhead and the §2 band is unreachable in either direction. This is the
largest build and the one that makes R-1's trade-off tunable rather than permanent.

### R-5. SSP policy. *(cost: one `if` in each of two functions)*

§A-4. Protects the SSP and everyone the SSP pays. **Does not protect a user-to-user payment**, and
must not be described as if it did.

### Explicitly NOT recommended

* **Any "the operator enforces a value" design.** §3.1. The SE is blind; a declared value is the
  attacker's own number.
* **Lowering `lockheight_init` as the primary fix.** It does bound the band (A-1) but it prices two
  independent things — the depth cap *and* `check_exit_headroom`'s available window — in opposite
  directions. Use it as a bound, not a fix.
* **Treating CATS-B as the answer.** §5. It shrinks `d`, leaves the option free, and shortens the
  warning window.

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
