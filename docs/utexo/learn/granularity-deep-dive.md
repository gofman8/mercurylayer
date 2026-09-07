# Sending partial amounts — the granularity deep dive

How a system whose on-chain unit is an indivisible UTXO lets you send 0.1 BTC out of a 1-BTC coin,
or 0.1 of a token out of a 1.0 allocation — without touching the chain, without a third party, and
without the operator ever learning an amount.

This is the long-form explainer. The normative statements live in
[../spec/SPEC.md](../spec/SPEC.md) (§5.1 coin selection, §6 split & combine, §7 tokens), the tier
machine in [../spec/PROTOCOL.md](../spec/PROTOCOL.md) §5.4/§5.9/§5.10, the child lifecycle in
[../spec/CHILDREN.md](../spec/CHILDREN.md), and the money in
[../spec/PARTIAL-PAYMENT-ECONOMICS.md](../spec/PARTIAL-PAYMENT-ECONOMICS.md). Transfer and token
basics are in [transfers.md](transfers.md) and [tokens.md](tokens.md); the invalidation machinery a
split rides on is in [invalidation-deep-dive.md](invalidation-deep-dive.md).

Audience: technical readers who have not read the code. Every constant below is named where it is
defined, and every number is re-derived from those constants rather than transcribed.

Contents: [the problem](#1-the-problem-utxos-dont-come-in-the-amount-you-owe)
· [the shapes a parent can have](#2-the-three-shapes-a-parent-can-have)
· [mechanics with real numbers](#3-mechanics-with-real-numbers)
· [the floors](#4-the-floors-and-where-each-one-comes-from)
· [effect on invalidation](#5-what-a-partial-send-does-to-invalidation)
· [effect on unilateral exit](#6-what-a-partial-send-does-to-unilateral-exit)
· [admission caps](#7-admission-depth-exit-length-and-headroom)
· [real-world situations](#8-real-world-situations)
· [privacy](#9-privacy-who-learns-what-from-a-partial-payment)
· [UX](#10-the-ux-perspective) · [FAQ](#11-faq) · [recap](#12-comparison-recap-granularity)
· [what this does not claim](#13-what-this-document-does-not-claim).

---

## 1. The problem: UTXOs don't come in the amount you owe

Bitcoin's unit of ownership is the UTXO, and a UTXO is indivisible: you spend all of it or none of
it. On-chain, "send 0.1 BTC out of a 1-BTC output" means broadcasting a transaction with a payment
output and a change output — divisibility is bought with a chain transaction every time. Every L2
that moves whole UTXOs off-chain (statechains, Spark leaves, Ark vouts) inherits the indivisibility
and must answer the same question: how does a user pay an *arbitrary* amount when the things being
transferred are fixed-size?

The deployed answers form a small design space. **Spark** keeps fixed leaf denominations and makes
arbitrary amounts a service: the SSP swaps your leaves for a set that sums right — server-side
pools, a third party in every odd-amount payment. **Ark** re-mints amounts every round: the ASP
includes your desired denominations as fresh vouts in the next round transaction — arbitrary
amounts, but only at round cadence and only through the ASP. **Lightning** has the finest nominal
resolution (millisats) but bounds every payment by channel capacity and inbound liquidity — the
amount you can *receive* is a provisioned resource, not a right.

**This system makes the partial amount the ordinary case.** One SE-co-signed, *un-broadcast*
transaction carves a coin into an exact piece plus change, and both are immediately first-class
off-chain coins (SPEC REQ-15, INV-22). No third party, no round, no liquidity table — and because
the SE co-signs blind, granularity costs nothing in the trust model: the SE never sees an amount.

That framing runs the other way too, and it is the sentence to keep hold of: **an arbitrary amount
equals a coin the sender already holds only by coincidence, so a partial payment is not an edge
case — it is what a payment IS.** Whole-coin handover exists (`child_retransfer`,
`clients/libs/rust/src/tesr.rs`), is free, and is almost never reachable, because the finest piece
the protocol will mint is 1,560 sat and no coin set is fine enough to land a subset sum on an
arbitrary amount's residue. Everything downstream of the first payment is therefore a **leaf**, and
leaf economics — not root economics — are the ones that describe a user. See
[../spec/PARTIAL-PAYMENT-ECONOMICS.md](../spec/PARTIAL-PAYMENT-ECONOMICS.md) §1.

---

## 2. The three shapes a parent can have

There is one protocol. `claim()` (`clients/libs/rust-sdk/src/wallet.rs`) establishes a TES-R exit
ladder — trigger `T` → extension `X_m` → state `S_0`, relative CSV, all un-broadcast — for every
un-laddered **root** coin whose funding `F` is on chain, at **first sight**: the establish pass
admits `IN_MEMPOOL`, `UNCONFIRMED` and `CONFIRMED` alike, because the trigger needs only the funding
outpoint, its value and the aggregate key. It is not unconditional, and the one condition matters
more than it used to: the pass calls `get_statechain_info`, which fails closed where no enclave
attestation identity is pinned or configured, and then ladders **nothing** — see §2's last paragraph.
A partial payment
therefore runs over whatever shape the selected parent has, and there are exactly three. The type is
`ParentShape` (`clients/libs/rust-sdk/src/transfer.rs`), resolved once per candidate by
`UtexoWallet::parent_shape`, and it selects the executor *and* the floor from the same read.

| shape | what it is | route | change leg's shape |
|---|---|---|---|
| **`Root`** | a claimed deposit, still whole, `tesr-` row | `in_ladder_pay` | one-rung **spine tip** |
| **`SpineTip`** | the sender's own change from an earlier in-ladder payment, `spinetip-` row | `spine_batch_pay` | one-rung **spine tip** |
| **`Child`** | a received piece — somebody's payment to you, `ctesr-` row | `child_in_ladder_pay` | two-tier **piece** |

There is no fourth arm, and the absence is deliberate rather than incidental. With every claimed
coin laddered, "no ladder of any kind" stopped being a lane to price and became a fault to report:
`parent_shape` **refuses** such a coin — run `claim()` to ladder it, and read `ladder_skip_reason` if
that declines — rather than routing it. `parent_shape_opt` is the probe form, kept for the one caller
where absence is data and not a fault: `has_exit_material`, where a coin with no ladder must read as
having no exit material rather than erroring the balance. That function is now **ladder-only** and
looks at nothing else — not even a legacy flat-backup row from before 2026-09-06. Counting one would
put a coin into the quote's `fundable` set that the executor then refuses, which is exactly the
quote-disagrees-with-executor bug the pair exists to make inexpressible. Read
*failures* propagate in both, because a swallowed DB error that read as "no ladder" is the
silent-degradation shape this pair exists to prevent.

Three things follow, and they are the shape of the rest of this document.

**`parent_shape` probes the spine tip first, and that ordering is the common case.** `in_ladder_pay`
declares `ChangeLeg::LastIsTip` (`clients/libs/rust/src/tesr.rs`), so after the *first* partial
payment the sender's change **is** a spine tip — and the second and every later payment out of it
take the `SpineTip` arm. A wallet that pays twice has spent most of its life on the spine-batch
lane, not the root lane.

**Nothing is split as plain BTC any more, and that is [B1] closed by construction.** The hazard was
structural: the trigger `T` carries `TRIGGER_SEQUENCE = 0xFFFF_FFFD` (`lib/src/tesr.rs`) — relative
lock disabled — and spends `F` directly, and every prior owner of a conveyed coin retains a signed
copy. A plain off-chain split spends that same `F`, so it is a *rival* for the funding outpoint
against a transaction with no timelock at all; no timelock schedule out-races that, and the payee
cannot detect the exposure. The old answer was a refusal — `split_coin` declined a parent carrying a
`tesr-` bundle by name — which held only for as long as every route into it kept asking. The answer
now is that the transaction cannot be built: `ParentShape::Unladdered`, `split_coin`, the plain
off-chain split, `ManyRoute::PlainSplit` and `ensure_exact_coin`'s minting fallback are deleted, so
the rival-spend shape is unconstructible rather than refused. The in-ladder split is what remains,
and it was always the real answer: the split state **descends from** `T` instead of racing it.

**A carrier is laddered like any other coin — wherever an enclave exists to attest.**
`SdkConfig::colored_ladder` (`clients/libs/rust-sdk/src/config.rs`) no longer states a bool: both
constructors READ the compiled-in attestation pin, `TesrParams::attestation_identity_const`
(`lib/src/tesr.rs`). Regtest pins the repo's own dev enclave, so the flag is **true** there and
`claim()`'s decision site — `match (config.colored_ladder, allocation)` — gives a carrier a
**coloured** ladder: every tier carries a real RGB state transition, so laddering *moves* the
allocation instead of destroying it. Mainnet's const returns `None`, because no mainnet enclave is
provisioned (and so does every public testnet's); the flag is therefore **false** there, and that is
not a verdict on the lane but a statement that there is nothing to verify an attestation against —
without a pin, terminality cannot be established at all (**V-6**, and TRUST-MODEL B11 for why
accepting the coordinator's own answer is not a substitute). Pin a mainnet identity and the flag
flips with no other change, which is the whole reason it reads the pin rather than restating it;
`colored_ladder_is_never_on_without_a_pinned_attestation_identity` is the unit test that stops the
two from drifting apart.

**What an unpinned network gets is NOT a second lane — it is nothing (2026-09-06).** The legacy flat
coloured split (`create_colored_split_tx`, `clients/libs/rust/src/rgb.rs`) is retired outright:
`UtexoWallet::refuse_legacy_colored_split_lane` refuses every route into it *before any SE co-sign*,
on both settings of `colored_ladder`, and `register_split_subcoins_n` / `register_combine_subcoins`
refuse again behind it. The migration hatch that used to let an un-colourable carrier through is
closed with it. And the pin does not only gate carriers: the SDK establish pass's
`get_statechain_info` call resolves the identity pin → configured value → refuse, so on an unpinned
network with nothing configured **every** deposit — plain or coloured — is left un-laddered
(`LadderSkipReason::AttestationIdentityUnpinned` for a plain coin, `RgbCarrier` for a carrier, whose
arm runs first), with no flat backup underneath and therefore no exit material at all: not
conveyable, not unilaterally exitable, and recoverable only by cooperative withdrawal (and not even
that for a carrier, which `withdraw` refuses). §3c and §8 still *describe* the legacy flat coloured
split, but only as history — it is a lane nothing takes now, and this document says which lane it is
describing every time it matters.

### 2.1 The constants everything is derived from

Read once, used everywhere below. Mainnet is `TesrParams::mainnet()` (`lib/src/tesr.rs`):
`d0 = 1440`, `δ = 36`, `d_floor = 144`, `e0 = 720`, `δE = 36`, `e_floor = 144`, `m_max = 15`,
`committed_fee_rate = 3.0`. `flat_ladder_params_const` still returns `(initlock, interval)` —
`(10 000, 100)` on bitcoin/testnet/testnet3/testnet4/signet, `(1 000, 10)` on regtest — but there is
no flat chain over `F` for it to parameterise any more (2026-09-06). `initlock` survives as the
**fixed window** the split-depth cap measures an exit walk against (§7), and `interval` is applied to
nothing.

```
TIER_VBYTES                     = 125        lib/src/tesr.rs   (measured signed vsize, 1 payload + P2A)
P2TR_OUT_VBYTES                 = 43         lib/src/tesr.rs   (one extra payload output)
COLORED_TIER_VBYTES             = 168        clients/libs/rust/src/rgb.rs   (= 125 + one opret out)
P2A_VALUE                       = 240        lib/src/tesr.rs
DUST_LIMIT                      = 330        lib/src/tesr.rs   (P2TR relay floor, one number for every script type)
BACKUP_TX_VBYTES                = 112        clients/libs/rust-sdk/src/transfer.rs

committed_fee(r)                = ceil(125·r)                        ->  375 @ r = 3.0
committed_fee_for_outputs(n,r)  = ceil((125 + 43(n−1))·r)            ->  504 @ n = 2, r = 3.0
colored_committed_fee(n,r)      = ceil((168 + 43(n−1))·r)            ->  504 @ n = 1;  633 @ n = 2
tier_out_total(v,n,r)           = v − committed_fee_for_outputs(n,r) − 240

rung  = committed_fee + P2A     = 615 plain / 744 coloured           @ r = 3.0
min_child_value(3.0, 330)       = 2·615 + 330 = 1 560                lib/src/tesr.rs
min_spine_tip_value(3.0, 330)   =   615 + 330 =   945                lib/src/tesr.rs
colored_child_floor(3.0, 330)   = 2·744 + 330 = 1 818                clients/libs/rust/src/tesr.rs
colored_spine_tip_floor(3.0,330)=   744 + 330 = 1 074                clients/libs/rust/src/tesr.rs
min_split_output(r)             = 330 + ceil(112·r)  ->  442 @ 1.0,  666 @ 3.0
split_fee_reserve(parent)       = clamp(parent/100, 300, 2000)       clients/libs/rust-sdk/src/transfer.rs
```

**Every floor above is a function of the RATE, and quoting one without its rate is quoting a rate.**
`min_child_value` and `min_spine_tip_value` take `fee_rate_sats_per_vb` as an argument; the numbers
in this document are those functions evaluated at the shipped `committed_fee_rate = 3.0`.
`min_split_output` takes a *different* rate — `backup_fee_rate` = `min(SE quote, cc.max_fee_rate)`,
the rate a sub-coin's own backup will actually be signed at — which moves with the fee market. So
the bare backup floor moves with the market while the ladder floors, pinned to a protocol constant,
do not.

---

## 3. Mechanics, with real numbers

Four walkthroughs: a first plain payment, a second one, tokens, and paying several people at once.
The arithmetic is exact.

### 3a. Sending 0.1 BTC out of a 1-BTC coin — the in-ladder split

Alice holds one confirmed coin of 100,000,000 sats (comfortably inside the `u32` per-coin ceiling of
~42.9 BTC, SPEC §14.2 L-17) and calls `transfer(bob_address, 10_000_000)`. Her deposit was laddered
at `claim()`, so `parent_shape` reports `Root`.

**Step 1 — plan.** `UtexoWallet::plan_payment` resolves a shape per candidate, takes the *smallest*
floor any candidate's legs impose (`SplitFloors::planning()` = `piece.min(change)`), and hands that
to `select::plan_with_floor` (`clients/libs/rust-sdk/src/select.rs`). That planner first looks for an
*exact subset* summing to 10,000,000 by dynamic programming over reachable sums (`exact_subset`).
One 1-BTC coin has none, so the plan is `Plan::WithSplit { split, split_amount: 10_000_000 }`. The
named coin is then re-judged **per leg** by `split_preflight_pure`, which BINDS; if it refuses, that
candidate is marked unsplittable and the planner runs again, so one awkward coin does not fail an
otherwise fundable payment.

**Step 2 — admission arithmetic**, pure and run before anything is touched. Each pre-signed tier
burns a committed fee plus a P2A anchor, so what the two legs share is `tier_out_total`:

```
F        = 100,000,000                                        (the on-chain 2-of-2)
T.out    = 100,000,000 − (375 + 240)   = 99,999,385           trigger,   built at claim()
X_0.out  =  99,999,385 − (375 + 240)   = 99,998,770           extension, built at claim()
split total = tier_out_total(X_0.out, 2, 3.0)
            =  99,998,770 − (504 + 240) = 99,998,026
piece  = 10,000,000                                           exact, requested
change = 99,998,026 − 10,000,000 = 89,998,026                 derived, not free
checks: piece  >= min_child_value(3.0, 330)     = 1,560  ✓
        change >= min_spine_tip_value(3.0, 330) =   945  ✓
```

`inladder_amounts_floored` enforces `piece + change == total` exactly — there is no fee reserve on
this lane; the tier's fee is committed inside it — and refuses with a message naming *which* leg
fell short and what that leg's own floor is.

**Step 3 — terminalize, then co-sign `SP`.** The SDK sets the parent's spend budget at the SE to
exactly one more signature (`set_spend_budget(.., 1)`, SPEC REQ-18) and spends it on the split
state. The SE co-signs **blind**: `cosign_tier_request` (`lib/src/tesr.rs`) builds the taproot
`SIGHASH_ALL` sighash *client-side* and hands the SE a MuSig2 session; `SignFirstRequestPayload`
(`lib/src/transaction.rs`) carries a statechain id and a signature over it, nothing else. The SE
learns that id `P` signed *something*. The result is fully signed and **not broadcast**:

```
       split state SP (v3/TRUC, nSequence = SPINE_CSV = 0, un-broadcast, committed fee 504 sat)
            ┌────────────────────────────────────────────────────┐
 X_0.out ──▶│ in:  X_0.out[0]   (99,998,770, 2-of-2)             │
 (extension │ out0: 10,000,000 → A_piece    "piece child"        │──▶ conveyed to Bob
  tier, un- │ out1: 89,998,026 → A_tip      "spine tip"          │──▶ stays with Alice
  broadcast)│ out2: 240        → P2A anchor (anyone-can-CPFP)    │
            └────────────────────────────────────────────────────┘
```

Be precise about what the budget buys. A budget bounds *future* co-signs; it cannot retract one
already issued, and `T` was co-signed back at `claim()`. What makes this safe is not the budget but
the **shape**: `SP` spends `X_m.out[0]`, a descendant of `T`, so a retained trigger can only start
the clock on the owner's own chain rather than void the split. `SPINE_CSV = 0`
(`clients/libs/rust/src/tesr.rs`) is what all three split builders sign, and it is correct for the
same reason: over that outpoint only two transactions can ever exist — the sender's own retained cap
and, later, the next `SP` — so the honest one is given the largest possible margin, and the party
who could void it is the party being paid out of it.

**Step 4 — legs.** The piece child gets its own headless two-tier ladder (an extension at
`ext_csv(0) = 720` and a state at `state_csv(0) = 1440`) co-signed under its aggregate by
`establish_child`. The change leg does **not**: `change_leg_role(SplitLane::PlainRoot)` reports
`SpineTip`, and `establish_spine_tip_journalled` gives it ONE cap tier directly over `SP.out[1]`.
The extension exists to reset the state budget by renewal, and on the spine every payment already
lands the change on a virgin outpoint at a virgin `D0`, so the rung would be dead weight. That
missing rung is 615 sat and 720 blocks saved per level, and it is why a payment adds exactly one
transaction to the sender's own exit chain.

The piece child's bundle travels to Bob's mailbox together with the standard SE key-handover
material and the ancestor chain `F → T → X_m → SP`. Bob's `claim()` runs `verify_child_bundle`
(§5), then *completes* the handover through `/transfer/receiver`: the SE rotates its share so the
aggregate `A_child` is invariant — which keeps every pre-signed child tier valid — and `auth` moves
to Bob. Alice is permanently locked out.

**What each party ends with.** Bob has a 10,000,000-sat first-class child: he can pay it onward
off-chain whole (`child_retransfer`) or split it again (`child_in_ladder_pay`, §8.1). Alice holds an
89,998,026-sat spine tip. The parent is terminal. The chain saw nothing. The SE saw hashes.

Evidence: `sdk58` (a real split child is accepted, and twelve adversarial bundles reject), `sdk59`
(end-to-end split payment, receiver claims and exits), `sdk60` (alice → bob → carol with the funding
outpoint unspent throughout), `sdk76` (splitting a *received* laddered coin still yields an adoptable
child), `sdk81` (a hard process kill mid-split is recoverable through the split journal).

### 3b. The second payment — the spine batch

Alice's change is now a `spinetip-` row, and `parent_shape` reports `SpineTip`. Her next payment
routes to `spine_batch_pay` → `spine_batch_split` (`clients/libs/rust/src/tesr.rs`), which builds
`SP_2` over the tip's own funding outpoint `SP_1.out[K]` at `SPINE_CSV`, retires the old cap into
the segment's superseded set, terminalizes **the tip's** slot, and leaves another one-cap tip.

Two consequences that are easy to get backwards:

* The batch's `SP` sits at `SPINE_CSV = 0` while the new cap sits at `state_csv(0) = 1440`. They are
  two different tiers with two different bounds. Pin the cap at `SPINE_CSV` and it ties with every
  future `SP`, and the builder's own `cap_csv <= SPINE_CSV` guard then refuses the next batch —
  stranding the tip when it is already terminal. `SpineTipBundle::validate()` is a precondition of
  `persist_spine_tip` and checks exactly this, among other structural facts, before any value check.
* A spine level costs the exit walk **one** transaction, not two, so the depth cap charges levels by
  shape (`SplitLevelShape`, §7). Charging a spine level as two is a silent economic cap; charging a
  two-tier level as one mints a leaf whose exit does not fit the fixed `initlock` window.

A tip is deliberately not a coin any other builder can take: it has no `tesr-` row so `in_ladder_pay`
cannot load it, no `ctesr-` row so `child_in_ladder_pay` cannot either, and `transfer` refuses to
hand one over **whole** by name — there is no conveyance builder for a tip and no flat lane to fall
through to (2026-09-06); a coin conveyed over an un-broadcast funding output would have no exit. Splitting it is the designed path; handing it
over is open work.

### 3c. Sending 0.1 TKN out of a 1.0 TKN allocation

Token amounts are **raw `u64` units**. `precision` (a `u8` in the contract metadata, fixed at
issuance) says only where a UI should put the decimal point; no SDK path scales by it. "1.0 TKN" at
precision 2 is 100 raw units; "0.1 TKN" is 10.

A carrier is funded at issuance with `TOKEN_CARRIER_SATS` (`clients/libs/rust-sdk/src/tokens.rs`),
which is **derived, not chosen**, and derived from the larger of the two lanes' requirements —
because the lane is picked per spend by a flag a wallet may flip after issuing:

```
legacy_carrier_sats(5) = 5 · (TOKEN_PIECE_SATS + 300) + 666 = 5 · 4,374 + 666 = 22,536
TOKEN_PIECE_SATS       = 3 · (ceil(168·6.0) + 240) + 330    = 3 · 1,248 + 330 =  4,074
```

`TOKEN_PIECE_SATS` is the coloured **root** floor computed at twice the committed tier rate
(`TIER_COMMITTED_FEE_RATE · PIECE_FEE_RATE_HEADROOM` = 6 sat/vB), so a received piece still clears
that floor if the committed rate ever doubles. `LEGACY_CARRIER_TAIL = 666` is `min_split_output(3.0)`
— the last change must still be able to fund its own backup. `LEGACY_CARRIER_SEND_DEPTH = 5` chained
sends; the coloured-ladder lane's `CTESR_CARRIER_SEND_DEPTH` is **1**, structurally, so the carrier
is sized for the legacy lane and over-provisions the other. Sizing for the max is the fail-closed
direction — an over-sized carrier parks sats in a change child, an under-sized one is refused at
spend time with the carrier already terminalized — and where the coloured lane is the one taken,
22,536 buys one send and the remainder lands in that depth-1 change child, movable only whole or by
exit. A real cost, stated rather than hidden behind a number that reads like five sends.

A carrier is **never given a *plain* ladder** (SPEC INV-29): a plain T/X/S tier spend is sats-only
and would destroy the allocation. Where an attestation identity is pinned it gets a **coloured** one
instead (§2) and its first payment is the coloured *in-ladder* split; where none is,
`claim()` leaves the carrier un-laddered and records `LadderSkipReason::RgbCarrier`, and that carrier
has no payment lane and no exit at all.

> **What follows in this sub-section is the RETIRED legacy flat coloured split, kept because the
> arithmetic explains where `TOKEN_PIECE_SATS` and the token floors come from and because rows minted
> before 2026-09-06 still exist.** It is not a lane any wallet takes now:
> `refuse_legacy_colored_split_lane` declines every route into `create_colored_split_tx` /
> `create_colored_combine_tx` before anything is co-signed, whichever way `colored_ladder` is set,
> and the migration hatch for un-colourable carriers is closed with it. On the lane that ships, the
> arithmetic is the in-ladder one of §3a wearing an RGB hat — no `split_fee_reserve` at all (the
> tier's fee is committed inside it), per-leg floors that are the max of `min_split_output` and the
> coloured floor for that leg's shape, and a piece carved at `TOKEN_PIECE_SATS` with the change
> landing on a one-rung coloured spine tip.

Its arithmetic was the plain split's wearing an RGB hat: `split_fee_reserve` off the top,
`min_split_output` under both legs, over `create_colored_split_tx` instead of a sats-only
transaction.

`create_colored_split_tx` built one SE-co-signed, un-broadcast transaction in which each output
carries an `(address, sats, rgb_amount)` triple, and rgb-lib inserts exactly one OP_RETURN carrying
the opret commitment to the state transition (SPEC INV-11):

```
carrier = 22,536 sats holding 100 raw units;  fee_reserve = clamp(22536/100, 300, 2000) = 300
piece   = (4,074 sats, 10 units)       ← TOKEN_PIECE_SATS: every piece carries the same sats;
change  = (18,162 sats, 90 units)        the sats are packaging, the token is the payload

            colored split tx (locktime 0, un-broadcast, fee 300 sats)
            ┌────────────────────────────────────────────────────┐
 carrier ──▶│ in:  carrier outpoint (22,536 sats + 100 units)     │
            │ out:  4,074 sats  ⟨seal: 10 units⟩   "piece"        │──▶ Bob
            │ out: 18,162 sats  ⟨seal: 90 units⟩   "change"       │──▶ Alice (new carrier)
            │ out: OP_RETURN ⟨opret commitment⟩                   │
            └────────────────────────────────────────────────────┘
```

Conservation (`Σ recipient units + change = carrier allocation`, SPEC INV-13) is enforced by rgb-lib
at colouring time, and the vouts are recomputed from the coloured transaction rather than assumed —
the OP_RETURN's position is not fixed. The **consignment** — the cryptographic history proving the
10-unit assignment — travelled inside the owner-encrypted transfer message as a
`ConsignmentEnvelope{c, a, s}` on `BackupTx.rgb_consignment`: consignment, an *advisory* amount hint,
and the piece's sats. No conveyance writes a backup row any more, so that field is dead on the wire;
the same `{c, a, s}` envelope shape is what `peek_pending_transfers` now *reconstructs* from a
coloured bundle for the SSP's pre-pay gate.

**Receiver booking is consignment-governed, not envelope-governed — and that rule outlived the
lane.** Bob's SDK validates the consignment off-chain against the witness txids, then books the
amount the consignment assigns to **his own witness outpoint** (SPEC REQ-21); a disagreement with the
envelope hint rejects the whole transfer (ERR-8). The contract id comes from the validated
consignment, never from the sender (REQ-22), and only fungible assignments count — an inflation right
can never book as balance (INV-26). A lying envelope buys a failed payment, never an inflated
balance. What changed on 2026-09-06 is only *where the material rides and what it resolves against*:
on the coloured lane the consignment is the **leaf consignment** carried in the child bundle, and it
resolves against the coin's own tier txids (`colored_child_txids()` for a child, `ladder_txids()` for
a root ladder) rather than against branch transactions. The single predicate
`verify_consignment_assignment` still serves both the claim path and the SSP's pre-payment gate.

**What Bob can do with 4,074 sats of packaging.** He can hold it, he can exit it by walking its
ladder (§8.6), and on the coloured lane he can forward it **whole** (`transfer_colored_child`). What
he cannot do is split it: a coloured child-level split does not exist, so a received piece cannot
fund *part* of a payment. *(On the retired flat lane the same limit had a different cause and a
different escape: `transfer_tokens` refused "carrier coin too small" whenever
`TOKEN_PIECE_SATS + fee_reserve >= carrier_sats` — `4,074 + 300 >= 4,074` — and two pieces could be
combined to clear `TOKEN_PIECE_SATS + fee_reserve + min_split_output` = 5,040 against their 8,148.
That combine is retired with the lane and is no longer a remedy.)*

### 3d. Paying several people at once: width beats depth

`transfer_many(&[(addr, amt), …])` carves **all pieces in one split**. (Its token sibling
`batch_transfer_tokens` no longer has a lane: a coloured carrier refuses K > 1 by name inside the
split engine — `refuse_colored_multi_payee`, because coloured pieces are conveyed serially after the
carrier is terminal and the lane journals no recipient address — and a carrier with no coloured
ladder is refused by `refuse_legacy_colored_split_lane`. Pay coloured recipients one per carrier.)
`transfer_many` dispatches on the parent's shape exactly as `transfer` does, so a laddered parent
gets one `SP` with N piece payloads plus the sender's own leg, each piece conveyed to its recipient's
mailbox with the standard key handover. `sdk69` proves the safety property by running the attack: a
retained trigger is broadcast against a multi-recipient split, and both recipients still exit for
their exact amounts.

Depth advances **per batch**, not per payment, which is the entire point. Every piece in a batch
sits at the same depth however many recipients share the tier, and `SP`'s width costs
`P2TR_OUT_VBYTES = 43` vB per extra payload output — `committed_fee_for_outputs` and
`build_split_state` are N-ary.

Two hard bounds on N:

* `MAX_BATCH_RECIPIENTS = 63` (`clients/libs/rust-sdk/src/wallet.rs`), stated by
  `refuse_oversized_slot_batch` as the outermost check in `transfer_many`. It comes from
  `DERIVED_SLOTS_PER_STATECHAIN = 64`, the SE's lifetime derived-voucher allowance per statechain
  (`max_derived_tokens_per_statechain`, `server/src/server_config.rs`), counted over lifetime
  issuance including spent rows. Each spine level is a *fresh* statechain, so the cap is per level,
  not global.
* The parent must be able to carry K legs at all. `min_batch_source_value`
  (`clients/libs/rust-sdk/src/transfer.rs`) is
  `K·floors.piece + floors.change + committed_fee_for_outputs(K+1, rate) + P2A`. At the shipped rate,
  with the backup floor not binding, that is `1,689·K + 1,560` — **3,249** at K = 1, **18,450** at
  K = 10, **35,340** at K = 20. It is refused *first*, on the aggregate, so the message names K and
  the shortfall instead of naming one leg and reading like a distribution problem the caller could
  fix by paying less.

The caveat that remains: a batch is **not atomic across recipients** (SPEC §14.2 L-14). Hand-offs
are independent, and a dropped one leaves that piece reclaimable by the sender — the split parent is
terminal, so there is no double-spend, but there is no all-or-nothing either.

---

## 4. The floors, and where each one comes from

This is the part of the system a wallet author has to internalise. There is no single minimum. There
are **per-leg** floors, resolved by **lane**, and the larger of two candidates binds.

`split_output_floors(backup_fee_rate, shape)` (`clients/libs/rust-sdk/src/transfer.rs`) is the one
place any of them is computed. It returns `SplitFloors { piece, change, lane }`, and every admission
guard and the quote derive from it and nowhere else — which is what makes `fundable: true` followed
by a refusal inexpressible. Each leg takes the max of:

* **`min_split_output(backup_fee_rate)`** — the dust limit plus the fee that leg's own backup
  transaction must pay. Below it the sub-coin exists but can never be exited.
* **the ladder floor for that leg's SHAPE** — and the shape is deliberately *not* a parameter. It is
  read from `change_leg_role(lane)`, the one function that describes what the builders actually
  emit, so the floor a payment is admitted at and the ladder that is then built cannot be two
  different shapes.

| floor | value @ shipped rate | where it comes from |
|---|---|---|
| Split-output dust floor | **330** (`DUST_LIMIT`) | the P2TR relay threshold. One number for every script type on purpose: P2WPKH would relay at 294, and a per-script table would reintroduce exactly the sender/receiver floor drift the constant exists to remove. Exempt: the P2A anchor (own threshold 240) and the provably-unspendable opret |
| Smallest **viable sub-coin**, on every lane (`min_split_output`) | **`330 + ceil(112·r)`** — 442 @ 1 sat/vB, 666 @ 3 | 330 is only the split *output* floor; a 112-vB spend of the piece must still clear dust after its fee, so a 330-sat piece cannot fund its own way out. The **universal lower bound**: it survived the retirement of the un-laddered shape, and of the flat backup itself (2026-09-06), as a dust-plus-fee fact about the transaction that eventually spends the output — no piece carries a backup any more. `transfer_many` states it first, before any parent is chosen; the in-ladder routes then raise it per-parent. Enforced on both legs by `split_amounts_floored` **before** the parent is made terminal. Pinned by unit `granularity_model::backup_fee_floor_is_the_true_mintable_minimum` |
| Smallest coin the **shape-blind planner** will propose splitting | **`2·min_split_output + reserve`** — 1,184 @ 1 sat/vB | piece + change must each clear the floor and `select::plan_with_floor` still deducts `split_fee_reserve` on top. That reserve survives as the planner's *conservatism*, not as a charge: no in-ladder route takes one (the tier's fee is committed inside it), so the bound is strictly stricter than the executor's and re-planning is what recovers a coin it declines |
| **ADMISSION** floor for a payee's leg (every lane) | **1 satoshi** | REQ-83. `split_output_floors` sets `piece: SplitLegRole::Tail.min_value(..)`, so no amount is refused for being small; `LeafShape::for_value` then picks the shape that gets BUILT. The rows below are BUILDER floors — the value at which each band becomes available — not gates a payment must pass |
| Smallest **two-rung** in-ladder piece (`LeafShape::Laddered`) | **1,560** = `min_child_value(3.0, 330)` = `2·(375+240) + 330` | a payee's piece funds its OWN extension and state rung before its final output can clear dust. Below it the leg is built as a one-rung `SpineTip` (≥ 945), a `Stub` (≥ dust) or a `Tail` — and `LeafShape::exits_unaided` is **false** for the bottom two, so they are payable but not unilaterally exitable. **The CHILD lane is the exception and the hazard:** `child_in_ladder_split` hard-codes `SplitLegRole::Piece` and never consults `LeafShape::for_value`, so a child-lane leg under 1,560 is admitted, terminalizes the parent, and *then* fails |
| Smallest **spine tip** (sender's change on the plain-root and spine-batch lanes) | **945** = `min_spine_tip_value(3.0, 330)` = `615 + 330` | the tip is one cap tier and no extension. The COLOURED lanes give their change a tip too (`change_leg_role(SplitLane::Colored)` = `SpineTip`) but floor it at the coloured number in the row below |
| Smallest **in-ladder change on the plain-CHILD lane** | **1,560** | `change_leg_role(SplitLane::PlainChild)` is `Piece`: a child-level split gives BOTH legs two tiers |
| Smallest **coloured** piece / tip | **1,818** / **1,074** | `colored_child_floor` / `colored_spine_tip_floor`: every rung carries an opret, so the rung is 744 rather than 615 |
| Smallest **in-ladder splittable** coin (K = 1) | **3,249** | `min_batch_source_value`: `1,560 + 945 + committed_fee_for_outputs(2) + 240` |
| Smallest token send | **1 raw unit** | amounts are `u64`; rgb-lib conserves them exactly; sub-unit resolution does not exist — that is what display-scaling `precision` is for |
| Smallest token-capable carrier (legacy lane — **RETIRED**, unreachable) | **`TOKEN_PIECE_SATS + reserve + min_split_output`** — 5,040 @ 3 sat/vB | the fit guard fired at `piece + reserve >= carrier`; the change then had to clear its own backup floor. Both guards now sit *behind* `refuse_legacy_colored_split_lane`, so the message a caller actually gets is the retirement, not "carrier coin too small" |
| Highest feerate a token split is admitted at (legacy lane — **RETIRED**, unreachable) | **33 sat/vB** | the *piece itself* had to clear `min_split_output(r)`, and `330 + ceil(112·34) = 4,138` exceeds the fixed 4,074-sat packaging. Same note: the retirement gate fires first |

**Above its lane's floor, resolution is exactly 1 sat.** In-ladder, any piece in
`[1,560, tier_out_total(X_m.out[0], 2, 3.0) − 945]`. On a **coloured** split — retired flat lane and
shipping in-ladder lane alike — the piece's sats are not a free parameter at all: every piece is
carved at `TOKEN_PIECE_SATS`, and it is the *token* amount that carries 1-raw-unit resolution.

**Everything is refused up-front, with the parent untouched.** That is the property worth stating
loudest, because the alternative is not a failed payment — it is a *stranded coin*. Refusing after
`set_spend_budget(.., 1)` leaves the parent permanently terminal with no child to show for it,
recoverable only by unilateral exit. `split_preflight_pure` is the pure function that decides, and
the planner, the quote and the executor all call it.

Two conservatisms worth knowing, both in the safe direction:

* **The planner is one sat stricter than the executor.** `plan_with_floor` filters candidates on
  `amount > remaining + reserve + min_output` — strict — while `split_amounts_floored` refuses on
  `<`. So a coin the executor would accept at exactly the boundary is not *proposed*. Pinned by
  `granularity_model::split_bounds_exact_boundary`.
* **The planner's floor is advisory, the per-leg one binds.** `plan_payment` hands the planner
  `SplitFloors::planning()` = `piece.min(change)`, so it never refuses a split some candidate could
  actually make; the named coin is then judged per leg, and a refusal re-plans rather than aborting.

---

## 5. What a partial send does to invalidation

On the retired flat coloured lane (before 2026-09-06) a split composed out of absolute locktimes, a
locktime-0 branch and the receiver's branch walk. On the ladder — the only lane now — consensus-level
relative timelocks and a disclosure **census** do that job. Item by item, with the retired lane kept
for contrast:

**The parent dies, permanently and publicly.** Setting the budget to one more signature and
spending it on the split makes the parent terminal: the SE will never co-sign it again — not for a
withdraw, not for a transfer, not for a fresh backup, not for the legitimate owner (SPEC REQ-18;
budgets only tighten, INV-24, so termination cannot be undone through the API). Anyone can read
`GET /statechain/spend_budget/<parent>`. Every partial send therefore *consumes* a coin: granularity
is bought with coin lifecycle, not with trust.

Stated honestly, the budget is a **co-sign** bound, not a **spend** bound. On the flat coloured lane
that gap does not open, because every spend of the parent's funding needs a fresh co-signature. On
the laddered lane it is closed by geometry instead — `SP` descends from `T` — and that is now the
*only* closure the design relies on: the plain split that used to need refusing (because it raced
`T` for `F` rather than descending from it) no longer exists to be refused.

**Both legs are new coins one level down.** *(Retired flat lane: piece and change were sub-coins with
fresh backup ladders anchored at the split height, sharing one exit branch.)* In-ladder: the piece gets its own two-tier ladder hung off
`SP.out[j]` by `establish_child`, the change gets a one-rung cap, and both carry the ancestor chain
`F → T → X_m → SP`. A child's clock is relative and starts only once `SP` confirms.

**On the ladder there is no calendar to move — and no exception (2026-09-06).** Every tier is
relative CSV and un-broadcast, so an idle coin never ages and pays no rent, and a *leaf* inherits no
deadline: the splitter retains no flat backup chain over `F` (no coin carries one; INV-5 is RETIRED,
INV-27 is unconditional), so nothing above the leaf matures on its own. What the leaf's subtree lives
inside is an event — the parent's un-timelocked trigger being broadcast — and its exit walk is bounded
by §7's depth cap against the fixed `initlock` window, not by any window that closes with time.
*(The "a leaf inherits the splitter's epoch expiry, which §7's headroom gate enforces" paragraph that
stood here is retired with the chain.)*

**What the receiver of a piece verifies.** Two check-sets, one per lane, both automatic in `claim()`.
The **branch** checks — what a coin arriving with `branch-` rows *was* verified by on the retired
flat lane; no such conveyance can be received now (`refuse_branch_material`; SPEC REQ-17's branch arm
is RETIRED), kept for contrast:

| Check | Defeats |
|---|---|
| Branch linkage root-first; root outpoint on-chain, unspent, **confirmed** | fabricated ancestry, 0-conf roots |
| Every branch tx locktime ≤ tip (locktime-0 in practice, INV-4) | branches that lose the exit race |
| Value conservation `Σout ≤ Σin` at every hop (INV-25) | script-valid but un-broadcastable branches |
| Full script/signature verification | unsigned or altered branch txs |
| Backup-ladder decrement + `num_sigs` (REQ-16) | stale-state handoffs |
| ≥ 1 named ancestor per structural input, each `terminal: true` at the SE (INV-20, ERR-7) | a sender keeping a spendable ancestor to double-spend the tree |
| Tokens: consignment validates against branch txids; booked = consignment-assigned amount (REQ-21/22, ERR-8) | amount and contract-id lies |

For an **in-ladder child**, `verify_child_bundle` replaces the branch walk with a census:

| Check | Defeats |
|---|---|
| The ancestor chain roots at an `F` that is on-chain, confirmed and unspent | fabricated ancestry |
| Every conveyed tier is fully signed, and the child's tiers pay the child's aggregate `A_child` | altered or unsigned tiers |
| Parent `terminal: true` at the SE | the sender re-spending the parent |
| **Exact-equality census**: the SE's attested `num_sigs` for each node equals `tiers + superseded` actually disclosed (the flat term is zero by construction), summed over the N-hop chain; every superseded state is parsed, signature-checked and carries a strictly HIGHER CSV than the tier replacing it | a hidden, un-disclosed rival state — including one signed before conveyance. A `.len()`-only count is paddable and an unparsed `csv: None` skips the race check, so neither is sufficient (SPEC ERR-15) |
| The child's exit walk — `exit_wait_blocks + exit_slack_margin` — fits inside the fixed `initlock` window (`enforce_exit_chain_length`, §7) | a payee handed a chain too long or too slow to walk; there is no epoch to run out of |
| The handover is then **completed** (`/transfer/receiver`): the SE rotates its share, `A_child` stays invariant, `auth` moves to the receiver | the sender staying a co-owner and signing a rival afterwards |

The census is what makes each hop auditable: a hop costs exactly one co-signature and discloses
exactly one superseded state, which the receiver counts and proves out-raced. The count has exactly
**two** categories now. *(Its third — `flat_backups` — is zero by construction since 2026-09-06:
`PARENT_V2_BASELINE` and `CHILD_V2_BASELINE` are both `0`, and a conveyance carrying any flat backup
is refused by name before the count is taken. The subtlety that used to live here — that the term had
to come from the parent's **conveyed** chain rather than a fresh local read, because a parent
received *k* times carried `1 + k` backups and a constant under-counted by exactly *k*, minting
children nobody could adopt — is retired with the term; `sdk76` is now the test that a received
parent's split still balances at a baseline of zero.)*

**The window between census and handover, stated exactly.** A sender's co-sign is refused by the
coordinator while a transfer of that coin is open — the pending-transfer lock. Its server-side extent
is measured: `sdk91` drives a payer who bypasses the client and POSTs `/sign/first` directly, and
gets HTTP 409 while the coordinator's **one-hour** transfer window is open and HTTP 200 with a
`server_pubnonce` once the row is older than an hour. So that window is the only *server*-side gate
on that path. `sdk90` shows an honest client is stopped by two independent **local** gates before it
ever reaches the SE. Both facts belong in a wallet author's model: the local gates are what an honest
wallet relies on, and the census plus the completed handover are what a receiver relies on.

Evidence: `sdk58` (accept + 12 adversarial child bundles reject), `sdk54`/`sdk55` (padding,
inversion, hidden low-CSV state), `sdk46`/`sdk47` (the census formula against a live SE), `sdk56`
(a repeated `sign/second` returns the cached partial and does not advance `sig_count`, so a retry
cannot brick the equation), `sdk60`/`sdk17` (multi-hop).

**The benign co-descendant hazard** (retired flat branch lane; kept because legacy rows exist). After Alice pays Bob from a split, Alice
(change) and Bob (piece) hold the *same* branch transactions, and either may broadcast them at any
time. That is harmless: broadcasting the shared branch only materialises both funding outputs on
chain earlier, and moves no ownership — each output is its own owner's 2-of-2. Receivers must expect
their sub-coin to become a flat coin without their involvement; the SDK treats an identical-tx
rebroadcast as success, and only a *different* transaction spending the branch root is an
`ExitBranchConflict`.

**Depth grows, and it grows linearly.** On the flat coloured lane, each level adds one branch hop
and one more 300–2,000-sat reserve burned at split time. In-ladder, each **two-tier** level adds
`SP` plus an extension — 293 vB and one extension CSV — while each **spine** level adds `SP` alone.
Nothing else compounds: leaf ladders are minted fresh, and a laddered coin has no idle clock to
consume. The cost of granularity is exit weight, fees and latency — not security.

---

## 6. What a partial send does to unilateral exit

**A piece on the flat branch lane exited in two moves — and no longer does (2026-09-06).** The
branch was locktime-free (INV-4), so it broadcast *now* and beat any locktimed stale ancestor backup
provided it landed before the earliest ancestor maturity; the piece's own backup then waited out its
fresh ladder. Neither half exists today: no conveyance carries branch material, no coin carries a
backup, and `unilateral_exit` **refuses** a coin whose only rows are `branch-` ones rather than
broadcasting them. For a legacy carrier, `materialise_carrier` (and `auto_exit_due`'s carrier loop)
still broadcasts such a branch — branch only, never a sweep — but that settles the RGB allocation,
it does not exit the coin.

**A laddered coin or an in-ladder child exits by walking tiers.** Nothing is locktimed against a
calendar and nothing races: broadcast `T`, then `X_m`, then each `SP`, then the leaf's own
`ext_child → state_child`, each waiting out its relative CSV after its parent confirms.
`unilateral_exit` (`clients/libs/rust-sdk/src/wallet.rs`) dispatches on shape with three arms probed
in order — laddered root, split child, spine tip — and a **named refusal** where the flat fallback
used to be (2026-09-06: there are no `branch-` rows plus latest backup to fall back to; "no `tesr-`
ladder row and therefore no exit material") — and is idempotent and incremental:
call it once per block until `complete`. A tier whose timelock is unreached is reported as
`ExitStatus { complete: false, wait_blocks > 0 }`, never as an error (SPEC REQ-25).

### 6.1 Cost, derived from the tier sizes

`tesr_exit_vbytes` and `tesr_exit_txs_for` (`clients/libs/rust-sdk/src/config.rs`) are the model, and
both are derived from `TIER_VBYTES = 125` and `P2TR_OUT_VBYTES = 43`:

```
walk        F → T(no lock) → X_m(ext_csv 0) → [ SP(spine, csv 0) → ext(ext_csv 0) ]×d → state(state_csv 0)
two-tier    txs = 3 + 2d          vB = 375 + 293d        (SP has two payload outputs; the rest one)
spine       txs = 4 + d                                  (SP alone per level, the tip's cap is the last rung)
```

`tesr_exit_vbytes` models the **two-tier** walk only; the shape is the `ExitShape` argument
`tesr_exit_txs_for` takes, and only the count is shape-aware. A spine level's own `SP` is
`committed_fee_for_outputs(K + 1, rate)`'s tier — `125 + 43K` vB for `K` payees plus the tip — so a
spine walk is cheaper per level than the two-tier figures below at the same depth.

| depth `d` | txs | vB | fee @3 | @10 | @50 |
|---|---:|---:|---:|---:|---:|
| 0 (laddered root, `sdk50`) | 3 | 375 | 1,125 | 3,750 | 18,750 |
| 1 (received piece, `sdk59`) | 5 | 668 | 2,004 | 6,680 | 33,400 |
| 2 (grandchild, `sdk17`) | 7 | 961 | 2,883 | 9,610 | 48,050 |
| 4 | 11 | 1,547 | 4,641 | 15,470 | 77,350 |
| **8 (the mainnet cap)** | **19** | **2,719** | 8,157 | 27,190 | 135,950 |

Pinned by `invalidation_model::exit_cost_scaling_model`, which asserts the per-level 293 and the
prefix 375 against the real tier constants and the exact fee arithmetic at each rate.

**The conservative count is the one every safety margin uses.** `tesr_exit_txs` reports
`ExitShape::TwoTier` unconditionally, and `3 + 2d ≥ d + 4` for all `d ≥ 1`. Over-counting a margin
makes a watchtower act *earlier*, which is safe; a shape-aware margin that guessed `ExitShape::Spine`
for a two-tier coin would act late. Callers publishing an economics figure must use `tesr_exit_txs_for` with the shape they are
actually describing, because there over-counting is simply a wrong number.

**The coloured walk has the same shape and the same transaction count, and costs more.** Every tier
carries an opret: 168 vB for a one-payload tier, 211 for two, so a depth-1 coloured walk is
**883 vB** against 668 plain.

### 6.2 Latency

`tesr_exit_wait_blocks` sums the relative locks **plus one confirmation per tier**, because a tier's
relative lock only starts counting once its parent confirms:

```
csv_total(d)  = d·(ext_csv(0) + SPINE_CSV) + (ext_csv(0) + state_csv(0))   = 720d + 2 160  (mainnet)
wait(d)       = csv_total(d) + tesr_exit_txs(d)                            = 722d + 2 163
```

Depth 0: **2,163** blocks ≈ 15 days. Depth 1: **2,885** ≈ 20 days. Depth 8: **7,939** ≈ 55 days.
Because `SP` is a spine tier at CSV 0, a level costs only its extension — 720 blocks, not 2,124 —
which is a two-thirds cut in the term that compounds with depth. Latency is **contagious**: a piece
inherits the identical ancestor chain, so the recipient of a payment inherits the sender's payment
history as exit latency. §7's cap is what bounds it.

### 6.3 Fees in a spike

**On the ladder the spike answer is built in.** Every tier carries a **P2A anchor** worth 240 sats —
`OP_1 <0x4e73>`, the standard anyone-can-spend anchor Bitcoin Core relays — so the owner, a keyless
watchtower or the operator can attach a live-rate fee child to a *pre-signed* transaction. The
committed 3 sat/vB is a floor that makes the base case relay standalone, not the whole fee:
`tier_is_relayable` (`lib/src/tesr.rs`) is the question "can this tier be broadcast on its own", and
it deliberately ignores the anchor, which is the conservative direction — package relay via
`submitpackage` *would* rescue an underpaying tier, and this tree has no `submitpackage` caller on
the keyless path, so counting the anchor would credit the ladder with a rescue nobody can perform.
`sdk45` proves a keyless tower can drive an offline owner's whole exit from a bundle holding no key
material; `sdk40` proves the SE blind-signs the v3 + relative-timelock + P2A shape.

The named residual (SPEC §14.2 L-15): the *bump* half exists on the exit path
(`exit_child_pass_with_bump`, `exit_spine_tip_pass_with_bump`, both wired into `unilateral_exit`),
but the *watch* half does not — `watch_child_pass_seen` and `watch_spine_tip_pass_seen` have no bump
variant, so a tower defending a child tier is stuck at the rate it was signed at.
`SdkConfig::fee_bump` is an `Option<FeeBumpConfig>` and ships `None` in both constructors, which is
the honest default: without a funding UTXO, a signer and a Core RPC endpoint there is nothing to bump
with, so a tier refused for fee reasons is reported as a stated limit rather than retried forever at
the same rate.

*(On the retired flat coloured lane the reserve was fixed at split time — `clamp(parent/100, 300,
2000)`, paid by the splitter out of change, with no RBF and no honest CPFP before the leaf's own
backup matured, every branch output being a 2-of-2. Legacy `branch-` rows still carry that
already-committed fee, and it is the fee they will land at whenever they are materialised.)*

### 6.4 A token piece's exit is the same broadcast wearing two hats

*Retired flat coloured lane (before 2026-09-06).* The coloured split transactions **were** the RGB witness transactions. When the
branch confirms the opret anchors confirm with it, and the allocation settles as an ordinary
on-chain RGB holding at the exact partial amount (SPEC INV-16) — seated on the piece outpoint,
provable from the consignment to any rgb-lib wallet, with no Mercury software needed to validate it.
`sdk39` drove this end-to-end at depth 2 and is re-derived onto the coloured lane (pending run):
depth 2 now arises as in-ladder splits, and the walk carries the allocation with it.

**On the lane that ships, the two hats are the same walk.** A coloured child's tiers *are* RGB
witness transactions, so walking `T → X_m → SP → ext_child → state_child` both exits the coin and
carries the allocation to the owner's own key — one act, not two. There is no separate settlement
step and no branch to broadcast.

Two precision points:

* **The consignment must survive.** Exit material for a token piece is its `ctesr-` bundle including
  the leaf consignment (for a legacy piece: branch rows + backup + `BackupTx.rgb_consignment`). None
  of it is derivable from the seed — it lives in the wallet DB and the exported recovery bundle.
  Mnemonic-only restore is total loss for *any* off-chain coin, and doubly so for tokens.
* **Settling and spending were different acts on the legacy lane.** Settlement needed nobody's
  cooperation; *moving* the settled allocation afterwards meant spending the piece's 2-of-2 outpoint
  inside a new coloured witness transaction, which is cooperative while the SE lives. The sats-only
  escape that used to exist — a pre-signed plain backup sweeping `4,074 − ceil(112·r)` sats while
  carrying **no RGB commitment**, i.e. abandoning the allocation — **no longer exists**: no coin
  carries a backup, and no SDK path broadcasts one. So a legacy piece has *no* unilateral exit at
  all; `materialise_carrier` settles its allocation and stops there. On the coloured lane the walk
  above is the exit, and it keeps the allocation.

*(**The packaging-dust threshold, 37 sat/vB**, described that vanished sweep: the piece's 112-vB
backup cost `ceil(112·r)`, so at 36 sat/vB 42 sats survived and at 37 the sweep zeroed out. The unit
`granularity_model::token_packaging_exit_economics` still searches for it from `TOKEN_PIECE_SATS`, so
the number is live in the suite even though the transaction it prices is not.)*

**Carriers are protected from accidental destruction.** Because a plain-BTC spend of a carrier
outpoint destroys its allocation, every plain sweep in the SDK refuses carriers: `withdraw` and
`unilateral_exit` exclude them from all-coins defaults and hard-error on an explicitly named carrier
id (SPEC REQ-44), and balance arithmetic fails closed when RGB state is unreadable. The plain
off-chain split that once had to refuse carrier parents by name no longer exists to be pointed at
one. The allocation cannot be swept away by a fat-fingered `withdraw()`.

---

## 7. Admission: depth, exit length and headroom

Two caps bound what may be minted and what may be received — depth and exit-chain length — both
enforced and both **derived from the schedule**, not chosen; the third that stood here, the
exit-headroom gate, is retired with the calendar it measured.

* **Exit headroom, receive-side — RETIRED 2026-09-06.** *(Was: `check_exit_headroom` admitted a child
  only if its exit could finish inside the epoch it inherited, read off the validated flat chain;
  `sdk82` mined toward a flat backup's maturity.)* No coin carries a flat backup, so there is no
  epoch expiry to measure headroom against and `check_exit_headroom_with_margin` has **no caller**.
  What admits a conveyed child is the fixed-window rule below: `enforce_exit_chain_length`
  (`clients/libs/rust/src/tesr.rs`) requires `exit_wait_blocks + exit_slack_margin ≤ initlock` over
  the *signed* chain. `sdk82` is re-derived — a child has no epoch to run out of — pending run.
* **Depth.** `max_split_depth` (`lib/src/transfer/receiver.rs`), enforced build-side by
  `enforce_split_depth_cap_shaped` (`clients/libs/rust/src/tesr.rs`) against the live schedule and
  `initlock` as a **fixed exit window** (a compatibility constant cross-checked against
  `/info/config`, not a calendar). Because admission adds the margin on top of the bare latency rule,
  the caps are **depth 8 on mainnet** and **depth 54 on regtest**. The build side must not mint
  what the receive side would refuse (SPEC REQ-47): a builder using the bare rule against a payee
  using the margin rule mints depths unadoptable at every tip, and since the parent is terminalized
  before its child is conveyed, each such child is a stranded piece with a dead parent behind it.
* **Exit-chain length.** `max_exit_txs = 3 + 2·max_split_depth` — **19 transactions on mainnet**,
  **111 on regtest**. The latency rule alone cannot see this: a spine tier costs one block of latency
  and a whole transaction, so an all-spine chain of thousands of tiers passes the latency rule and
  is still unusable. `enforce_split_depth_cap_shaped` evaluates the length cap *above* the latency
  rule's early return and charges each level by its real shape (`SplitLevelShape`).

The cap is on the **chain**, not on one tier: an `SP`'s width is a free parameter. A sender's spine
tip walks `s + 3` transactions and a payee's piece `i + 4`, so 19 admits 16 spine levels for the
sender and 15 for a payee's piece.

**Depth is bounded, never reset.** A child cannot be re-anchored — `SP.out[j]` is un-broadcast, so
there is no confirmed outpoint to cooperatively spend. `refresh()` re-anchors *through* `withdraw`,
and `withdraw` routes a `ctesr-` row — and a `spinetip-` one, for the same reason — to
`unilateral_exit` rather than to a cooperative spend. It *can* be **renewed**: `renew_child` / `renew_child_auto` rebuild
`child_extension` + `child_state` in place over the same `SP.out[j]` for zero on-chain bytes and no
depth (`sdk84`). That matters because a leaf's whole-coin transfer budget is finite —
`child_supersede_csv` takes one `δ` off the state rung per hop, so `(1440 − 144)/36 = 36` hops per
extension rung — and renewal makes it `36 hops × 16 rungs` = 576 per depth level (a CSV budget spent
by hops, not a calendar). A leaf that has itself made a
partial payment is terminal at the SE and cannot renew; `renew_child` refuses that by name,
pre-flight, before burning a co-signature.

---

## 8. Real-world situations

### 8.1 You receive 0.1 BTC, then pay 0.03 out of it

Bob's piece from §3a is a received in-ladder child, and it is **first-class**: he completed the SE
key handover at claim, so he co-owns `A_child` and Alice is locked out
([../spec/CHILDREN.md](../spec/CHILDREN.md)). Two ways to spend it, both entirely off-chain:

* **Whole** → `child_retransfer`: co-sign one fresh state over `ext_child.out[0]` at a strictly
  *lower* CSV so it out-races the state it replaces, pay the new recipient, and disclose the
  replaced state as superseded. Spends zero sats, adds zero depth, never touches `ancestors`.
  Verified by `sdk60` — alice → bob → carol, `F` never spent, two payments and zero on-chain
  footprint.
* **Partial** → `child_in_ladder_pay`: the child's state is replaced by a split state paying two
  grandchildren, and the child becomes an intermediate segment in each grandchild's ancestor chain.
  Note the floor difference: on the plain-CHILD lane `change_leg_role` reports `Piece`, so **both**
  legs are floored at 1,560 rather than 1,560/945. Verified by `sdk17` (second hop non-exact, carol
  exits walking a depth-2 chain).

Either way the arithmetic is the tier model, not a reserve:
`piece + change == tier_out_total(ext_child.out[0], 2, rate)`. Each hop costs exactly one
co-signature and discloses exactly one superseded state. The grandchildren are depth 2: 7
transactions and 961 vB to exit (§6.1).

### 8.2 Wallet holds 60 + 50 TKN on two carriers and pays 100

`transfer_tokens` first looks for a **single** carrier holding ≥ 100. Finding none, it spans several
— and *how* it spans them is the part that changed on 2026-09-06.

**On the coloured lane there is no combine, and there cannot be one.** Each carrier's `F` is already
spent by its own trigger `T`, and `SP` spends exactly one `X_m`, so no multi-parent coloured tier
exists. `colored_multi_carrier_transfer` pays across carriers as a multi-**piece** payment instead:
one in-ladder split per carrier, each conveying a coloured child to the same recipient, who books
them as separate allocations summing to the amount. Terminality is then one terminal parent per leg
rather than N per transaction — and the legs are **sequential and not atomic**: a failure on leg
`k > 0` leaves legs `0..k` conveyed, so the recipient is short-paid rather than unpaid, and the error
names every piece already handed over. Two further limits are worth knowing before relying on it: a
received coloured CHILD cannot contribute part of a payment (no coloured child-level split exists),
and a carrier of the asset with no coloured ladder is *not* silently paid over the old lane — it is
excluded and named. `sdk31` drives this lane (pending run).

*(**The legacy combine is retired.** It selected carriers largest-allocation-first until the
allocation covered the amount and the selected sats cleared
`TOKEN_PIECE_SATS + reserve + min_split_output`, made each input terminal at the SE, and minted the
payment in ONE coloured combine transaction — N inputs → the recipient's piece + change. Its
invalidation argument was the branch's: one terminal ancestor per structural **input**, `validate_branch`
rejecting a non-tree branch and requiring every on-chain root confirmed. All of it goes with the flat
backups that exited a combine's outputs; `refuse_legacy_colored_split_lane` declines the route before
any co-sign and `register_combine_subcoins` refuses behind it.)*

### 8.3 Receiving the same asset twice

A wallet already holding asset X that receives a *second, separate* allocation of X books it, and the
balance **sums**. The accept path imports X's genesis only on first sight and is idempotent on an
already-known asset; the second receive brings in the new transitions and registers the new
allocation. This is the normal flow behind a merchant taking repeated payments in one token, and
behind a two-recipient batch landing both pieces at one receiver. Pinned by `sdk29` §(b2), where bob
receives the same asset twice — a partial piece and then a whole forwarded change coin — and his
balance sums across two independently adopted allocations.

### 8.4 Sending your entire balance vs almost your entire balance

**Entire balance** — or any amount an exact subset covers — is no split at all. `Plan::Exact` hands
each coin over whole: N coins, N transfer messages, the receiver ends up with **N coins**. No reserve
is burned, no depth is added; this is the cheapest possible payment shape and it actively consumes
odd coins.

**Almost-entire is the trap.** Coins `[500, 300]`, pay 600: no exact subset exists, the greedy pass
takes 500, and the 100-sat remainder is below the planner's floor and cannot be minted as a piece, so
`plan_with_floor` returns `Insufficient { available: 800 }`. The user sees "insufficient balance"
*despite* holding more than the amount. Usually that means no composition of coins can pay this
exactly; occasionally it is planner conservatism, since the greedy pass explores one largest-first
ordering and is one sat stricter than the executor at every boundary. What the SDK does about it:
`PaymentPlan` carries `split: Option<SplitChoice>`, so when candidates were refused on the way to an
`Insufficient` verdict, *that* refusal — with its named leg and its named floor — is the reason
surfaced, not a bare "insufficient". Remedies: pay a slightly different amount, use the whole coins,
or compose manually. Unit coverage: `select`'s own tests plus
`granularity_model::plan_paths_matrix`.

### 8.5 A merchant receives hundreds of small pieces

Each piece is a full coin with its own ladder and its own future exit. Offboarding N of them
cooperatively costs N withdraw transactions; unilaterally it costs N tier chains. Fragmentation is a
real, linear cost.

What exists to fight it:

* **Spend pieces onward.** Exact-subset selection actively consumes odd coins at zero cost, and a
  received child is first-class, so it can be paid on whole or split again (§8.1).
* **The leaf combine.** Once `SP` **confirms**, every `SP.out[j]` is an ordinary on-chain P2TR paying
  that leaf's aggregate key, and the owner can spend it with a *fresh* co-signature carrying no
  timelock — it does not have to be the pre-signed extension tier. N such outputs go in **one**
  transaction. `combine_leaves` (`clients/libs/rust/src/combine.rs`) is that driver, and `sdk83` runs
  it against a live SE and a live chain: alice splits into five leaves through one spine batch, the
  shared prefix `T → X_0 → SP` is walked on chain until `SP` confirms, and bob combines three of his
  four leaves into one address — one new UTXO, three leaf outpoints spent, carol's leaf untouched.
  It has no caller outside that test, so it is a proven primitive rather than a wallet feature.
* **Consolidate and redeposit** — N withdraws plus one deposit — where a cooperative SE is available.

There is no deadline bookkeeping (2026-09-06). A laddered *root* has no calendar, a *leaf* inherits
none (its splitter holds no flat backup), and a carrier with no ladder has no exit material rather
than a deadline. `auto_exit_due` (on by default via `SdkConfig::auto_exit`) has a legacy subject only —
`branch-` rows from before the rule; the defence that matters is `defend_ladders`, event-driven on `F`.

### 8.6 Exiting a token piece during an SE outage

You hold 10 units on a 4,074-sat piece at depth *d*, and the SE stops answering. This walkthrough is
the **retired flat coloured lane** — a piece minted before 2026-09-06 where no attestation identity
was pinned, so the carrier it came from was never laddered; no such piece is minted now, and a
carrier with no coloured ladder has no exit material (TRUST-MODEL B12). On the coloured-ladder lane
the piece is a coloured CHILD instead,
and its exit is the five-tier walk `T → X_m → SP → ext_child → state_child`, keyless and pre-signed,
carrying the allocation with it (`unilateral_exit` takes the `ctesr-` arm; `sdk75`).

1. **Nothing is lost by waiting a moment.** Your exit material is already local: the `branch-<id>`
   rows (the fully-signed coloured splits), the consignment, your key share (no plain backup exists).
2. **Materialise the branch.** Branch transactions are ordinary consensus-final transactions —
   broadcast them root-first with any tool; they are in the recovery bundle. Honest caveat:
   `unilateral_exit(piece_id)` will *refuse*, because the piece is a carrier and that operation is a
   plain sweep (§6.4). You rarely need to do it by hand: `auto_exit_due`
   (`clients/libs/rust-sdk/src/wallet.rs`) still **materialises** such a legacy carrier — branch-only,
   never a plain sweep — from the background watcher; there is no deadline behind it any more, only
   the legacy row (`sdk34` re-derived to the event-driven defence, pending run). A co-descendant
   exiting first also materialises the shared branch for free.
3. **Anchors confirm ⇒ the allocation settles.** Each coloured split's opret confirms with its
   transaction; after an rgb-lib refresh the 10 units are a settled on-chain RGB holding at your
   piece outpoint (INV-16), provable to anyone from the consignment.
4. **The sats and the future — and this step got worse on 2026-09-06.** The packaging stays in the
   2-of-2. If the SE returns, coloured cooperative spends resume. If it never does, **nothing**
   recovers the sats: the pre-signed plain backup that used to reclaim `4,074 − ceil(112·r)` after
   the leaf wait — at the price of abandoning the allocation — does not exist any more, and no SDK
   path broadcasts one. A legacy piece's *asset* is secured unilaterally and permanently by step 2;
   its *sats* are not, and there is no unilateral route to them at all.

So the honest summary for this class: the token state is secured unilaterally and permanently, and
both onward movement of the settled allocation and any recovery of the packaging sats want a live SE.

### 8.7 The change coin after the token is fully spent

Send the *entire* allocation (`token_amount == carrier_amount`) and the change output's `rgb_amount`
is 0, so it is left **uncoloured** — a plain BTC sub-coin. The RGB engine marks the old carrier
outpoint spent; the change coin is ordinary sats, transferable and withdrawable, with no
carrier guard applying. It gets **no ladder**, and that half is permanent: its funding is the
un-broadcast coloured split output, so a trigger over it would have no prevout to spend (**B0**),
which is exactly why `claim()`'s ladder pass is root-only. What *has* changed is what that means for
spending it. The plain off-chain split it used to take is deleted, so `parent_shape` refuses it
rather than routing it — and since `has_exit_material` is now **ladder-only**, the SDK counts such a
coin as holding *no* exit material even though its legacy `branch-<id>` rows are still on disk: it is
excluded from `fundable`, reported in `no_exit_material_coins`, and `unilateral_exit` refuses it by
name rather than broadcasting those rows. Reaching its value means getting the branch on chain (the
rows are consensus-final and broadcastable with any tool) and then spending the outpoint with a
co-operating SE. Carriers are not carriers forever. This is a property
of the legacy lane specifically: the
coloured-ladder lane consumes the whole of `F` in the trigger before any payment is carved, so a
spent carrier there leaves no BTC sub-coin at all and `colored_in_ladder_pay` carves no change child
when the allocation is fully paid out.

### 8.8 A high-feerate day

Covered in §6.3. The short version: **laddered exits have no deadline to beat** — they confirm
slowly, wait out their CSVs, and can be topped up through the anchor by a party that is not the
owner. *(The old counterpart — "a flat-branch exit confirms safely only if the branch lands before
the earliest hostile maturity, so broadcast early" — is retired with the maturity it raced: no
ancestor holds a matured spend of `F` any more. Materialising a legacy branch is now about settling
an allocation, not winning a race.)* What a sustained high-fee world moves is
the *floors*, not the safety: `min_split_output` is `330 + ceil(112·r)`, so the smallest viable
sub-coin on any lane rises linearly with the rate, while the in-ladder floors are pinned to
`committed_fee_rate`, a protocol constant, and do not move with the market at all.

### 8.9 Invoice flows

`create_tokens_invoice(asset, 50, …)` at precision 2 encodes **amount = 50** — raw units on the wire,
always. `UtexoInvoice.amount` (`clients/libs/rust-sdk/src/invoice.rs`) is a `u64`: sats when
`asset_id` is `None`, raw units when it is `Some` (SPEC REQ-28). `fulfill_utexo_invoice` checks
expiry (ERR-11), then routes to `transfer_tokens` — which lands on the coloured **in-ladder** split
where the carrier is coloured, and refuses where it is not (§3c's flat split is retired). The decoder
probes `version` *before* the full parse, so an unknown version is refused rather than mis-parsed.
Precision appears exactly once in the pipeline: at display time. What you see is scaled; what moves
is integral.

---

## 9. Privacy: who learns what from a partial payment

| Party | Learns | Does NOT learn |
|---|---|---|
| **SE** | that id `P` was made terminal and co-signed once more; that fresh child slots were initialised; the attested `num_sigs` counters the census reads; message-relay timing | **any amount.** `cosign_tier_request` computes the sighash client-side and `SignFirstRequestPayload` carries a statechain id and a signature over it — the SE sees no transaction, no output, no value, and cannot tell a split tier from a state re-sign. It never learns K, the denominations, the colour, or that a spine exists rather than a two-way split |
| **Receiver** | in-ladder: the ancestor chain `F → T → X_m → SP` and every superseded state the census requires, which exposes **your change leg's value**; for a coloured child, its leaf consignment and the witness chain that resolves it. *(Retired flat branch lane: the full branch, including your change outputs' values along its path, plus the consignment's transition-history subset.)* | your other coins; anything outside their chain's cone |
| **Chain observer** | nothing until someone exits; then the tiers become visible — amounts, the P2A anchors, and (tokens) the opret marking RGB use | who owns which output (fresh 2-of-2 keys); token amounts (RGB state is off-chain, the anchor is a commitment) |

Granularity is invisible to the operator by construction; the necessary trade is that a payee sees
the chain they must be able to verify, including the change values on it. Full disclosure is not an
accident — it **is** the census: a hop that hid a state would fail the receiver's exact-equality
count.

One coloured-lane detail worth stating where a wallet author will see it: seal blinding is derived
**per payload output** — `per_output_blinding(base, vout)` feeds `AssetColoringInfo::output_blinding`
(`clients/libs/rust-rgb/src/lib.rs`) — so a payee in a multi-payload batch cannot enumerate the vouts
and de-conceal a sibling's seal from their own blinding. On the legacy coloured lane the privacy that
matters is the consignment's, and it travels owner-encrypted inside the transfer message.

---

## 10. The UX perspective

**What the user types vs what happens.** `transfer(addr, 10_000_000)` — one call — hides coin
refresh, carrier filtering (token carriers never fund plain sends), auto-refresh of any near-final
coin before selection, exact-subset search, split planning, per-leg preflight with re-planning,
**lane dispatch** (root → in-ladder split, spine tip → spine batch, received child → child-level
split; a coin with no ladder is refused rather than routed), terminal-guarding, blind co-signing,
child or tip establishment,
and conveyance or per-coin handover. `transfer_tokens(asset, addr, 10)` likewise. There is no "split"
button and no protocol switch; the split is an implementation detail of exact amounts.

**Coin slots are free on the in-ladder path.** Each split leg consumes a slot from the SE's anti-spam
token server, but in-ladder legs draw **derived** vouchers against the parent — free, capped at
`DERIVED_SLOTS_PER_STATECHAIN = 64` per statechain over lifetime issuance, and `take_derived_tokens`
spends leftovers from an earlier attempt first and persists the pool before handing any out, so a
failed attempt costs the parent's allowance nothing.

**What the receiver sees.** Possibly **several coins for one payment**: an exact-subset payment of
100k covered by 60k + 25k + 15k arrives as three coins — three claims, one logical payment. A
split-based payment arrives as exactly one piece. `TransferResult { receiver_address, total_sats,
coins, used_split }` (`clients/libs/rust-sdk/src/types.rs`) tells the sender which happened;
receivers should sum, not count. A received in-ladder piece is a first-class coin, not a claim
ticket.

**Quoting.** `quote_transfer(amount)` → `TransferQuote { amount_sats, network_fee_sats,
renewal_fee_sats, total_fee_sats, fundable, stuck_coins, no_exit_material_coins, note }`. It runs the
executor's own planner and preflight, so `fundable` is what the executor will do rather than an
estimate. On an in-ladder split `network_fee_sats` is `in_ladder_split_cost(rate)` = the split tier
plus four rungs = **3,204 sat** at the shipped rate — deliberately **gross**: it does not credit the
superseded state rung the split replaces, and it prices four rungs even though the shipped root lane
gives its change leg only one, so the quote never comes in under what the tree actually gives up.
`stuck_coins` are coins worth less than their own renewal fee; `no_exit_material_coins` are coins
this wallet holds no exit material for on any lane, which is a different problem that combining does
not fix, and their value is excluded from `fundable`.

**Events** (`WalletEvent`, `clients/libs/rust-sdk/src/events.rs`): `TransferClaimed
{ statechain_ids }` — note the plural — `TokenTransferClaimed { asset_id, amount, statechain_id }`
(raw units), `BalanceUpdate { balance }`, plus the exit-side `ExitDeadlineApproaching
{ statechain_id, deadline_block, tip }` and `ExitBranchConflict { statechain_id }`.
`ClaimResult.token_results` carries per-statechain token outcomes separately from
`claimed_transfers`, because a Mercury coin can be CONFIRMED while RGB acceptance is still pending.

**Balances.** `TokenBalance { asset_id, ticker, name, precision, balance, total }` — `balance`
(settled) and `total` are raw `u64` units; rendering `balance / 10^precision` is the UI's job.
Plain-BTC balance excludes carrier sats entirely, and the arithmetic fails closed if RGB state is
unreadable. A wallet's balance is a function of **distinct statechain ids**, not of rows (SPEC
REQ-46): a second live row under one id would otherwise be one coin counted twice.

**The sharp edges, honestly.** A received **token** piece can be forwarded whole but never split, and
the two-piece combine that used to be the way round that is retired with the flat lane (§3c, §8.2);
received **sats** pieces are first-class and re-spendable either way. A multi-carrier token payment
is a sequence of independent legs, so it is not atomic; a coloured carrier refuses multiple payees
outright. Plain-BTC fragmentation has no shipped wallet-level combine, only the proven
`combine_leaves` primitive. The floors refuse rather than
strand, but they do refuse: 330 dust, `330 + ceil(112·r)` for the transaction that will spend a
sub-coin, 1,560 for a piece, 945 for a tip (1,818 / 1,074 coloured). The consignment and any legacy
branch rows are recovery-bundle material, not seed-derivable — and so is the ladder itself, which is
now the *only* exit material a coin has. And
"insufficient balance" sometimes means "no exact composition exists" — or, rarely, planner
conservatism — not "too poor".

---

## 11. FAQ

**Can I send 1 sat?** Not as a minted piece. No sub-coin on any lane may be smaller than
`330 + ceil(112·r)` — 442 at 1 sat/vB — because a 112-vB spend of it must still clear dust after its
fee (the floor was named for the backup that spend used to be; the arithmetic outlived it).
In-ladder, a payee's piece must additionally fund its own two rungs: **1,560** at the
shipped rate. Both are enforced up-front, so an under-floor piece is refused cleanly with the coin
untouched, never stranded. A pre-existing tiny coin can still move whole. Above the floor, resolution
is 1 sat.

**Why did my receiver get 3 coins for one payment?** An exact subset of the sender's coins summed to
your amount, so each was handed over whole — cheaper for everyone than splitting. Sum them; they are
one payment.

**Can the SE censor payments by amount?** It cannot *see* amounts — co-signing is blind — so no
policy keyed on value is possible. This is structural, not a promise: it signs 32-byte hashes and
cannot tell a tier from a backup or a small coin from a whole bitcoin (SPEC §14.1 L-3). It can refuse
service per statechain id or per authenticated owner, which degrades those coins to their SE-free
exit paths.

**Why is there 4,074 sats on my token piece?** Packaging, and it is derived rather than chosen: it is
the coloured **root** ladder floor computed at twice the committed tier rate, so a received piece
still clears that floor if the committed rate ever doubles. `TOKEN_PIECE_SATS` keeps pieces uniform;
the token amount is the payload.

**Can I re-send tokens I just received?** Whole, yes: a received coloured piece is a coloured child
and `transfer_tokens` forwards it entire (`transfer_colored_child`). Partially, no — a coloured
child-level split does not exist, so one piece cannot fund part of a payment. *(The old answer named
a different cause and a remedy that is gone: the flat lane refused "carrier coin too small" whenever
`TOKEN_PIECE_SATS + fee_reserve >= carrier_sats`, and combining two pieces cleared it. That combine
is retired.)* A received **sats** piece is first-class and re-spendable either way.

**What if the envelope lies about the token amount?** You book what the *consignment* assigns to your
outpoint; an envelope that disagrees rejects the whole transfer (ERR-8). Lying buys the sender a
failed payment, never an inflated balance.

**Can I pay across several carriers at once?** Yes, but as several legs rather than one combine:
`colored_multi_carrier_transfer` runs one in-ladder split per carrier and conveys one coloured child
per leg to the same recipient, who books them as separate allocations that sum. It is a sender-side
operation over *your* carriers, and it is **not atomic** — a failure after leg 0 short-pays rather
than un-pays, and the error names every piece already conveyed.

**Does splitting reset my clock?** A laddered **root** has no clock: every tier is relative-CSV and
un-broadcast, so an idle coin never ages and pays no rent. A **leaf** inherits none either
(2026-09-06): the splitter holds no flat backup chain over `F`, so there is no calendar for §7 to
measure against — only the fixed `initlock` window that bounds the walk's length. *(On the retired
flat coloured lane the leaf's clock reset at split height while the tree's root deadline
`H_deposit + initlock` never moved; neither exists now.)*

**Why can't I split my carrier's sats as plain BTC?** A plain split carries no RGB commitment, so
spending the carrier outpoint through it would destroy the allocation. There is no plain off-chain
split left to point at one — `split_coin` is deleted — and `withdraw` and `unilateral_exit` refuse a
carrier by name. Sats leave a carrier only inside a coloured split (§8.7).

**What is `precision`, and can it change?** A `u8` in the contract metadata, fixed at issuance,
consulted only by UIs. It cannot change, and no SDK path scales by it.

**Is 0.1 + 0.9 == 1.0 exact?** Always. There are no floats: 10 + 90 = 100 raw `u64` units, and
rgb-lib enforces exact conservation per transition (INV-13). Rounding error is structurally
impossible.

**Who pays the split fee, and where does it go?** In-ladder — the only lane there is —
there is no reserve at all. *(On the retired flat coloured lane it was the splitter, deducted from
change at split time as `clamp(parent/100, 300, 2000)` and becoming the split transaction's miner
fee.)* Every tier carries its own committed fee plus a 240-sat anchor, taken
off the tier's value as it is built — the split tier costs `committed_fee_for_outputs(2, 3.0) + 240`
= **744**, a payee's two rungs cost `2 · 615` = **1,230**, and the sender's tip one rung, **615**.
Same economics (spent, not escrowed), charged per pre-signed transaction rather than per split.

**What happens to dust-level change?** It never exists on any path. The split guards error and the
planner will not select a coin that would produce one, both before the parent is touched —
`inladder_amounts_floored` against per-leg `SplitFloors` in-ladder, and the coloured engine's own
per-leg `max(min_split_output, colored floor for the leg's shape)`. (`split_amounts_floored` was the
flat coloured lane's version and survives as the executable dust-boundary spec behind
`split_amounts`.)

**Can one carrier hold two different tokens?** Not in SDK flows: carrier selection, colouring and
booking all operate on one contract per coin, and issuance and receipt each bind one allocation to a
fresh outpoint. "One carrier, one asset" is the operative rule.

**Does a split cost me a transfer hop?** No. *(On the retired flat coloured lane, hops burned `interval` off a backup
ladder and a split minted fresh ladders for both sub-coins.)* In-ladder, hops decrement the
state CSV by `δ = 36` and a split gives each leg a fresh schedule of its own. What it costs instead:
the parent coin (now terminal), the fees above, one more level of exit weight and latency, and two
coin slots — derived, hence free, on the in-ladder path.

---

## 12. Comparison recap: granularity

| | **Ours (native off-chain split)** | **Spark (denominations + SSP)** | **Ark (per-round re-mint)** | **Lightning** |
|---|---|---|---|---|
| Resolution | 1 sat above the lane's floor (1,560 in-ladder piece, 945 tip, never below the `330 + ceil(112·r)` sub-coin floor); tokens 1 raw unit | fixed leaf denominations; odd amounts via SSP swap | arbitrary, but only at round re-mint | 1 msat nominal |
| Amount ceiling per payment | your largest coin (split) or any exact subset | leaf set + SSP pool depth | round capacity | channel capacity / inbound liquidity |
| Third party in a partial payment | **none** — the SE co-signs blind and sees no amounts | SSP pool required for odd amounts | ASP required, every round | route liquidity required |
| Cost per partial payment | 744 sat of split tier + 1,230 to the payee's rungs + 615 to the sender's tip; +1 depth for the batch, ~293 vB of future exit weight per two-tier level | swap fee/spread to the SSP | share of the round tx, every round | routing fees; liquidity opportunity cost |
| Recipients per split | up to 63, all at the same depth (`MAX_BATCH_RECIPIENTS`) | per-leaf | per round | per payment |
| Is the received partial amount re-spendable off-chain? | **yes, first-class** — the handover completes at claim, and it pays on whole (`child_retransfer`) or splits again (`child_in_ladder_pay`); `sdk60`, `sdk17`. Received *token* pieces are the exception: whole-forward only | yes (leaf transfer) | yes (out-of-round transfer) | yes (it is just balance) |
| Exit implication of receiving a partial amount | `3 + 2d` sequential txs, `375 + 293d` vB, `722d + 2 163` blocks; capped at depth 8 / 19 txs | leaf chain broadcast (depth of the Spark tree) | your branch of the round tree; **miss the round ⇒ swept** | force-close per channel; amounts not per-payment exitable |
| Idle rent | **0 vB** — nothing ages while un-broadcast | 0 | re-mint each round or lose funds | 0 |
| Deadline you inherit | none — neither a root nor a leaf has a calendar (2026-09-06) | none | every round | none |
| Operator sees amounts? | **no** (blind) | yes (SSP swaps by denomination) | yes (it constructs the round) | no (onion), but channel peers see HTLCs |

**Where this design LOSES, stated because a comparison that only wins is marketing.** A one-to-many
payout on Bitcoin is one transaction with N+1 outputs, about 44 vB per recipient — not N × 155. A
leaf walked out unilaterally costs `375 + 293d` vB over `3 + 2d` sequential transactions, which at
depth 1 is 668 vB against ~154 vB for an ordinary on-chain payment. What this design sells is every
payment **after** the first: on chain another ~154 vB each, off chain **zero**. The saving is a
function of how many times value moves before it settles, and the design rule follows — *a piece
received and immediately cashed out should never have been an off-chain split*. The full ledger,
including the sweep coverage at which the lane overtakes a batched on-chain payout, is
[../spec/PARTIAL-PAYMENT-ECONOMICS.md](../spec/PARTIAL-PAYMENT-ECONOMICS.md) §1.

---

## 13. What this document does not claim

* **The discharge round is DESIGN, not built.** SPEC §5.4 specifies retiring a whole tree in one
  transaction, which is what would make the footprint scale with *pieces held* rather than with
  *payments made*. Its SE-side enforcement point is empty. Every figure attributed to it is what it
  *would* cost, not what anything measures.
* **The sweep / absorption path is DESIGN, not built.** SPEC §5.3 (REQ-49–52) specifies absorbing a
  leaf at claim and handing the payee an ordinary root coin. No `sweep_*` parameter and no absorption
  predicate exists in the tree. What *is* proven is the structural fact underneath it: `sdk83` runs
  `combine_leaves` against a live SE and a live chain and consolidates three confirmed `SP.out[j]`
  into one UTXO.
* **Whole-coin handover of a spine tip is refused by name.** A tip's funding is un-broadcast, so any
  conveyance of it would hand the recipient a coin with no exit — and there is no flat lane to
  attempt one (2026-09-06). Splitting it (the spine batch) is the
  shipped path.
* **The token E2Es still set `colored_ladder = true` by name, and on regtest that is now
  belt-and-braces.** `sdk29`, `sdk31`, `sdk34` and the `sdk74`–`sdk79` family opt in explicitly
  (`sdk73` and `sdk78` were deleted outright in the same commit, along with `RGB_E2E` ids 1, 2, 3, 5,
  6, 8, 9 and 10 — do not cite them). Since the flag reads the compiled-in attestation pin, regtest
  ships it **on**, so the line no longer selects the lane there — it documents it. The *legacy* flat
  coloured lane is RETIRED (2026-09-06): `sdk39` is re-derived onto the coloured lane (pending run),
  and `rgb16` still runs, showing deterministically why rgb-lib refuses to colour a legacy-lane
  carrier — its finding stands even though the migration hatch it was written to justify is now
  closed. A network with **no** pinned enclave takes no lane at all: its carriers have no exit
  material, and neither do its plain coins (B12).
* **The keyless watchtower cannot fee-bump a child tier.** The exit path has bump variants; the watch
  path does not (SPEC §14.2 L-15).
* **`estimate_exit_cost` on a laddered coin reports the tier walk** — `backup_vbytes` is the signed
  vsize of the tiers, `wait_blocks` is 0 while idle, and `exit_deadline_block` is `None` — and since
  2026-09-06 that `None` IS the claim that the coin has no calendar: no ancestor holds a matured spend
  of `F`. Only a legacy `branch-` coin from before the rule reports a height there.

---

Further reading: normative requirements and constants in [../spec/SPEC.md](../spec/SPEC.md); the
tier machine, splits and RGB integration in [../spec/PROTOCOL.md](../spec/PROTOCOL.md) §5.4/§5.9/
§5.10; the child lifecycle, renewal and conveyance in [../spec/CHILDREN.md](../spec/CHILDREN.md);
the cost model in
[../spec/PARTIAL-PAYMENT-ECONOMICS.md](../spec/PARTIAL-PAYMENT-ECONOMICS.md); the residual trust
surface in [../spec/TRUST-MODEL.md](../spec/TRUST-MODEL.md); the Lightning latch in
[../spec/LIGHTNING.md](../spec/LIGHTNING.md). Primers: [transfers.md](transfers.md),
[tokens.md](tokens.md), [exits.md](exits.md), [invalidation-deep-dive.md](invalidation-deep-dive.md).

**Status of that evidence.** The unit and CI-guard suites are green (`granularity_model`,
`invalidation_model`, `select`, `split_math_tests` among them) and the E2E crate compiles, but **no
E2E flow has been re-run against the regtest stack since the flat backup was removed on
2026-09-06**. Read every `sdk*` / `rgb*` citation below as evidence *pending a run*.

Test evidence for the claims above: `sdk58`/`sdk59` (in-ladder split: verifier and payment),
`sdk60`/`sdk17` (first-class children, whole and partial re-transfer, depth-2 exit), `sdk69`
(multi-recipient in-ladder split under a retained-trigger attack), `sdk76` (splitting a received
laddered coin), `sdk81` (crash-safe split), `sdk82` (re-derived: a child has no epoch to run out of, pending run), `sdk83` (leaf combine),
`sdk84` (leaf renewal), `sdk50` (unilateral ladder exit), `sdk45` (keyless tower), `sdk40`
(consensus: each tier rejected before its CSV, accepted after), `sdk54`/`sdk55` (adversarial
bundles), `sdk90`/`sdk91` (the transfer window's local and server-side gates), `sdk29`/`sdk31`/
`sdk34`/`sdk39` (tokens: granularity, combine, watchtower materialisation, depth-2 coloured exit),
`rgb16` (why the legacy lane's carriers are uncolourable). Units: `select`, `split_math_tests`,
`granularity_model`, `invalidation_model`.
