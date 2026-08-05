# Partial-payment economics — what a real payment costs, and what to build

> **Status: PARTLY SHIPPED.** Numbers are derived from constants read at `feat/spark` and cited
> `file:line`; §3.0 lists defects that existed independent of which design shipped, and most are now
> closed. **The "nothing in §4 exists in code yet" this line used to carry is false** — see the
> build-status block at the head of §4, which is authoritative: change 1 (the spine tier) is landed
> and change 2's VERIFIER half is landed with its producer deliberately gated off.
>
> **Read the ⚠️ CORRECTION inside §4.5 before implementing anything from §4.5** — it voids that
> section's stated safety argument, and the code now carries an executable disproof of it.

---

## 1. The correction, stated plainly

Utexo has been described — in [README.md](README.md), in [PARITY.md](PARITY.md), in the pitch — as
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
(`clients/libs/rust/src/tesr.rs:2508`) pushes the entire preceding segment into `ancestors`
(`:2609-2612`). There is no cap anywhere, and no reset: a child cannot be refreshed — `refresh()`
routes to `withdraw()`, which routes any `ctesr-` coin to `unilateral_exit` because `SP.out[j]` is
un-broadcast and there is no confirmed outpoint to co-operatively spend
(`clients/libs/rust-sdk/src/wallet.rs:1856-1878`), and `refresh` hard-refuses any RGB carrier
outright (`clients/libs/rust-sdk/src/refresh.rs:152-157`).

**BIP-68 relative timelocks are sequential**, so exit latency compounds:

```
WAIT(d) = 2124·d + 2160 blocks        [T 0 | X_m 720 | SP 1404 | (d−1)×(720+1404) | ext 720 | state 1440]
```

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
is a depth-1 coloured child that can be moved only **whole** or exited. `CARRIER_SEND_DEPTH = 5` and
`TOKEN_CARRIER_SATS = 17 384` (`clients/libs/rust-sdk/src/tokens.rs:113,126`) still size carriers for
five sends — a stale assumption from the retired flat-split lane. On the live CTES-R in-ladder lane
the real depth is **1**.

---

## 2. The baseline cost curve

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

### 3.0 Ship these first — they are defects today, under every design

