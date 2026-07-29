# Sending partial amounts — the granularity deep dive

How a system whose on-chain unit is an indivisible UTXO lets you send 0.1 BTC from a 1-BTC coin,
or 0.1 of a token from a 1.0 allocation — without touching the chain, without a third party, and
without the operator ever learning an amount. This page is the long-form explainer; the normative
requirements live in [GRANULARITY-SPEC.md](../GRANULARITY-SPEC.md) (GRN-REQ/GRN-INV/GRN-ERR), the
transfer basics in [transfers.md](transfers.md), token basics in [tokens.md](tokens.md), the
invalidation machinery this rides on in
[invalidation-deep-dive.md](invalidation-deep-dive.md) and
[INVALIDATION-SPEC.md](../INVALIDATION-SPEC.md) (IVL-*), fee/size tables in
[invalidation-economics.md](../research/invalidation-economics.md), and the granularity-specific
pricing (unpayable-amounts map, carrier depletion, token breakevens, colored exit at depth) in
[granularity-economics.md](../research/granularity-economics.md). Audience: technical readers
who have not read the code. Every number is computed from constants and measurements cited in
those documents or verified in the code paths named inline; open limitations are stated, not
rounded off ([AUDIT-2026-07.md](../AUDIT-2026-07.md)).

Contents: [the problem](#1-the-problem-utxos-dont-come-in-the-amount-you-owe)
· [mechanics](#2-mechanics-with-real-numbers)
· [effect on invalidation](#3-what-a-partial-send-does-to-invalidation)
· [effect on unilateral exit](#4-what-a-partial-send-does-to-unilateral-exit)
· [real-world situations](#5-real-world-situations) · [UX](#6-the-ux-perspective)
· [FAQ](#7-faq) · [recap](#8-comparison-recap-granularity).

---

## 1. The problem: UTXOs don't come in the amount you owe

Bitcoin's unit of ownership is the UTXO, and a UTXO is indivisible: you spend all of it or none
of it. On-chain, "sending 0.1 BTC from a 1-BTC output" means broadcasting a transaction with a
payment output and a change output — divisibility is bought with a chain transaction every time.
Every L2 that moves whole UTXOs off-chain (statechains, Spark leaves, Ark vouts) inherits the
indivisibility and must answer the same question: how does a user pay an *arbitrary* amount when
the things being transferred are fixed-size?

The deployed answers form a small design space. **Spark** keeps fixed leaf denominations and
makes arbitrary amounts a service: the SSP swaps your leaves for a set that sums right
(server-side pools, a third party in every odd-amount payment). **Ark** re-mints amounts every
round: the ASP includes your desired denominations as fresh vouts in the next round transaction —
arbitrary amounts, but only at round cadence and only through the ASP. **Lightning** has the
finest nominal resolution (millisats) but bounds every payment by channel capacity and inbound
liquidity — the amount you can receive is a provisioned resource, not a right. **This system**
makes partial amounts a native off-chain operation: one SE-co-signed, *un-broadcast* transaction
splits a coin into an exact piece plus change, both of which are immediately first-class off-chain
coins ([SPEC.md](../SPEC.md) REQ-15, INV-22). No third party, no round, no
liquidity table — and because the SE co-signs blindly, granularity costs nothing in the trust
model: the SE never sees a single amount. The comparison is tabulated in §8.

**One protocol, two coin shapes.** There is one protocol. `claim()` establishes a TES-R exit ladder
(trigger `T` → extension `X_m` → state `S`, relative CSV, all un-broadcast) for every fresh
confirmed **root** coin, unconditionally — no protocol-version switch, no legacy lane
(`clients/libs/rust-sdk/src/wallet.rs`, `claim`). Granularity therefore runs on two coin *shapes*,
both current, and every number below is tagged with the one it belongs to:

- **Laddered** — every plain deposit. A partial payment is an **in-ladder split**: a split state
  `SP` spends `X_m.out[0]`, so it *descends* from the trigger rather than racing it for the funding
  outpoint `F` [B1]. It pays a piece child (conveyed straight to the recipient) and a change child.
  Relative CSV throughout: no absolute deadline, zero idle rent, and depth costs pre-signed tiers
  rather than ladder capacity.
- **Un-laddered** — an RGB **carrier** is deliberately never laddered, because a plain tier spend
  would destroy its allocation (terminal freeze, [PROTOCOL.md §5.10](../PROTOCOL.md); `sdk52`), and a
  split sub-coin cannot be laddered either — its funding is an un-broadcast split output, so a
  trigger would have no prevout to spend [B0]. These coins keep the signed-once backup chain and the
  locktime-0 split tx, and they transfer by backup-chain handover. This shape is load-bearing, not
  dead code: every colored (token) split of §2b, §5.2 and §5.6 lives here.

`transfer()` routes a split plan by the selected parent's shape — a received child to
`child_in_ladder_pay`, a laddered root to `in_ladder_pay`, an un-laddered coin to `split_coin`
(`transfer.rs`) — so the caller never picks a lane.

## 2. Mechanics, with real numbers

Three walkthroughs: plain sats, tokens, and the multi-recipient batch. All constants below are
from `clients/libs/rust-sdk` (`transfer.rs`, `tokens.rs`, `select.rs`), `lib/src/tesr.rs` (the tier
fee model) and `clients/libs/rust/src/rgb.rs`; the arithmetic is exact.

### 2a. Sending 0.1 BTC from a 1-BTC coin (the in-ladder split)

Alice holds one confirmed coin of 100,000,000 sats (1 BTC — comfortably inside the u32 per-coin
cap of ~42.9 BTC, [SPEC.md §14](../SPEC.md#14-known-limitations-adversarial-review)) and calls
`transfer(bob_address, 10_000_000)`. Her deposit was laddered at `claim()`, so this is the
**laddered** lane; the un-laddered lane is the box at the end of this section.

**Step 1 — plan.** The planner (`select::plan_with_floor`) first looks for an *exact subset* of
coins summing to 10,000,000 (dynamic programming over reachable sums). One 1-BTC coin has no such
subset, so the plan is `WithSplit`: split the 1-BTC coin, piece = 10,000,000. `transfer()` then
dispatches on the parent's shape — laddered root ⇒ `in_ladder_pay`.

**Step 2 — admission arithmetic** (the pure guard run *before* anything is touched). The tier fee
model replaces the reserve: each pre-signed tier burns a **committed fee** plus a P2A anchor, so
what the two children share is `tier_out_total(X_m.out[0], 2, rate)`. At the deployed 2 sat/vB
committed rate (`TesrParams::mainnet`), with `committed_fee = ceil(124·2) = 248` and
`P2A_VALUE = 240`:

```
T.out    = 100,000,000 − (248 + 240)                      = 99,999,512   (trigger, at claim())
X_0.out  =  99,999,512 − (248 + 240)                      = 99,999,024   (extension, at claim())
split total = X_0.out − (committed_fee_for_outputs(2) + P2A)
            =  99,999,024 − (334 + 240)                   = 99,998,450
piece  = 10,000,000  (exact, requested)
change = 99,998,450 − 10,000,000                          = 89,998,450   (derived, not free)
checks: piece  ≥ min_child_value(2.0, 330) = 2·(248+240)+330 = 1,306 ✓
        change ≥ 1,306                                                    ✓
```

`min_child_value` is the load-bearing floor: an in-ladder child is not a bare output — it gets its
**own** headless ladder (an extension tier and a state tier), each burning `committed_fee + P2A`,
and its final state output must still clear dust. **D1, fixed:** the guard used to be the
un-laddered backup floor (442 sats at 1 sat/vB), so a child in `[442, 1306)` was *admitted*, the
parent was terminalized, and the child ladder then failed with `FeeTooHigh` — stranding the parent
to unilateral-exit-only. Both floors now apply and the larger binds, checked before the parent is
touched (`transfer.rs`, `max(min_split_output(rate), min_child_value(parent.fee_rate, DUST_LIMIT))`).

**Step 3 — make the parent terminal, then co-sign `SP`.** The SDK sets the parent's spend budget
at the SE to *exactly one more signature* and uses it on the split state (REQ-18). The SE co-signs
**blind** — blind-MuSig2 hands it a challenge derived from the tx, never the tx: it learns that
statechain id `P` signed *something*, not that 1 BTC became 0.1 + 0.89998. The result is a
fully-signed transaction that is **not broadcast**:

```
       split state SP (v3, relative CSV, un-broadcast, committed fee 334 sats)
            ┌──────────────────────────────────────────────────┐
 X_0.out ──▶│ in:  X_0.out[0]  (99,999,024, 2-of-2, CSV)        │
 (extension │ out0: 10,000,000 → A_piece   "piece child"        │──▶ conveyed to Bob
  tier, un- │ out1: 89,998,450 → A_change  "change child"       │──▶ stays with Alice
  broadcast)│ out2: 240        → P2A anchor (anyone-can-CPFP)   │
            └──────────────────────────────────────────────────┘
```

Be precise about what the budget buys. A budget bounds *future* co-signs; it cannot retract one
already issued, and the parent's trigger `T` was co-signed back at `claim()`. What defeats [B1] is
not the budget but the **shape**: `SP` spends `X_m.out[0]`, a descendant of `T`, so a retained
trigger can only start the clock on the current owner's own chain rather than race it. Alice's
superseded owner state is disclosed alongside, so Bob's census can count it.

**Step 4 — establish the children, convey the piece.** Each child gets its own two-tier headless
ladder co-signed under its aggregate (`establish_child`). The piece child's bundle is conveyed to
Bob's mailbox **together with the standard SE key-handover material** and the ancestor chain
`F → T → X_m → SP`. Bob's `claim()` runs `verify_child_bundle` — parent `F` on-chain, parent
terminal, and an exact-equality **census** (`num_sigs` == disclosed tiers, so no state was hidden)
— and then *completes the handover*: the SE rotates its share so the aggregate `A_child` is
invariant (which keeps the pre-signed exit ladder valid) and re-points `auth` to Bob. Alice is
permanently locked out. The change child is persisted locally as a first-class coin.

**What each party ends with:** Bob has a 10,000,000-sat **first-class** child — he can pay it
onward off-chain, whole (`child_retransfer`) or split again (`child_in_ladder_pay`, §5.1); Alice
has an 89,998,450-sat change child; the parent is dead forever; the chain saw nothing; the SE saw
a hash. Proven end-to-end by `sdk59` (Alice pays Bob, Bob claims and exits), `sdk58`
(`verify_child_bundle` accepts a real split child, parent terminal, child left non-terminal) and
`sdk60` (alice → bob → carol, funding outpoint unspent throughout).

> **The un-laddered lane.** A carrier, or a sub-coin whose funding is an un-broadcast split output
> [B0], has no ladder, and `split_coin` mints its pieces the signed-once way: one SE-co-signed,
> **locktime-0**, un-broadcast tx with `fee_reserve = clamp(parent/100, 300, 2000)` sats pre-committed
> out of the splitter's change (2,000 on a 1-BTC parent, 300 on a 10,000-sat carrier), both outputs
> ≥ 330 dust and ≥ their own backup fee. Both outputs become sub-coins with a *fresh* first backup
> tx (locktime `h_split + initlock`), one shared **exit branch** row (the signed split tx, appended
> to any branch the parent already had) and an ancestor list (`parents-<id>`); the piece is handed
> over like any whole coin, branch and ancestor ids riding along in the encrypted transfer message,
> and the receiver verifies the branch end-to-end plus `terminal: true` on every named ancestor
> (§3). This is the lane every colored split in §2b runs on, and `split_coin` hard-refuses a
> laddered parent rather than producing the [B1] shape.

### 2b. Sending 0.1 TKN from a 1.0 TKN allocation

Token amounts are **raw u64 units**; `precision` (u8, fixed contract metadata) says only where
the UI should put the decimal point. "1.0 TKN" at precision 2 is 100 raw units; "0.1 TKN" is
`10^(precision−1) = 10` units. The SDK never scales — every API takes and returns raw units.

Alice issued 1.0 TKN (`issue_token(ticker, name, 2, 100)`): the full 100-unit supply sits on a
statechain **carrier** coin of 10,000 sats. She calls `transfer_tokens(asset, bob, 10)`.

A carrier is **never laddered**, by design: the TES-R tiers are plain, RGB-unaware spends, so a
trigger over a carrier would destroy the allocation the first time it moved. `claim()` therefore
excludes carriers from auto-establish and fails *closed* when RGB state is momentarily unreadable
(terminal freeze, [PROTOCOL.md §5.10](../PROTOCOL.md); `sdk52`). Everything below is the un-laddered
lane of §2a, and it is the only lane tokens ever use.

**The colored split** (`create_colored_split_tx`): one SE-co-signed, un-broadcast tx, same shape
as 2a's un-laddered lane, but each output carries an `(address, sats, rgb_amount)` triple and rgb-lib inserts exactly
one OP_RETURN carrying the opret commitment to the RGB state transition (INV-11):

```
fee_reserve = clamp(10,000/100, 300, 2000) = 300 sats
piece  = (1,500 sats, 10 units)       ← TOKEN_PIECE_SATS is a constant: every piece carries
change = (8,200 sats, 90 units)          exactly 1,500 sats; the sats are packaging, the
                                         token is the payload
            colored split tx (locktime 0, un-broadcast, fee 300 sats)
            ┌──────────────────────────────────────────────────┐
 carrier ──▶│ in:  carrier outpoint (10,000 sats + 100 units)   │
            │ out: 1,500 sats  ⟨seal: 10 units⟩   "piece"       │──▶ Bob
            │ out: 8,200 sats  ⟨seal: 90 units⟩   "change"      │──▶ Alice (new carrier)
            │ out: OP_RETURN ⟨opret commitment⟩                 │
            └──────────────────────────────────────────────────┘
```

Conservation (`Σ recipient units + change units = carrier allocation`, INV-13) is enforced by
rgb-lib at coloring time; the vouts are recomputed from the colored tx (the OP_RETURN position is
not assumed). The **consignment** — the cryptographic history proving the 10-unit assignment —
travels *inside the owner-encrypted transfer message* as a `ConsignmentEnvelope{c, a, s}`:
consignment, an **advisory** amount hint, and the piece's sats.

**Receiver booking is consignment-governed, not envelope-governed.** Bob's SDK validates the
consignment off-chain against the branch txids, then books **the amount the consignment assigns
to his own witness outpoint** (REQ-21); if that disagrees with the envelope hint, the transfer is
rejected (ERR-8). The contract id is taken from the validated consignment, never from the sender
(REQ-22), and only fungible assignments count — an inflation right can never book as balance
(INV-26). A lying envelope can cause a rejection; it cannot inflate a balance.

**Result:** Bob holds 10 units on a 1,500-sat piece — a carrier he can hold or exit, but **not
re-send from**: 1,500 sats is below the 2,130-sat minimum carrier of §5.3, so a `transfer_tokens`
drawing on a received piece always fails with *"carrier coin too small (1500 sats) for a token
split"* — SDK token rails are one-hop today (§6, §7 FAQ,
[granularity-economics §3](../research/granularity-economics.md)). Alice holds 90 units on an
8,200-sat change carrier; one un-broadcast tx and zero on-chain footprint.

### 2c. Paying three people at once: width beats depth

`transfer_many([(a,x),(b,y),(c,z)])` and `batch_transfer_tokens` carve **all pieces in one
split**: one SE-co-signed tx with N piece outputs plus one change output (`change = parent −
Σ amounts − reserve`, every output ≥ 330). Each recipient gets their piece by an ordinary
handover; for tokens, one consignment is shared and each piece carries its own envelope with its
own amount hint.

On the **un-laddered** lane the measured constants are: a 3-recipient fan-out split is **241 vB**
total — about **60 vB of future exit weight per piece**, versus **155 vB per piece** if you paid
the three sequentially (each payment splitting the previous change, stacking depth). Width puts
every piece at depth `parent_depth + 1`; depth chains put the last change at depth
`parent_depth + 3`. When paying many parties from one coin, batch. *Evidence:* these vsizes are now
carried by the executable model — unit `granularity_model::width_vs_depth_weight_model` pins
112/155/241 and the width-beats-depth ordering through the real `ExitCostEstimate`, cross-pinned by
`invalidation_model::exit_cost_scaling_model`. The E2E that originally measured them (sdk26) is
retired and **no live E2E re-measures the vsizes** — the ordering is model-pinned, not
chain-measured.

(Caveat: hand-offs within a batch are independent — the batch is not atomic across recipients,
[SPEC.md §14](../SPEC.md#14-known-limitations-adversarial-review).)

**Known gap — `transfer_many` has no in-ladder route.** Unlike `transfer()`, its parent filter
excludes carriers but *not* laddered coins, so it will plain-split a laddered parent: exactly the
[B1] shape `split_coin` hard-refuses. `in_ladder_pay` carves one piece plus change, not N, so there
is nothing yet to route it to. Until `transfer_many` either refuses laddered parents or gains a
multi-child split state, prefer N sequential `transfer()` calls, which route per parent shape
(the width saving above is then forfeited).

## 3. What a partial send does to invalidation

This is the heart of the design. On the un-laddered lane it composes entirely out of the machinery
described in the [invalidation deep dive](invalidation-deep-dive.md) (especially §3b/§3c); on the
ladder, consensus-level relative timelocks and the disclosure census do the same job. What a split
adds, item by item — each stated for both shapes:

**The parent dies, permanently and publicly.** Setting the budget to "one more signature" and
spending it on the split makes the parent **terminal**: the SE will never co-sign it again — not
for a withdraw, not for a transfer, not for a fresh backup, not for the legitimate owner
(REQ-18; budgets are `min()`-monotonic, INV-24, so termination cannot be undone via the API).
Anyone can confirm this at `GET /statechain/spend_budget/<parent>`. Every partial send therefore
*consumes* a coin: granularity is bought with coin lifecycle, not with trust.

Stated honestly, the budget is a **co-sign** bound, not a **spend** bound: it cannot retract a
signature already issued, and a laddered coin's trigger `T` was co-signed back at `claim()`. On the
un-laddered lane that gap does not open, because every spend of the parent's funding needs a fresh
co-signature. On the laddered lane it is closed by geometry instead — `SP` descends from `T`, so a
retained trigger starts the owner's own clock rather than racing the split [B1]. `split_coin`
therefore refuses a laddered parent outright rather than relying on the budget alone.

**Both outputs are new coins at depth +1.** On the un-laddered lane, piece *and* change are
sub-coins with **fresh backup ladders** anchored at the split height (`h_split + initlock` — a full
new hop budget each; depth does not consume ladder), sharing one branch: the same signed split tx
is the last hop of both coins' exit branches. On the laddered lane, each child gets its **own
headless two-tier ladder** (extension + state) hung off its `SP` output by `establish_child` — the
child's clock is relative and only starts once `SP` confirms — and both children share the ancestor
chain `F → T → X_m → SP`.

**The root deadline does not move — and on the ladder there is none.** An un-laddered tree's
off-chain lifetime is bounded by the earliest maturity among *ancestor* stale backups — for an
unsplit-fresh root, exactly `H_deposit + initlock`; where ancestors were transferred before
splitting, up to `k·interval` earlier than the reported number (IVL-INV-10; the open half of audit
[17]). Splitting extends *leaf* ladders, never that root deadline. A **laddered** coin has no
absolute deadline at all: every tier is relative-CSV and un-broadcast, so an idle coin never ages
and pays no rent — splitting it in-ladder costs pre-signed tier weight, not clock. See the deep
dive §4 for the un-laddered treatment.

**What the receiver of a piece must verify.** Two check-sets, one per lane — both automatic in
`claim()`. The un-laddered branch checks (normative: IVL-REQ-10..12, SPEC REQ-16/17) are:

| Check | Defeats |
|---|---|
| Branch linkage root-first; root outpoint on-chain, unspent, confirmed | fabricated ancestry |
| Every branch tx locktime ≤ tip (locktime-0 in practice, INV-4) | branches that lose the exit race (audit [11]) |
| Value conservation Σout ≤ Σin at every hop (INV-25) | script-valid but un-broadcastable branches |
| Full script/signature verification | unsigned or altered branch txs |
| Backup-ladder decrement + `num_sigs` (REQ-16) | stale-state handoffs |
| ≥ 1 named ancestor per branch hop, each `terminal: true` at the SE (INV-20, ERR-7) | a sender keeping a spendable ancestor to double-spend the tree |
| Tokens: consignment validates against branch txids; booked = consignment-assigned amount (REQ-21/22, ERR-8) | amount and contract-id lies |

(Blind-SE caveat: ancestor *ids* are not cryptographically bound to branch outpoints — the count
check defeats omission, not substitution; the compensating control is that the receiver holds the
full signed branch and can exit immediately. [SPEC.md §14](../SPEC.md#14-known-limitations-adversarial-review).)

For an **in-ladder child**, `verify_child_bundle` replaces the branch walk with a census:

| Check | Defeats |
|---|---|
| The ancestor chain roots at an `F` that is on-chain, confirmed and unspent | fabricated ancestry |
| Every conveyed tier is fully signed, and the child's tiers pay the child's aggregate `A_child` | altered or unsigned tiers |
| Parent `terminal: true` at the SE | the sender re-spending the parent |
| **Exact-equality census**: the SE's public `num_sigs` for each node equals the tiers actually disclosed (`Σ` over the N-hop chain), and the current state sits at a strictly *lower* CSV than every disclosed superseded state | a hidden, un-disclosed rival state — including one signed before conveyance |
| The handover is then **completed** (`/transfer/receiver`): the SE rotates its share, `A_child` stays invariant, `auth` moves to the receiver | the sender staying a co-owner and signing a rival afterwards |

The census is what makes each hop auditable: a hop costs exactly one co-signature and discloses
exactly one superseded state, which the receiver counts and proves out-raced. A sender's co-sign is
additionally refused by the coordinator while a transfer of that coin is open (the pending-transfer
lock), closing the window between the receiver's census and its handover completion. Evidence:
`sdk58` (accepts a real split child), `sdk54`/`sdk55` (adversarial bundles: padding, inversion,
hidden low-CSV state), `sdk46`/`sdk47` (`R'` census), `sdk60` and `sdk17` (multi-hop).

**The benign co-descendant hazard** (un-laddered lane). After Alice pays Bob from a split, Alice
(change) and Bob (piece) hold the *same* branch txs. Either can broadcast them at any time — and that is
harmless: broadcasting the shared branch only materializes both funding outputs on-chain earlier;
it moves no ownership (each output is its owner's 2-of-2). Receivers must expect their sub-coin
to become a flat coin without their involvement; the SDK treats an identical-tx rebroadcast as
success, and only a *different* tx spending the branch root is a conflict (IVL-REQ-13,
`ExitBranchConflict`).

**Keep splitting the change and depth grows — linearly.** k successive partial sends from one coin
leave the last change coin at depth k. On the un-laddered lane each level adds exactly **155 vB**
to the future exit and one more 300–2,000-sat reserve burned at split time (model-pinned, §2c).
On the ladder each level adds the child's **two tiers — ≈ 248 vB** at 124 vB apiece — so a depth-*d*
child exits through `3 + 2d` tier txs ([PROTOCOL.md §6](../PROTOCOL.md)), paid out of committed tier
fees rather than a reserve. Nothing else compounds: leaf ladders stay fresh, the un-laddered root
deadline still bounds its whole tree, and a laddered coin has no deadline to consume. The cost of
granularity is *exit weight and fees*, not security.

## 4. What a partial send does to unilateral exit

**An un-laddered piece exits in two moves.** The branch (locktime-0) broadcasts *now* and always
beats any locktimed stale ancestor backup, provided it lands before the earliest ancestor maturity;
the piece's own backup then waits out its **fresh** ladder — up to ≈ `initlock` (≈ 6.9 days on the
deployed 1000/10 profile) if you exit right after receiving. Value is secured on-chain within a
block; it becomes spendable plain BTC after the leaf wait.

**An in-ladder child exits by walking tiers.** Nothing is locktimed and nothing races: broadcast the
ancestor chain `T → X_m → SP` and then the child's own `ext_child → state_child`, each tier waiting
out its relative CSV after its parent confirms. On the mainnet schedule (`E0 = 720`, `D0 = 1440`,
decrementing 36 blocks per hop) a flat coin's worst wait is `E0 + D0 = 2,160` blocks ≈ **15 days**,
shrinking with activity; a depth-*d* child is bounded by the sequential CSVs of the chain above it
([PROTOCOL.md §5.2/§6](../PROTOCOL.md)). Each tier carries its own committed fee, so the base case
relays standalone with no reserve arithmetic. `sdk50` walks a root coin's full ladder exit through
the public `unilateral_exit()`; `sdk59` exits a received **child**; `sdk17` exits a depth-2
grandchild.

**On the un-laddered lane, cost grows 155 vB per level**
([economics §3b](../research/invalidation-economics.md), depths 0–4):

| Depth | Txs | Total vB | Fee @1 sat/vB | @10 | @100 |
|---|---|---|---|---|---|
| 0 (flat) | 1 | 112 | 112 | 1,120 | 11,200 |
| 1 | 2 | 267 | 267 | 2,670 | 26,700 |
| 2 | 3 | 422 | 422 | 4,220 | 42,200 |
| 4 | 5 | 732 | 732 | 7,320 | 73,200 |

(These are *plain* splits, and the vsizes are model-pinned rather than E2E-measured — see §2c. A
colored token branch carries one OP_RETURN per hop: **198 vB/hop** instead of 155 (measured,
`SDK_E2E=29`: colored split = 198 vB) — depth-1 = 310 vB, depth-2 = 508 vB (measured, and exercised
end-to-end by `SDK_E2E=39`), depth-4 ≈ 904 vB;
[granularity-economics §6](../research/granularity-economics.md).)

**On the ladder, cost grows ~248 vB per level** — every tier is the same ~124 vB, so a depth-*d*
child needs `3 + 2d` of them ([PROTOCOL.md §6](../PROTOCOL.md)), plus 0–3 P2A fee children
(~152 vB each) only if a spike demands them:

| Depth | Tier txs | Total vB | Fee @2 sat/vB (committed) | Worst wait |
|---|---|---|---|---|
| 0 (flat laddered coin, `sdk50`) | 3 (`T→X→S`) | ~372 | ~744 | `E0 + D0` = 2,160 blk ≈ 15 d |
| 1 (in-ladder child, `sdk59`) | 5 (`T→X→SP` + `ext→state`) | ~620 | ~1,240 | + the child's own `E + D` |
| 2 (grandchild, `sdk17`) | 7 | ~868 | ~1,736 | + one more `E + D` |

Unlike the un-laddered reserve, these fees are *committed per tier* at signing (2 sat/vB) and the
tier relays standalone; the P2A anchor is the top-up path, not a shortfall.

Branch fees on the un-laddered lane are the pre-committed reserves (effective ~1.9–12.9 sat/vB); the backup is
CPFP-bumpable from its own output once matured, the branch is not bumpable by anyone honest
(every branch output is a 2-of-2) — the full fee-spike treatment is
[invalidation-deep-dive §5.4](invalidation-deep-dive.md#54-fee-spike-10-during-a-unilateral-exit).

**A token piece's exit is the same broadcast wearing two hats.** The colored split txs *are* the
RGB witness transactions: when the branch confirms, the OP_RETURN anchors confirm with it, and
the allocation **settles as an ordinary on-chain RGB holding at the exact partial amount**
(INV-16) — seated on the piece outpoint, provable from the consignment to any rgb-lib wallet, no
Mercury software needed to validate it. Two precision points, honestly:

- **The consignment must survive.** Exit material for a token piece = branch rows + backup +
  consignment (`BackupTx.rgb_consignment`). None of that is derivable from the seed — it lives in
  the wallet DB and the exported **recovery bundle** (see the deep dive's H3 caveat: mnemonic-only
  restore is total loss for *any* off-chain coin, and doubly so for tokens).
- **Settling and spending are different acts.** Settlement needs nobody's cooperation. *Moving*
  the settled allocation afterwards means spending the piece's 2-of-2 outpoint inside a new
  colored witness tx — a cooperative act while the SE lives. The pre-signed plain backup is the
  sats-only escape: it sweeps the 1,500 sats (minus the pre-signed 112-sat fee ≈ **1,388 sats**
  net at the default 1.0 sat/vB cap) but carries **no RGB commitment**, so broadcasting it
  abandons the allocation. A dedicated *colored* unilateral exit is not shipped (roadmap).

**The 1,500-sat economics at high feerates.** At 100 sat/vB, confirming the piece's 112-vB backup
costs ~11,200 sats — ~7.5× the 1,500 sats it carries, over 8× the ~1,388 it actually recovers —
and a CPFP from a 1,388-sat output cannot fund it (the child alone would need ~22,000 sats,
[economics §3b](../research/invalidation-economics.md)). The sats are economically dust in a
spike; the breakeven feerates are tabulated in
[granularity-economics §3](../research/granularity-economics.md). That is by design: a token exit
is about the **asset**, not the packaging — the branch (whose reserve was pre-committed) settles
the allocation whatever the fee market does to the 1,500 sats.

**Carriers are protected from accidental destruction.** Because a plain-BTC spend of a carrier
outpoint destroys its allocation, *every* plain sweep in the SDK refuses carriers: `withdraw` and
`unilateral_exit` exclude them from all-coins defaults and hard-error on explicitly named carrier
ids (audit [6][7]); `split_coin` refuses carrier parents; balance arithmetic fails closed when
RGB state is unavailable (audit [23]). The token allocation cannot be swept away by a fat-fingered
`withdraw()`.

## 5. Real-world situations

### 5.1 You receive 0.1 BTC, then pay 0.03 out of it

Bob's 10,000,000-sat piece from §2a is a received in-ladder **child**, and it is **first-class**:
Bob completed the SE key handover at claim, so he co-owns `A_child` and Alice is locked out
(CHILDREN.md). He can spend it two ways, both entirely off-chain:

- **Whole** → `child_retransfer`: co-sign one fresh state over `ext_child.out[0]` at a strictly
  *lower* CSV (so it out-races the state it replaces), pay the new recipient, and disclose the
  replaced state as superseded. Verified by `sdk60` (alice → bob → carol; the funding outpoint `F`
  is never spent — two payments, zero on-chain footprint).
- **Partial** (this case: 3,000,000 of 10,000,000) → `child_in_ladder_pay`, a **child-level**
  in-ladder split. The child's state is replaced by a split state paying two grandchildren; the
  child itself becomes an intermediate segment in each grandchild's `ancestors` chain. The same two
  floors apply as at root level (`max(min_split_output, min_child_value)` — 1,306 sats at 2 sat/vB),
  checked before the child is terminalized. Verified by `sdk17` (alice → bob → carol, second hop
  non-exact, Carol's exit walking a depth-2 chain).

Either way the arithmetic is the tier model, not a reserve: `piece + change = tier_out_total(
ext_child.out[0], 2, rate)`. Each hop costs exactly **one co-signature** and discloses exactly
**one superseded state**, which the receiver's N-hop census counts and proves out-raced — so
depth is auditable rather than merely deep. The grandchildren are depth 2: `3 + 2·2 = 7` tier txs
to exit, ~868 vB (§4). There is no root deadline to renew or exhaust — the ladder is relative-CSV
throughout.
**Outcome:** works exactly like any payment; the receiver-side check is the census over a 2-hop
ancestor chain, and the new owner inherits one more level of tier weight (~248 vB).

### 5.2 Wallet holds 60 + 50 TKN on two carriers, pays 100 → COMBINED

`transfer_tokens` first tries a **single carrier** holding ≥ 100; finding none, it **combines**
carriers automatically. It selects the fewest carriers of the asset whose allocations sum to ≥ 100
(here both), makes each terminal at the SE, and mints the payment in ONE SE-co-signed colored
combine tx — N inputs → the recipient's piece (exactly 100) + your change (10). bob gets a single
piece; you keep a 10-unit change carrier. Only if your *total* asset balance is below 100 does it
fail, with a typed insufficient error. Verified by `SDK_E2E=31` (60 + 50 pays 100).

**Security — the combine does not weaken invalidation.** A combined coin's exit branch is a
multi-input DAG. The receiver requires **one terminal ancestor per structural input** (`Σ inputs`,
not per hop), so a 2-input combine forces *both* carriers named and terminal — a sender cannot
combine a good carrier with a double-spendable one and hide it. `validate_branch` also rejects a
**non-tree** branch (two branch inputs spending the same outpoint — which could never confirm) and
requires every on-chain root **confirmed** (not a 0-conf mempool utxo). Residual (as for splits):
the blind SE binds no id to an outpoint, so an online receiver should exit the combined piece
promptly — the locktime-0 branch lets it win the race (SPEC §14 substitution caveat).

### 5.2b Receiving the same asset twice → works (balance sums)

A wallet that already holds asset X and receives a *second, separate* allocation of X books it and
its balance **sums**. The accept path imports X's genesis only on *first sight* (idempotent on an
already-known asset); the second receive brings in the new transitions and registers the new
allocation. This is the normal flow behind a merchant taking repeated payments in one token, or
the "pay 60 + 40" split above landing both pieces at one receiver. **Outcome:** both allocations
book; balances add up. *This was a bug* — the accept path used to re-import the genesis on every
receive, hit a `UNIQUE constraint`, and strand the second allocation while the retriable-booking
watcher spun on the permanent error — now fixed in the rgb-lib fork. Verified by `sdk29` (bob
receives PT2 three times: 10 → 11 → 9,996).

### 5.3 Tiny amounts: the floors, and why

Rows 1–3 are the **un-laddered** lane (`split_coin`, `transfer_many`, every colored split); rows 4–5
are the **laddered** one; the rest are token-specific and therefore un-laddered too.

| Floor | Value | Why |
|---|---|---|
| Split-output dust floor | **330 sats** | P2TR dust: a sub-330 split output makes the branch non-standard/unrelayable — the sub-coins would be stranded with no exit (audit [9]; enforced **sender-side** in the planner, the split guard and the PSBT builder; receiver-side branch validation checks linkage/locktime/conservation/scripts, *not* output standardness — a hostile non-SDK sender could still hand over a stranded branch, a known gap) |
| Smallest **mintable** un-laddered piece | **≈ 442 sats** (`330 + backup fee`, at 1 sat/vB) | 330 is only the split-output floor; the piece's own backup (112 vB) must sweep above dust after its fee, so a 330-sat piece can't back itself. The split guard enforces `min_split_output(fee_rate) = 330 + ceil(112·fee_rate)` on both outputs *before* the parent is made terminal (fixed), so a `[330, 442)` piece is refused cleanly with the coin untouched — never stranded. Pinned by unit `granularity_model::backup_fee_floor_is_the_true_mintable_minimum` and minted live by `sdk29`, which splits a freed sub-coin at exactly `330 + ceil(112·r)` (the E2E that first measured it, sdk28, is retired) |
| Smallest splittable un-laddered coin | **960 sats** | `330 (piece) + 300 (reserve floor) + 330 (change)`; at exactly 960 the only admissible piece is 330 (change lands exactly on the floor); at 959 every split is refused. The planner is 1 sat stricter: `transfer()` demands a 961-sat coin where a manual `split_coin` accepts 960 ([granularity-economics §2](../research/granularity-economics.md)) |
| Smallest **in-ladder child** | **1,306 sats** (at the deployed 2 sat/vB committed rate) | `min_child_value(rate, dust) = 2·(committed_fee + P2A) + dust = 2·(248 + 240) + 330`: a child funds its OWN extension and state tier before its final output can clear dust. **D1, fixed:** the admission guard used the un-laddered 442 floor, so a child in `[442, 1306)` was admitted, the parent was terminalized, and `establish_child` *then* failed with `FeeTooHigh` — stranding the parent to unilateral-exit-only. Both floors now apply, the larger binds, and both are checked before the parent's spend budget is consumed. Same floor at child level (§5.1). Pinned by `sdk30` (a sponsored rebate sized at `max(fee + dust, min_child_value)` states the 1,306 identity) and exercised by `sdk58`/`sdk59`/`sdk17` |
| Smallest **in-ladder splittable coin** | **~2,612 sats + one tier fee** | two children at 1,306 plus the split state's own `committed_fee_for_outputs(2) + P2A` (574 at 2 sat/vB) taken off `X_m.out[0]`. Below that, `in_ladder_pay` refuses up-front with the parent fully spendable |
| Smallest token send | **1 raw unit** | amounts are u64 units; rgb-lib conserves them exactly; sub-unit resolution does not exist (that is what `precision` display-scaling is for) |
| Smallest token-capable carrier | **~2,242 sats** (at 1 sat/vB) | `1,500 (piece) + 300 (reserve) + 442 (change, backup-fee floor)`. The fit guard ("carrier coin too small") fires at ≤ 1,800; the backup-fee floor then requires the change to clear ~442, refusing the whole [1,801, 2,242) band up-front (the dust-only derivation gives 2,130; the enforced floor is 2,242 and rises with feerate) |
| Smallest exit-viable coin | **≈ 442 sats** | the backup output must clear dust after its fee ([economics §5](../research/invalidation-economics.md)) |

Above its lane's mint floor, resolution is exactly **1 sat** — un-laddered, any piece in
`[330 + backup_fee, parent − reserve − 330]` (the 330 dust floor bounds the split output; the
backup-fee floor bounds what you can actually mint into a usable coin); in-ladder, any piece in
`[1306, tier_out_total(X_m.out[0], 2, rate) − 1306]`.
**Outcome:** on the `split_coin` / `in_ladder_pay` / `child_in_ladder_pay` / planner paths,
micro-amounts are refused loudly and early — before the parent is touched. One consequence worth
knowing: the *planner* carries only the 442-sat backup floor, so a laddered payment leaving a
change in `[442, 1306)` gets past planning and is refused by `in_ladder_pay`'s own guard — cleanly,
with the parent still fully spendable (this is exactly the D1 window, now closed).
Two paths still check late, honestly: a boundary-sized `transfer_many`
parent, or a token carrier of 1,801–2,129 sats (the row above), passes the SDK guard and is only
refused by the PSBT builder's dust floor *after* the terminal-guard has pinned the parent to a
single remaining co-signature — recoverable (that one co-sign funds a corrected retry) but
irreversible ([GRANULARITY-SPEC](../GRANULARITY-SPEC.md) GRN-REQ-8 note / GRN-INV-6 / §11.7;
[granularity-economics §2](../research/granularity-economics.md) "boundary hazard").

### 5.4 Sending your entire balance vs almost your entire balance

**Entire balance** (or any amount an exact subset covers): no split at all. `Plan::Exact` hands
over each coin whole — N coins, N transfer messages, the receiver ends up with **N coins**
(§6). No reserve is burned, no depth is added; this is the cheapest possible payment shape.
**Almost-entire** is the trap: coins `[500, 300]`, pay 600 → no exact subset; the greedy pass
takes 500, the 100-sat remainder is below the planner's floor (`min_split_output`, 442 sats at
1 sat/vB — the bare 330 dust value is its special case) and cannot be minted as a piece —
`plan_with_floor()` refuses with `InsufficientBalance{requested: 600, available: 800}` (audit [29];
unit tests `sub_dust_remainder_is_refused` and `granularity_model::plan_paths_matrix` — the E2E that
asserted the "no candidate large enough" clause, sdk28, is retired and **nothing replaces it at E2E
level**). **Outcome:** the user sees "insufficient balance" *despite*
having more than the amount — which *usually* means no composition of coins can pay this exactly,
and occasionally is planner conservatism: the greedy pass explores one largest-first ordering and
is 1 sat stricter than `split_coin` at every boundary, so some fundable targets are refused
(worked examples in [granularity-economics §2](../research/granularity-economics.md)). Pay a
slightly different amount, use the whole coins, or compose manually — with `in_ladder_pay` on a
laddered coin, `split_coin` + `transfer` on an un-laddered one (`split_coin` refuses a laddered
parent, so it is not the general escape hatch it once was).

### 5.5 A merchant receives hundreds of small pieces

Each piece is a full coin: its own ladder, its own future exit. With no shipped plain-BTC combine,
offboarding N coins costs **N withdraw txs** (~111–140 vB each) cooperatively, or N tier chains
(un-laddered: N branch-plus-backup chains) unilaterally — fragmentation is a real, linear cost
([economics §3a](../research/invalidation-economics.md); the
daily-sweep bill is worked in [granularity-economics §4](../research/granularity-economics.md)).
What to do today: *spend pieces onward* (exact-subset selection actively consumes odd coins at
zero cost, and received children are first-class so they can be paid on whole or split again, §5.1),
consolidate periodically via withdraw-to-one-address + redeposit (**N withdraw txs
plus 1 redeposit** — there is no batched withdraw until combine ships). Deadline bookkeeping is
lane-dependent: laddered pieces have **none** (relative CSV, zero idle rent), while *un-laddered*
pieces — token carriers above all — each keep their sender's absolute root deadline, so a wallet
holding those should run `auto_exit_due` (§5.6).
**Outcome:** nothing breaks, but N coins = N exits/withdraws until combine ships.

### 5.6 Exiting a token piece during an SE outage, step by step

You hold 10 units on a 1,500-sat piece (depth d); the SE stops answering.

1. **Nothing is lost by waiting a moment.** Your exit material is already local: `branch-<id>`
   rows (the fully-signed colored splits), the consignment, your key share, the plain backup.
2. **Materialize the branch.** The branch txs are ordinary consensus-final transactions —
   broadcast them root-first with any tool (`sendrawtransaction`; they are in the recovery
   bundle). Honest caveat: `unilateral_exit(piece_id)` will *refuse* — the piece is a carrier
   and that op is a plain sweep (§4). But you rarely need to broadcast by hand: since REQ-33 the
   `auto_exit_due` watchtower **materializes** a received carrier near its deadline (branch-only,
   never a plain sweep — `wallet.rs`, `SDK_E2E=34`), and it runs from the background watcher by
   default (`SdkConfig::auto_exit`) — so the manual broadcast is the fallback for a wallet that
   isn't running a watcher (or a delegated keyless tower). A co-descendant exiting first also
   materializes the branch for free; in this scenario the sender's change coin holds the residual
   90 units and is itself a carrier, so it takes the same (now automatic) route; only a *plain*
   co-descendant (e.g. the uncolored change of a later full-allocation spend) materializes the
   shared branch for free via the normal SDK exit paths. Do it before the tree's root
   deadline, like any sub-coin exit.
3. **Anchors confirm ⇒ allocation settles.** Each colored split's OP_RETURN confirms with it;
   after `rgb-lib` refresh the 10 units are a settled on-chain RGB holding at your piece outpoint
   (INV-16), provable to anyone from the consignment.
4. **The sats and the future.** The 1,500 sats stay in the 2-of-2. If the SE returns, colored
   cooperative spends resume. If it never does, the plain backup can reclaim ≈ 1,388 sats after
   the leaf-ladder wait — at the price of abandoning the allocation (no colored unilateral exit
   is shipped; §4). **Outcome:** the token state is secured unilaterally and permanently; only
   *onward movement* of the settled allocation still wants a live SE today. (A depth-2 colored exit
   is verified end-to-end by `SDK_E2E=39`: two successive token transfers build a piece whose exit
   branch is `[split1, split2]`, and broadcasting both root-first materializes the whole chain
   on-chain with the allocation preserved.)

### 5.7 The change coin after the token is fully spent

Send the *entire* allocation (`token_amount = carrier_amount`): the change output's
`rgb_amount = 0`, so it is left **uncolored** — a plain BTC sub-coin. The RGB engine marks the
old carrier outpoint spent; the change coin (carrier sats − 1,500 − reserve) is ordinary sats:
splittable, transferable, withdrawable, with no carrier guard applying. It stays **un-laddered**
(its funding is the un-broadcast colored split output, [B0]), so it is the plain `split_coin` lane's
own coin. **Outcome:** carriers are not carriers forever; sats exit token duty the moment the
allocation moves on whole. `sdk29` asserts the freed change surfaces in `available_sats` and that a
plain `split_coin` on it succeeds where the same call was refused while it carried; the honesty
residual is narrower than before — `transfer`/`withdraw`/`unilateral_exit` admit it through the
same `is_token_carrier` predicate the split guard reads, but no E2E drives *those* three on the
freed coin.

### 5.8 A high-feerate day

**Un-laddered lane.** The split's fee reserve was fixed at split time — `clamp(parent/100, 300, 2000)`
sats, paid by the **splitter** out of change, giving the branch an effective ~1.9–12.9 sat/vB forever
(no RBF; every branch output is a 2-of-2, so no honest party can CPFP it before their own backup
matures). What CPFP *can* do: once your leaf backup is in the mempool, a child from its (solely-owned)
output can lift the whole package — at spike prices, and only if the coin is big enough to fund
it. What it cannot do: accelerate the branch alone, or rescue a sub-1,500-sat output's economics.

**Laddered lane — the spike answer is built in.** Every tier tx (trigger, extension, state, split
state, every child tier) carries a **P2A anchor** worth 240 sats: an anyone-can-spend output that
lets the owner, a keyless watchtower or the operator attach a live-rate fee child (~152 vB) to a
*pre-signed* transaction. The committed 2 sat/vB is a floor that makes the base case relay
standalone, not the whole fee — so the stranded-reserve pathology above does not return in force,
and a party other than the owner can pay to rescue an exit. `sdk45` proves a keyless tower can drive
an offline owner's whole exit from a bundle holding no key material, and `sdk40` proves the SE
blind-signs the v3 + relative-timelock + P2A shape; **attaching an actual P2A fee child in a spike
is not E2E-covered** — the anchor is present and standard-relayed, the rescue path is not exercised.

Full arithmetic and the deadline interaction:
[invalidation-deep-dive §5.4](invalidation-deep-dive.md#54-fee-spike-10-during-a-unilateral-exit)
and [economics §3b](../research/invalidation-economics.md); how a sustained 10× feerate world
moves the granularity floors is [granularity-economics §7](../research/granularity-economics.md).
**Outcome:** un-laddered exits confirm slowly but
safely if the branch lands before the earliest hostile maturity — broadcast early, not at the
deadline. Laddered exits have no deadline to beat: they confirm slowly, wait out their CSVs, and
can be topped up through the anchor.

### 5.9 Invoice flows: a `utexoinv` for 0.5 TKN

`create_tokens_invoice(asset, 50, …)` at precision 2 encodes **amount = 50** — raw units on the
wire, always (`UtexoInvoice.amount` is u64: sats when `asset_id` is absent, raw units when
present; REQ-28). The payer's `fulfill_utexo_invoice` checks expiry (ERR-11: "invoice expired"),
then routes to `transfer_tokens(asset, address, 50)` — landing in §2b's colored split. Precision
appears exactly once in the pipeline: at display time, when a UI renders 50 units of a
precision-2 asset as "0.5". A wallet that scaled amounts anywhere else would double-convert;
none of the SDK does. **Outcome:** what you see is scaled, what moves is integral.

### 5.10 Privacy: who learns what from a partial payment

| Party | Learns | Does NOT learn |
|---|---|---|
| **SE** | that id `P` was made terminal and co-signed once more (in-ladder: that `SP` and two children's tiers were co-signed, and the public `num_sigs` counters that the census reads); that ids `A`, `B` were initialised; message-relay timing | **any amount** (blind-MuSig2: it signs a blinded challenge, never sees a transaction, output, or value — from the co-sign request *alone* it cannot distinguish a split from a state re-sign or a backup; the budget call plus two fresh child slots do reveal that *some* structural operation happened, but amounts stay invisible either way) |
| **Receiver** | un-laddered: the full branch — including *your change outputs' sats values* along their path — plus, for tokens, the consignment's transition history subset. In-ladder: the ancestor chain `F → T → X_m → SP` and every superseded state the census requires, which likewise exposes *your change child's value* | your other coins; anything beyond their chain's cone |
| **Chain observer** | nothing, until someone exits; then the split txs (amounts, the P2A anchors, and the OP_RETURN marking RGB use) become visible | who owns which output (fresh 2-of-2 keys), token amounts (RGB state is off-chain; the anchor is a commitment) |

**Outcome:** granularity is invisible to the operator by construction; the necessary trade is
that a payee sees the chain they must be able to verify — including the change values on it.
Full disclosure is not an accident of the design, it *is* the census: a hop that hid a state would
fail the receiver's exact-equality count.

## 6. The UX perspective

**What the user types vs what happens.** `transfer(addr, 10_000_000)` — one call — hides all of
the above: coin refresh, carrier filtering (token carriers never fund plain sends), exact-subset
search, split planning, **lane dispatch** (laddered root → in-ladder split, received child →
child-level split, un-laddered coin → plain split), terminal-guarding, blind co-signing, child
establishment or sub-coin registration, and conveyance or per-coin handover.
`transfer_tokens(asset, addr, 10)` likewise. There is no "split" button and no protocol switch; the
split is an implementation detail of exact amounts. (One visible side effect: each split output
consumes a deposit-token slot from the SE's anti-spam token server, auto-requested — or
`SdkError::TokenPaymentRequired` if the SE charges. In-ladder children draw *derived* slots against
the parent, which are free.)

**What the receiver sees.** Possibly **several coins for one payment**: an exact-subset payment
of 100k covered by 60k+25k+15k arrives as three coins (three claims, one logical payment). A
split-based payment arrives as exactly one piece. `TransferResult{coins, total_sats, used_split}`
tells the sender which happened; receivers should sum, not count. A received in-ladder piece is a
**first-class coin**, not a claim ticket: the receiver's `claim()` completes the SE key handover, so
it can be paid onward off-chain whole or split again (§5.1).

**Events.** `TransferClaimed{statechain_ids}` (note the plural), `TokenTransferClaimed{asset_id,
amount, statechain_id}` (raw units), `BalanceUpdate`, plus the exit-side events
(`ExitDeadlineApproaching`, `ExitBranchConflict`) described in the
[invalidation deep dive §6](invalidation-deep-dive.md#6-the-ux-perspective).

**Balances.** `TokenBalance{asset_id, ticker, name, precision, balance, total}` — `balance`
(settled) and `total` are **raw u64 units**; rendering `balance / 10^precision` is the UI's job.
Plain-BTC balance excludes carrier sats entirely (they are packaging, not spendable BTC — and
the arithmetic fails closed if RGB state is unreadable, audit [23]).

**The sharp edges, honestly:** a receiver cannot aggregate the 1,500-sat **token** pieces they are
handed — below the carrier floor (§5.2; sender-side combine across carriers now ships, so the
*sending* limit is gone); **received token pieces are terminal at the SDK layer** — 1,500 sats of
packaging is below the token-carrier floor, so tokens
you receive can be held or exited but not re-sent off-chain until combine/top-up ships (§2b;
quantified in [granularity-economics §3/§8](../research/granularity-economics.md)) — note this is a
*token-packaging* limit, not a protocol one: received **sats** pieces are first-class and re-spendable
(§5.1); fragmentation with no plain-BTC combine (§5.5); `transfer_many` has no in-ladder route and
will plain-split a laddered parent, so prefer sequential `transfer()` calls (§2c); the split-output
floors — 330-sat dust, ~442-sat *mintable* un-laddered piece (backup-fee floor, enforced up-front so
it refuses rather than strands), 1,306-sat in-ladder child (`min_child_value`, likewise enforced
up-front since D1), ~2,242-sat token-viable carrier (§5.3); every token piece carries exactly 1,500
sats of packaging whose exit economics are poor in a spike (§4);
seal blinding is a **fixed constant** in SDK token flows (a design simplification — acceptable
because the consignment travels owner-encrypted, flagged for randomization once bindings allow);
the consignment and branch rows are recovery-bundle material, not seed-derivable; and the
"insufficient balance" message sometimes means "no exact composition exists" — or, rarely,
planner conservatism — not "too poor" (§5.4).

## 7. FAQ

**Can I send 1 sat?** Not as a minted piece. On the un-laddered lane, split outputs must clear the
330-sat dust floor and the piece's own backup must sweep above dust after its fee, so the smallest
piece you can actually mint is `330 + backup_fee` ≈ **442 sats at 1 sat/vB** (pinned by unit
`granularity_model::backup_fee_floor_is_the_true_mintable_minimum`, minted live by `sdk29`). On the
laddered lane the child must additionally fund its own two tiers: **1,306 sats at 2 sat/vB**
(`min_child_value`). Both floors are enforced up-front, so an under-floor piece is refused cleanly
(coin untouched), never stranded. A pre-existing tiny coin can still move whole. Above the floor,
resolution is 1 sat. See [economics §5](../research/invalidation-economics.md).

**Why did my receiver get 3 coins for one payment?** An exact subset of the sender's coins summed
to your amount, so each was handed over whole (§5.4) — cheaper for everyone than splitting. Sum
them; they are one payment.

**Can the SE censor payments by amount?** It cannot *see* amounts — co-signing is blind
(§5.10) — so no policy keyed on value is possible. It can refuse service per statechain id or
per authenticated owner, which degrades those coins to their (SE-free) exit paths.

**Why is there 1,500 sats on my token piece?** Packaging: comfortably above the 330 dust floor
so the branch relays, and enough that the piece's pre-signed 112-sat-fee backup is valid. The
constant (`TOKEN_PIECE_SATS`) keeps token pieces uniform; the token amount is the payload.

**Can I re-send tokens I just received?** Not off-chain, today — and note this is a *token* limit
only; a received **sats** piece is first-class and re-spendable (§5.1). Your token piece carries
exactly 1,500 sats — below the 2,130-sat minimum carrier (§5.3) — so a `transfer_tokens` drawing on
it always fails with *"carrier coin too small"*. SDK token rails are structurally **one-hop**: an
issuer or holder with a fat carrier fans out; receivers hold or exit (settlement and the asset
itself are unaffected, §5.6). No value of the packaging constant fixes this; the fix is combining
several received pieces (sender-side combine has **shipped**, §5.2 — though it cannot rescue a lone
1,500-sat piece) or variable/top-up packaging (still open) — the arithmetic is worked in
[granularity-economics §3/§7](../research/granularity-economics.md).

**What if the envelope lies about the token amount?** You book what the *consignment* assigns to
your outpoint; an envelope that disagrees gets the whole transfer rejected (ERR-8). Lying buys
the sender a failed payment, never an inflated balance (§2b).

**Can I pay across several carriers at once?** Yes — `transfer_tokens` combines them automatically
when no single carrier covers the amount (§5.2). It's a sender-side operation: it merges *your*
carriers into one payment. (A receiver still can't aggregate the incoming 1,500-sat pieces they're
handed — those are below the carrier floor — so receiver-side fragmentation is a separate item.)

**Does splitting reset my 7-day clock?** On a **laddered** coin there is no such clock: every tier
is relative-CSV and un-broadcast, so an idle coin never ages, pays no rent, and has no deadline to
reset. An in-ladder split just gives each child its own two-tier ladder. On the **un-laddered**
lane (carriers, freed sub-coins) the old answer still holds: the *leaf's* clock resets — each
sub-coin gets a fresh ladder at split height — while the *tree's* does not, since the root deadline
`H_deposit + initlock` never moves (§3;
[deep dive §4](invalidation-deep-dive.md#4-over-time-the-ladder-as-a-consumable-budget)).

**Why can't I split my carrier's 8,200 sats as plain BTC?** A plain split carries no RGB
commitment — spending the carrier outpoint through it would destroy the allocation. `split_coin`
refuses carriers (as do withdraw and exit, audit [6][7]); sats leave a carrier only inside a
colored split (§5.7).

**What is precision, and can it change?** A u8 in the contract metadata, fixed at issuance,
consulted only by UIs. It cannot change, and no SDK code path scales by it (§5.9).

**Is 0.1 + 0.9 == 1.0 exact?** Always. There are no floats anywhere: 10 + 90 = 100 raw u64
units, and rgb-lib enforces exact conservation per transition (INV-13). Rounding error is
structurally impossible.

**Who pays the split fee reserve, and where does it go?** On the un-laddered lane: the splitter,
deducted from their change at split time (`clamp(parent/100, 300, 2000)` sats). It becomes the split
tx's miner fee — *burned on every exit path*, cooperative or unilateral, since both materialize the
branch. It is a spent cost, not a refundable escrow. On the ladder there is no reserve at all: every
tier carries its own **committed fee** (2 sat/vB) plus a 240-sat P2A anchor, taken off the tier's
value as it is built — so the split state costs `committed_fee_for_outputs(2) + P2A` = 574 sats at
2 sat/vB, and each child's own two tiers cost `2·(committed_fee + P2A)` = 976. Same economics
(spent, not escrowed), charged per pre-signed tx rather than per split.

**What happens to dust-level change?** It never exists on any path: the split guards error and
the planner won't select a coin that would produce one (audit [29]), both before the parent is
touched — `split_amounts_floored` un-laddered, `max(min_split_output, min_child_value)` in-ladder
(§5.3, D1). On the late-checked paths (`transfer_many`, token splits at the carrier boundary) the
PSBT builder's dust floor refuses instead — no dust output is ever co-signed there either, but
the refusal lands *after* the parent was pinned to one remaining co-signature (§5.3).

**Can one carrier hold two different tokens?** Not in SDK flows: carrier selection, coloring and
booking all operate on one contract per coin, and issuance/receipt each bind one allocation to a
fresh outpoint. Treat "one carrier, one asset" as the operative rule.

**Does a split cost me a transfer hop?** No. On the un-laddered lane hops burn `interval` off a
ladder, and a split mints *fresh* ladders for both children; on the ladder, hops decrement the state
CSV `δ` (36 blocks) and a split gives each child a *fresh* schedule of its own. What it costs
instead: the parent coin (terminal), the fee (reserve un-laddered, committed tier fees in-ladder),
+155 vB (un-laddered) or ~248 vB (in-ladder) of descendant exit weight, and two coin slots —
*derived*, hence free, on the in-ladder path.

## 8. Comparison recap: granularity

| | **Ours (native off-chain split)** | **Spark (denominations + SSP)** | **Ark / Second (per-round re-mint)** | **Lightning** |
|---|---|---|---|---|
| Resolution | 1 sat above the lane's floor (1,306 in-ladder, 442 un-laddered); tokens: 1 raw unit | fixed leaf denominations; odd amounts via SSP swap | arbitrary, but only at round re-mint | 1 msat nominal |
| Amount ceiling per payment | your largest coin (split) or any exact subset | leaf set + SSP pool depth | round capacity | channel capacity / inbound liquidity |
| Third party in a partial payment | **none** (SE co-signs blind; sees no amounts) | SSP pool required for odd amounts | ASP required, every round | route liquidity required |
| Cost per partial payment | in-ladder: ~574 sats of committed tier fee + ~248 vB future exit weight (+1 depth). Un-laddered: 300–2,000 sats reserve (splitter) + 155 vB (+1 depth); batch: ~60 vB/piece | swap fee/spread to SSP | share of round tx, every round | routing fees; liquidity opportunity cost |
| Is the received partial amount re-spendable off-chain? | **yes, first-class** — a received child completes the SE key handover at claim and can be paid on whole (`child_retransfer`) or split again (`child_in_ladder_pay`); `sdk60`, `sdk17`. (Received *token* pieces are the exception, §7) | yes (leaf transfer) | yes (out-of-round transfer) | yes (it is just balance) |
| Exit implication of receiving a partial amount | in-ladder child: `3 + 2d` tier txs (~620 vB at depth 1), sequential CSV, no deadline. Un-laddered sub-coin: branch (instant) + fresh-ladder backup wait ≈ initlock; cost 112+155·depth vB plain (~198 vB per colored hop, [granularity-economics §6](../research/granularity-economics.md)) | leaf chain broadcast (depth of Spark tree) | your branch of the round tree; **miss the round ⇒ swept** | force-close per channel; amounts not per-payment exitable |
| Idle rent while holding a partial amount | **0 vB** laddered (no deadline); un-laddered coins keep their sender's absolute root deadline | 0 | re-mint each round or lose funds | 0 |
| Operator sees amounts? | **no** (blind) | yes (SSP swaps by denomination) | yes (constructs the round) | no (onion), but channel peers see HTLCs |

Further reading: normative granularity requirements in
[GRANULARITY-SPEC.md](../GRANULARITY-SPEC.md); transfer and token primers in
[transfers.md](transfers.md) / [tokens.md](tokens.md); the invalidation machinery in
[invalidation-deep-dive.md](invalidation-deep-dive.md) and
[INVALIDATION-SPEC.md](../INVALIDATION-SPEC.md); prices in
[invalidation-economics.md](../research/invalidation-economics.md) and — granularity-specific
(unpayable-amounts map, carrier depletion, token breakevens, colored exit at depth) —
[granularity-economics.md](../research/granularity-economics.md); exit flows in
[exits.md](exits.md); the in-ladder split and the tier fee model in
[PROTOCOL.md](../PROTOCOL.md) §5.2/§5.4/§5.10 and
[CHILDREN.md](../CHILDREN.md); system spec [SPEC.md](../SPEC.md)
(REQ-15/17/18/21/22/27/28, INV-9/10/11/13/16/22/26, ERR-8/9/11).

Test evidence: `SDK_E2E=1/2/9/11` (payment flows), `29`/`31`/`34`/`39` (tokens: granularity,
combine, watchtower materialization, depth-2 colored exit), `58`/`59` (in-ladder split: verifier and
payment), `60`/`17` (first-class children, whole and partial re-transfer), `50` (unilateral ladder
exit), `45` (keyless tower), `54`/`55` (adversarial bundles), `30` (the 1,306 in-ladder floor);
`RGB_E2E=1/3/6/13/14`; unit `select`/`split_math`/`envelope`/`granularity_model`. Two things this
document is careful not to over-claim: the plain-split **vsizes** (112/155/241 vB) are model-pinned,
not E2E-measured, since sdk26 was retired; and the planner's "no candidate large enough" refusal has
only unit-level evidence, since sdk28 was retired with nothing replacing it at E2E level.