None of these depend on which design wins. Two are exploitable now.

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
| baseline (today) | 2 046 (2 390) | — |
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
> to 720 (mainnet depth 1: 4 284 → 2 880 blocks; depth 100: 4.08 → 1.41 years).
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
> ordinary two-tier child, census `0 + 2 + 1`), which is refused by name today; and the **coloured**
> spine (§4.5 RGB items 1–3), where `cosign_colored_in_ladder_split` still carves a two-tier change
> and `refuse_uncolored_over_colored_tip` keeps a coloured tip out of the batch builder.

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
   (It is dead weight **today** too: `renew`/`rollover` take `&mut TesrBundle`
   (`clients/libs/rust/src/tesr.rs:3235,3265`) and there is no `ChildTesrBundle` analogue — a split
   child's extension can never be renewed. Both designs give the change exactly 36 whole-coin hops.)
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
2-way split. **Zero server diff, zero enclave diff, no new endpoint, no new cryptography** — which
matters given the SGX/lockbox lane divergence: nothing here lands on the signing lane at all.

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

**This does not bound depth — it makes each level cheap.** `s` still grows without limit, and the
only depth reset remains the root re-anchor, which a split tree does not have (§7).

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
>    own signature. Per `ADMISSION-INPUTS.md`, that makes shape **derived**, not **declared** — the
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

The cost is the piece's renewal rung. That rung is unreachable today (no child rollover exists), so
this is not a capability trade **now** — but it forecloses ever adding child renewal without a new
tier. Recommend landing CATS-B first and taking this as a separate, argued change.

### 4.7 Prerequisites that gate K > 1

- **P0-3** (crash-safe carve) must land first. The unrecoverable window is `2K + 2` SE round-trips
  wide: at K = 20 that is an 8.6× increase in independent failure points that destroy the whole coin.
- **Idempotent re-conveyance.** `in_ladder_pay_many` conveys the K pieces **serially**
  (`clients/libs/rust-sdk/src/transfer.rs:1352-1370`) after the parent is already terminal. A failure
  at recipient *j* loses bundles *j..K−1* permanently — the sender owns those slots' keys but has no
  tier hex and never will.
- **Coin selection must not eat its own inventory.** `plan_with_floor` sorts split candidates
  ascending and takes `.first()` (`clients/libs/rust-sdk/src/select.rs:86-131`), so a forecast miss
  splits the smallest **piece**, not the spine tip — deepening a payee and minting a stranded crumb.
  `Candidate` needs an `is_inventory` flag.
- **Derived-slot budget.** `max_derived_tokens_per_statechain = 64`
  (`server/src/server_config.rs:103`), counted over lifetime issuance including spent rows. K ≤ 63
  per spine level — and because each level is a **fresh** statechain, the cap is per-level, not
  global. But retry budget collapses: 31 failed attempts survive at K = 1, only 2 at K = 20.
  Mint slots lazily per child, or make `take_derived_tokens` recoverable.
- **The watchtower cannot express the trigger.** `WatchEntry` has exactly one trigger field,
  `deadline_block` (`clients/libs/rust-sdk/src/watchtower.rs:44-59`), and `watch_pass` evaluates one
  predicate, `tip + margin < deadline_block` (`:176-198`). CATS's race starts when the **sender**
  confirms `SP_i` — an event at no fixed height. The tower returns `Idle`, documented as "a positive
  observation: nothing needed doing" (`:164-166`), for the whole 1 440-block window. **This is the
  repo's silent-degradation shape and it must be fixed before CATS ships**: add
  `{ watch_txid, csv_blocks, push_txs }`, subscribe to the outpoint, and add a `Blind` state so an
  unevaluable entry never reports `Idle`.
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
> [SUBECONOMIC-FINALITY.md](SUBECONOMIC-FINALITY.md).

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
   is K′ on-chain re-anchors; and for **coloured** coins there is no recovery at all
   (`clients/libs/rust-sdk/src/refresh.rs:152-157`). Worse, the SSP then holds a co-signed `S'` at
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
| **baseline (today)** | 2 046 | 0 | 1 470 + 2 046N | 23 tx / 3 305 vB / 162 d | 203 tx / 29 675 vB / **4.08 yr** | 2 003 tx / 293 375 vB / **40.4 yr** |
| **CATS, K=1** | 1 556 | **0** | 1 470 + 1 556N | 13 tx / 2 055 vB / **15.1 d** | 103 tx / 17 175 vB / **15.7 d** | 1 003 tx / 168 375 vB / **21.9 d** |
| **CATS-B, K=10** | 1 115 | 0 | 1 470 + 1 115N | 4 tx / 930 vB / **15.0 d** | 13 tx / 5 925 vB / **15.0 d** | 103 tx / 55 875 vB / **15.7 d** |
| **CATS-B, K=20** | 1 091 | 0 | 1 470 + 1 091N | 4 tx / 1 360 vB / **15.0 d** | 8 tx / 5 300 vB / **15.0 d** | 53 tx / 49 625 vB / **15.3 d** |
| **CATS-B K=20 + lean leaf** | 601 | 0 | 1 470 + 601N | same | same | same, payee −720 blk |
| *DFO (rejected)* | 0 on-grid / `E[u/2]` off-grid | **1 066N − 86, recurring 9.2×/yr** | 1 384 + 1 066N | 5 tx / 1 012 vB / 29.8 d | *unreachable — tree is terminal* | — |
| *DENOM-SWAP (rejected)* | 0 on-lattice / `E[b/2]` off | K onboarding tokens + 11·43·r vB | K · 1 800 (flat) | 3 tx / 375 vB / 14.75 d | same | same |

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
`H_deposit + lockheight_init` (default 10 000 blocks ≈ 69.4 days,
`server/src/server_config.rs:82`; the mainnet compose profile uses 50 000 ≈ 347 days). `T` is
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

- **Raise `lockheight_init`** (the compose profile's 50 000 already does). Cheap, but it lengthens
  the depositor's clawback window and therefore the trust window. This is a policy dial, not a fix.
- **A co-operative de-trigger for terminal trees** — the SE co-signing a fresh spend of `F` after the
  tree is terminal. Requires raising the spend budget on a terminalized statechain *and* a protocol
  for invalidating every live child with its holder's consent. Hard; not designed.
- **A child re-anchor primitive.** Structurally impossible as posed: a child's funding `SP.out[j]` is
  un-broadcast, so there is no confirmed outpoint to spend, and producing one *is* the on-chain
  transaction you were trying to avoid.

**Also unchanged:**

- **The payee-borne 980 sat** (1 152 coloured) per received piece. Only the lean-leaf variant (§4.6)
  touches it, halving it to 490. Batching and the spine do not.
- **Depth still grows without bound.** CATS makes each level cost one tx and one block; batching
  divides the level count by K. Neither bounds it.
- **A child can never be refreshed or re-anchored** (`clients/libs/rust-sdk/src/wallet.rs:1856-1878`).
  The error string at `clients/libs/rust/src/tesr.rs:2671-2674` ("exit or re-anchor it instead of
  re-sending") instructs the user to do something the code refuses, and should be corrected to name
  unilateral exit as the only route.
- **A coloured carrier cannot be re-anchored at all** (`clients/libs/rust-sdk/src/refresh.rs:152-157`).
  Every coloured coin therefore dies at its root epoch and must be moved off-carrier first. This is
  the single biggest remaining blocker to the RGB half and is untouched by everything above.
- **The 36-hop CSV budget.** A child survives `(1440−144)/36 = 36` whole-coin handovers
  (`clients/libs/rust/src/tesr.rs:2664-2678`), with no renewal path. `CoinInfo`
  (`clients/libs/rust-sdk/src/types.rs:142-151`) exposes no `hops_remaining`, so no wallet can warn a
  user that a received coin is one hop from being exit-only.
- **Nothing is offline.** Every payment needs an authenticated derived-token draw and SE co-signs.
  CATS-B buys depth, latency and fees — not availability.

---

## 8. Build order

| phase | content | gates |
|---|---|---|
| **0** | P0-1 … P0-6 (§3.0) | none — these are live defects; P0-1 and P0-3 are exploitable |
| **1** | CATS spine, plain lane, K = 1: V1–V5 (§4.5) + watchtower event trigger (§4.7) | Phase 0; adversarial E2E on **sender-declared segment shape**, not the race check |
| **2** | Batching K > 1 on the spine tier (plain) | crash-safe carve, idempotent re-conveyance, `is_inventory` in `Candidate`, lazy slot minting |
| **2b** | Coloured spine: `TierRole::Spine`, N-deep seal schedule, **per-output blinding** | Phase 2; until blinding lands, coloured K > 1 only for mutually-known payees |
| **3** | Opt-in self-carve inventory (Mode B) for fixed-amount books | utilisation gate `> 0.685K + 0.315` enforced in the planner |
| **4** | Lean leaf (§4.6) — separate, argued decision | forecloses child renewal |

Everything before Phase 3 is unconditional. Phase 3 is the only place denominations appear, it is
opt-in, and it is **not** gated on an SSP swap — which cannot be built safely until the
`lightning_latch` holes in §5.2 are closed.
