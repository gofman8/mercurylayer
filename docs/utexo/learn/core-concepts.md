# Core concepts

> The conceptual tour. The normative descriptions are [`../spec/PROTOCOL.md`](../spec/PROTOCOL.md)
> (the TES-R protocol), [`../spec/CHILDREN.md`](../spec/CHILDREN.md) (first-class split children),
> [`../spec/LIGHTNING.md`](../spec/LIGHTNING.md) (the HODL latch),
> [`../spec/TRUST-MODEL.md`](../spec/TRUST-MODEL.md) (who trusts whom) and
> [`../spec/SPEC.md`](../spec/SPEC.md) (REQ / INV / ERR). Where this page and a spec document
> disagree, the spec wins.

## The statechain entity (SE)

The SE is the co-signing service: an API server plus a database (the **coordinator**) and a lockbox
enclave holding **one half of a 2-of-2 key for every coin** (blind MuSig2). It:

- **co-signs blindly** — it receives a session commitment, never the transaction: no amounts, no
  outpoints, no destinations, no colours;
- **rotates its key share on transfer**, so previous owners lose the ability to co-sign;
- **enforces per-coin gates** at `/sign/first` and `/sign/second` (`server/src/endpoints/sign.rs`):
  a Schnorr signature by the coin's own auth key (else 401), the per-coin `single_use` rule, the
  `epoch_deadline` after which it stops co-signing, the monotonic **spend budget** (which may only
  tighten — `set_sig_budget`, `server/src/database/deposit.rs`), and a **pending-transfer lock**
  that denies the sender any co-signature while a transfer of that coin is open;
- **publishes counters** — the lockbox's lifetime `sig_count`, served as `num_sigs` through
  `GET /info/statechain/<id>`, plus the per-node budget/terminality receipt at
  `GET /statechain/spend_budget/<id>`. That count is what lets a receiver prove no hidden state
  exists ([verification at claim](#verification-at-claim)).

The SE **cannot move funds** (it holds one of two keys) and **cannot freeze you out** (you hold a
complete pre-signed exit chain). This is the role Spark's operator set plays, collapsed to one
entity: the trust assumption becomes "the SE deletes the previous owner's share" rather than "1 of n
operators is honest". Either way, unilateral exit never needs the operator's cooperation.

Two properties an implementer must not design around:

- **The SE has no trustworthy chain access.** It runs in an operator-controlled container on an
  operator-controlled network, so "the SE checked the chain" reduces to "the operator says so."
- **The lockbox and the coordinator are run by the same operator.** Their separation is a software
  boundary inside one administrative domain, not two parties. There is also no enclave-residency
  attestation a client checks — what the enclave key *does* sign is the numbers the census rests on
  (`utexo/sig_count/v2`), and the client verifies that signature against a **pinned attestation
  identity**, never against a key served in the same response. `TesrParams::attestation_identity_const`
  (`lib/src/tesr.rs`) carries no pin for any network yet, so the identity must be configured
  (`SdkConfig::attestation_identity`); with neither, the client **refuses** rather than degrading.

## A coin is a timelock ladder (TES-R)

A **coin** is a statechain: an on-chain funding UTXO `F` whose key is `owner + SE`, plus a
pre-signed, **un-broadcast** three-tier chain hanging off it — the **TES-R ladder** (Trigger /
Extension / State, with rollover). `claim()` establishes one for every fresh confirmed **root** coin,
unconditionally.

```
F   on-chain funding UTXO, key = owner + SE       ← the only thing resting on-chain
└─ T    TRIGGER     no timelock, signed ONCE at deposit, never re-signed
   └─ X_m EXTENSION  relative CSV E_m  — renewal replaces it horizontally (E_{m+1} = E_m − δE)
      └─ S_k STATE   relative CSV Δ_k  — each transfer decrements (Δ_{k+1} = Δ_k − δ), pays owner k
```

All three tiers are v3/TRUC transactions with a 240-sat P2A anchor (`P2A_VALUE`). Each bakes in a
small committed fee at `TesrParams::committed_fee_rate` = **3 sat/vB**, so it relays and confirms on
its own; the anchor lets a party holding a funding input attach a live-rate fee child during a spike.
A signed one-payload tier is **125 vB** (`TIER_VBYTES`, measured through the production finaliser —
TES-R hashes with `TapSighashType::All`, so the witness carries the explicit sighash byte); its
coloured sibling is **168 vB** (`COLORED_TIER_VBYTES`, exactly one P2TR output wider).

**The one property everything else follows from**: the tiers use *relative* (BIP-68/112 CSV)
timelocks, which only start counting once the **parent confirms** — and `T` has no timelock at all.
So **nothing matures until someone broadcasts `T` on-chain**. An idle ladder never ages, there is
nothing to renew on a calendar, and idle rent is **0 vB**. When trouble starts it is loud: an
adversary must publish `T` first, and no hostile transaction is valid for at least 144 blocks (~1
day) after that.

Mainnet parameters, `TesrParams::mainnet()` (`lib/src/tesr.rs`), compiled in per network rather than
served by the SE: state `D0` = 1,440 with δ = 36 (≈6 h of head start per hop), floor 144; extension
`E0` = 720 with δE = 36, floor 144, forced rollover at `m_max` = 15. The unilateral wait on a fresh
depth-0 laddered coin is `E0 + D0` = 2,160 blocks ≈ **15 days** (`T` waits nothing), shrinking 36
blocks per hop; each further split level adds `E0` = 720 more (`tesr_exit_wait_blocks`,
`clients/libs/rust-sdk/src/config.rs`), since a spine tier waits nothing either. A regtest
preset (24/6/6, 12/3/3, `m_max` 2) exists so a full lifecycle fits a test's mining budget; testnet
and signet deliberately run the **mainnet** schedule. `sdk44` pins the arithmetic.

Because the schedule is the receiver's own, a hostile coordinator cannot widen or narrow anyone's
race window: `cap_schedule` (`clients/libs/rust/src/tesr.rs`) measures every conveyed `TesrParams`
field by field against the receiver's network preset and refuses by name on the first disagreement.

### What the ladder does *not* delete

The ladder is not the coin's only pre-signed material. The **flat backup chain** over `F` is
retained, carrying absolute locktimes `L_k = L_0 − k·interval` — one decrement per whole-coin hop.
A coin that has been *received* therefore sits on `min(L_k)`, a real approaching calendar height held
by its prior owners, inside a root epoch of `initlock` = **10,000 blocks ≈ 69.4 days** on mainnet
(`TesrParams::flat_ladder_params_const`; regtest's 1,000 blocks is the test clock and must not be
quoted as the deployed one). `SDK_E2E=86` measures the tip advancing toward that height and each hop
spending `interval` of it.

So: **laddering removes the CSV-side ageing, and nothing else.** `deadline_safety_due`
(`clients/libs/rust-sdk/src/refresh.rs`) is the scheduled defence of `min(L_k)` on a laddered coin,
and broadcasting `T` kills every flat backup permanently — the two families of material are
alternatives, not layers.

### The ladder lives off-chain, indefinitely

- **A transfer** co-signs a fresh state one δ **lower** than the one it replaces
  (Decker–Wattenhofer *replace-by-lower-timelock*), so the new owner's state matures **first**. The
  replaced state is disclosed to the receiver as superseded — and counted.
- **Renewal**: when the next state CSV would fall below the floor, the SDK co-signs a fresh extension
  `X_{m+1}` at a lower CSV inside the transfer (`renew` — two blind co-signs over the ordinary
  `/sign/first` + `/sign/second` pair). It strictly undercuts every older extension in the race for
  `T.out[0]`, so every pre-renewal state hangs on an extension that can now never confirm. Zero
  on-chain bytes. There is no SE-side renewal counter machine and none is planned: `m` and `k` are
  fields of the client's own bundle.
- **Rollover**: at epoch exhaustion a 1-in-1-out off-chain self-split mints a fresh level with a
  fresh hop budget. Also zero on-chain bytes, at the cost of one more depth level.
- Both are unbounded: `sdk43` drives renew → rollover → renew past exhaustion and then exits
  unilaterally through the whole deep chain, with `F` never touched.

`refresh` is the **re-anchor** primitive — one on-chain transaction (~112 vB) that moves the coin to
a fresh funding outpoint and mints a new ladder (`sdk30`). It caps exit depth, restores a coin after
a hostile trigger, and buys a fresh root epoch. `refresh_sponsored` lets an operator pay the rebate,
sized as `max(fee + DUST_LIMIT, min_child_value)` so it clears the split admission floor.

### Defending, and exiting

If someone broadcasts `T` — a past owner racing a stale state, or a pure griefer — the coin is not in
danger, it is on notice:

- **Cooperative de-trigger** (the normal response): owner + SE key-path-spend `T.out[0]` immediately
  into an address the owner names. That spend carries no timelock, so it confirms unopposed inside
  the ≥144-block window during which no adversary transaction is even valid. It is a *tier*, anchor
  and all — **125 vB**, not a bare 111-vB co-op spend. `detrigger_to_owner`
  (`clients/libs/rust-sdk/src/refresh.rs`), driven end to end by `sdk89`: the griefer's `T` confirms,
  the owner answers, and the pre-signed extension `X_0` is then submitted to the node and refused,
  because the output it spends is gone. **The restoration half is not built** — there is no fresh `F′`
  and no rebuilt `T′/X′_0/S′_0`, so getting back off-chain after a de-trigger is a fresh deposit.
- Griefing is **survivable and bounded, but not costly to the attacker**: both transactions pay out
  of the coin's own committed fees, so at or below the committed rate the griefer pays nothing and
  the coin loses two rungs' worth of fee and anchor.
- **Ladder defence** (SE unreachable): the owner — or a keyless watchtower holding the watch bundle —
  broadcasts the current extension at +E and the current state at +Δ. The current state is strictly
  the lowest-CSV one, so it matures first and the funds land at the owner's key (`defend_ladders`,
  `sdk51`).
- **Unilateral exit** is a **walk**, not a single broadcast: `T`, then `X` once its relative timelock
  has run, then `S` once its own has (`unilateral_exit`, `sdk50`; `sdk45` performs the same walk from
  a bundle containing **no key material** at all, with a second independent tower proven idempotent).

**What a keyless tower cannot do, normatively:** fee-bump. A CPFP child spending the P2A anchor needs
a funding input the tower does not hold, so if the mempool floor rises above a tier's committed rate
the tower has no move — it says so rather than retrying. The party that funds a bump is the coin
**owner**, and `SdkConfig::fee_bump` ships as `None` on both presets, so bumping requires an
owner-supplied fee source. An operator may optionally run a *funded* tower; it still holds no coin
keys, and its capacity is the number of **confirmed** fee UTXOs it holds, not its balance (TRUC
allows one unconfirmed ancestor, so chained rescues are refused at any price).

## One protocol, two coin shapes

Not every coin is laddered, by design. Nothing selects the shape: it follows from what the coin
carries, and the transfer message's `protocol_version` reports it to the receiver.

| | **Laddered** | **Un-laddered** |
|---|---|---|
| Which coins | every plain BTC deposit | **RGB carriers**; split sub-coins whose funding is un-broadcast |
| Exit material | `T → X_m → S_k`, relative CSV, un-broadcast | the signed-once backup, absolute nLockTime, plus the branch |
| A transfer | co-signs a state one δ lower | pre-signs the receiver's backup at `previous − interval` |
| Ageing | no CSV clock; the retained flat chain still carries `min(L_k)` | absolute locktimes; a received coin has a root deadline |
| Owner duty while idle | defend `min(L_k)` (`deadline_safety_due`) | materialize the branch before the deadline (`auto_exit_due`, default on) |

Why the second shape exists:

- **An RGB carrier is not given a plain ladder.** RGB transitions may anchor only in signed-once
  transactions, and a plain tier spend is sats-only — it would sweep the carrier and destroy the
  allocation. This is the *terminal-freeze* rule, and it is load-bearing for tokens. `sdk52` pins it:
  in one wallet the plain coin carries a ladder and the token carrier carries none, and an off-chain
  RGB transfer still settles.
  A **coloured** ladder — every tier carrying a valid RGB state transition — exists behind
  `SdkConfig::colored_ladder`, which **ships `false`** on both presets. So the shipped default for a
  carrier is the flat signed-once shape, with the calendar duties that come with it. `sdk74`
  (establish), `sdk75` (exit), `sdk77` (coloured in-ladder split) exercise the flag turned on;
  `colored_ladder_health` reports on a coloured carrier, and `LadderSkipReason` names why a given
  coin fell back.
- **A split sub-coin over un-broadcast funding cannot root a trigger** — a trigger needs a confirmed
  prevout, and a v3 tier cannot relay over an unconfirmed parent. This is checked against the chain
  fail-closed, never inferred.

A received carrier's deadline duty is handled by the SDK's watchtower, which materializes the
coloured branch before the clawback window opens (`materialise_carrier`; `sdk34`, with `sdk32`
documenting the residual window if nobody acts).

## Splits, the spine, and children

Payments are arbitrary amounts, and an arbitrary amount equals a coin you already hold only by
coincidence — so essentially every payment is a **split**, and the payee receives a **leaf**.

On a laddered coin a split is an **in-ladder split**: a state tier `SP` spending `X_m.out[0]` — a
**descendant of the trigger**, never a rival for the funding outpoint `F`. That is the whole security
argument: a past owner's retained no-timelock trigger has nothing to race, because the split does not
compete for `F`. `SP` is signed at `SPINE_CSV = 0` (`clients/libs/rust/src/tesr.rs`) and carries K+1
payload outputs plus the anchor.

```
F (on-chain root)
└─ T ── X_m
         └─ SP        state tier, un-broadcast, nSequence 0, Σout = Σin − fee
              ├─ out[0..K−1]  piece children  → each paid to a payee, each with its own ext + state
              └─ out[K]       the SPINE TIP   → the sender's change, ONE cap tier, no extension
```

- **Width is free**: carving K pieces is one off-chain transaction (`in_ladder_pay_many` drives the
  N-ary builder). Depth advances per *batch*, not per payment.
- **The change leg is a tip, not a child.** `establish_spine_tip_journalled` hangs ONE state tier at
  `state_csv(0)` directly over `SP.out[K]`. The extension exists to reset the state budget by
  renewal, and on the spine every payment already lands the change on a virgin outpoint at a virgin
  `D0`, so the rung is dead weight. `change_leg_role` (`clients/libs/rust/src/tesr.rs`) is the single
  per-lane authority for this, so the floor a payment is admitted at and the ladder the builder then
  constructs can never be two different shapes. It reports `SplitLegRole::SpineTip` on
  `SplitLane::PlainRoot`, `SpineBatch` and `Colored`, and `Piece` on `SplitLane::PlainChild` — a
  *child* being split still gives its change leg an ordinary two-tier piece.
- **The next payment is a spine batch.** A tip is not a coin other builders can load; `spine_batch_split`
  builds the next `SP` over the tip's own outpoint, retires the previous cap into `superseded_states`,
  terminalizes the tip's slot and leaves another one-cap tip. So a payment adds exactly **one**
  transaction to the sender's exit chain — the bound this architecture attains.
- **There is an admission floor, and it is a rate evaluation.** At the shipped 3 sat/vB a rung costs
  `committed_fee + P2A` = 615 sat, so `min_child_value` = 2·615 + 330 = **1,560 sat** and
  `min_spine_tip_value` = 615 + 330 = **945 sat** (`lib/src/tesr.rs`; `DUST_LIMIT` = 330). Both are
  checked *before* the parent is terminalized, so a too-small piece is refused cleanly instead of
  stranding the parent to unilateral-exit-only. `sdk58` (12 tamperings of the authoritative inputs,
  each rejected for the *named* reason it targets, so a rejection for any other cause fails the
  test), `sdk59` (the end-to-end split payment), `sdk81` (recovery of an interrupted split).
- **Depth costs on exit, and the cap is derived.** A unilateral exit walks `3 + 2d` transactions and
  waits out each tier's CSV, so `max_split_depth` / `max_exit_txs`
  (`lib/src/transfer/receiver.rs`) derive the ceiling from the receiver's own schedule and the SE's
  funding epoch — **depth 8 / 19 transactions** on mainnet, 54 / 111 on regtest — enforced by
  `enforce_split_depth_cap_shaped` and `enforce_exit_chain_length`. A spine level costs the walk one
  tier and a two-tier level two, so levels are charged by shape.

### Received children are first-class

A received piece is a real coin, not an exit-only claim. The claim completes the standard SE **key
handover**: the child aggregate `A_child` is *invariant* across the rotation
(`sender_share + SE_old == receiver_share + SE_new`), which is exactly what keeps the pre-signed
child exit chain valid, while the sender is **permanently locked out**. The receiver can then pay the
child onward off-chain — **whole** (`child_retransfer`, which spends zero sats and adds zero depth)
or **split again** (`child_in_ladder_pay` / `child_in_ladder_pay_many`). Each hop costs exactly one
co-signature and discloses exactly one superseded state, which the receiver's census counts and
proves out-raced. `sdk60` (alice → bob → carol, `F` unspent throughout), `sdk17` (a partial second
hop), `sdk76` (a received parent splitting), `sdk84` (child renewal).

**The rule is uniform at every level: the node being split is terminalized; the piece being conveyed
is not.** A conveyed child's safety is two-layer — the census closes any *pre*-conveyance rival, and
the coordinator's pending-transfer lock closes any *post*-conveyance rival until the handover makes
the lockout permanent. That lock's non-batch branch is a hard-coded one-hour wall clock
(`OPEN_TRANSFER_WINDOW_SQL`, `server/src/database/transfer_sender.rs`); see
[transfers](#transfers) below.

### Combines

A **combine** goes the other way: one SE-co-signed transaction `CB` spends N sub-coins into fewer (or
one) outputs, carrying a per-input relative timelock (BIP-112 is per-input). The output's ancestry
becomes the *union* of all N inputs' ancestries plus the combine tx, so the structure becomes a DAG
at that node — but it is still a tree over *outpoints* (only disjoint input ancestries are combined;
a shared ancestor is rejected). This is why the receiver requires **Σ-inputs terminal ancestors**:
one terminal per structural input, not one per branch. A combine spends `SP.out[j]` directly and
never broadcasts the leaf's own tiers, so it is also how a small leaf realises far more of its face
value than a walk does.

The **coloured** combine is wired into the token lane and runs on an ordinary `transfer_tokens`
(`colored_combine_transfer`, `clients/libs/rust-sdk/src/tokens.rs`; `sdk31`). The **sats-leaf**
driver `combine_leaves` (`clients/libs/rust/src/combine.rs`) is proven end to end against a live SE
and a live chain by `sdk83` — N leaves into one UTXO, with blind and mempool attempts both refused —
but it has no caller outside that test, so no wallet method reaches it yet. That is why
[`../spec/PARTIAL-PAYMENT-ECONOMICS.md`](../spec/PARTIAL-PAYMENT-ECONOMICS.md)'s swept row is not a
number that ships.

## Transfers

A transfer is a **key handover** — no block, no fee, sub-second, fully async:

1. the sender co-signs the receiver's new state (laddered) or pre-signs the receiver's backup
   (un-laddered), and posts an encrypted transfer message through the SE's relay;
2. the receiver validates everything, then calls the SE, which **rotates its key share** — from that
   moment only receiver + SE can sign, and the sender's share is dead.

While a transfer is open, the SE holds a **pending-transfer lock** on that coin and refuses the
sender any further co-signature. Two ordering rules make that safe: all of the sender's own pre-signs
happen *before* `get_new_x1` opens the transfer, and any lane that co-signs a superseding state moves
the coin out of `CONFIRMED` **durably** before the co-sign, so a watchtower is never armed to
broadcast a state the recipient's chain supersedes (`sdk80`).

**The lock is temporary, and its size is measured.** Its non-batch branch — every ordinary payment —
is a hard-coded `updated_at > NOW() - INTERVAL '1 hour'`. `sdk91` puts a payer in front of it who
skips their own client and POSTs `/sign/first` to the coordinator with their own genuine credential:
**HTTP 409** while the window is open, **HTTP 200** with a `server_pubnonce` once the row is older
than an hour. So on the server side, that clock is the only gate on that path. `sdk90` measures the
two *local* gates that stop an honest client first — the wallet's own coin lookup and
`refuse_outstanding_conveyance` — and reaches no conclusion about the server, because a payer who
wants to cheat does not run their own client. A `sign/first` session is the first link of a theft,
not a completed one: `sign/second` and a broadcast race against the payee's strictly-lower-CSV state
still stand in the way, and those links are untested in either direction. The owner latch specified
to replace the wall clock is **design, not built**.

### Verification at claim

The receiver trusts nothing and re-derives everything from public data — any deviation is a reject
(`verify_bundle` / `verify_bundle_bound` / `verify_child_bundle`, `clients/libs/rust/src/tesr.rs`):

- **`F` is on-chain, unspent** and pays the expected aggregate key — read from the chain, not from
  the bundle, and cross-checked against the coordinator's recorded aggregate;
- the conveyed structure is consensus-valid back to that root: `T` spends `F` with no timelock, tier
  outputs pay the correct publicly-tweaked keys, each tier's **signed** nSequence lies inside the
  band its kind allows (`[e_floor, e0]` for an extension, `[d_floor, d0]` for a state, exactly
  `SPINE_CSV = 0` for a split tier) and its **declared** CSV is bound to that signed number
  (`bind_declared_csv`), so a schedule that contradicts the signatures is refused rather than
  believed;
- the **census** — exact equality `se_num_sigs == flat_backups + tiers + superseded`. A hidden,
  undisclosed rival state shows up as a count mismatch. This is the linchpin, and it only proves
  anything if the count is the *enclave's*: `get_statechain_info` sends a fresh random nonce and
  refuses any answer not carrying a `utexo/sig_count/v2` attestation over
  (`statechain_id`, `num_sigs`, budget, nonce), verified against the pinned identity. The census
  generalizes to N hops for children, per segment, with `CHILD_V2_BASELINE = 0` because a derived
  child slot never created a flat backup. `sdk46`, `sdk47`, `sdk54`, `sdk55`, `sdk58`, `sdk60`,
  `sdk70`;
- **terminality** is likewise derived from the enclave-signed payload (`attested_terminal`) for the
  parent and every intermediate segment, keeping the coordinator's answer only as a cross-check that
  refuses on disagreement;
- for sub-coins, per-level branch validation plus Σ-inputs terminal ancestors;
- for tokens, the RGB consignment is client-validated, with un-broadcast witness transactions
  allowed.

## Tokens

Tokens are **RGB assets**: client-validated contracts whose allocations live on coins and sub-coins.
The server knows nothing about tokens — validation is done by the receiving wallet against
cryptographic consignments. Two rules matter at concept level:

- RGB transitions anchor **only in signed-once transactions** — coloured splits and combines, and
  coloured self-transitions. Plain ladder tiers are sats-only and would destroy an allocation, which
  is why the shipped default gives a carrier no ladder at all.
- **Terminal-freeze**: a coloured transaction only ever spends outputs of *terminalized* structure,
  so no ancestor of an RGB anchor is ever re-signed and no superseded coloured witness exists
  anywhere. The corollary is a lane rule enforced by `verify_flat_backup_lane`: on a coloured bundle
  every conveyed flat backup must be **plain**, because a retained *coloured* backup would let a
  prior owner spend `F` and re-assign the whole allocation to themselves.

Token pieces are sized from the coloured floors rather than chosen: `TOKEN_PIECE_SATS` = **4,074**
(`clients/libs/rust-sdk/src/tokens.rs`) is the coloured root-ladder floor computed at twice the
committed rate, so a piece still clears its floor if that rate ever doubles. See [tokens](tokens.md).

## Lightning

Lightning works **both directions on the ladder**, through a **HODL-invoice latch**: the SE's
co-signature — including any renewal bundled into the same transfer — is gated on the payment
preimage, so the off-chain state moves if and only if the Lightning payment settles. PTLCs do not
exist in the routing network, so no adaptor-signature construction is available; the latch uses HODL
plus a BOLT11 preimage only.

| direction | amount | entry point | evidence |
|---|---|---|---|
| pay (coin → LN) | exact | `pay_lightning_invoice` | `sdk63` |
| pay | non-exact | `pay_lightning_invoice_inladder` | `sdk65` |
| receive (LN → coin) | exact | `create_receive` | `sdk64` |
| receive | non-exact | `create_receive` → in-ladder split | `sdk67` |
| rollback | non-exact / exact | booking rolled back / `reclaim_lightning_payment` | `sdk66` / `sdk68` |

The one-call pay API cannot mint an exact laddered coin, so it falls back to the non-exact in-ladder
lane — the same way the receive side does. The **latched piece is the one case that stays
terminalized**: it sits unclaimed past the pending-transfer lock's window (the payment provider
settles on its own schedule), so a permanent lockout replaces the temporary one. Every other
in-ladder child relies on the census plus the key handover instead. See [lightning](lightning.md).

## Exits

- **Cooperative (normal, 1 tx)**: `withdraw` — the SE co-signs a fresh direct spend of the coin to
  your L1 address, no timelock, no waiting. One exception: a *received in-ladder child* has no
  confirmed outpoint to spend (its funding `SP.out[j]` is un-broadcast), so it routes to the
  unilateral walk instead, whose final state already pays your own key.
- **Unilateral (SE gone)**: never needs anyone's cooperation. On a **laddered** coin you walk the
  pre-signed chain tier by tier, waiting out each relative timelock — `T`, then `X`, then `S`, plus
  the tiers of each split level. On an **un-laddered** coin you broadcast the branch and then the
  signed-once backup once its absolute locktime passes; your backup unlocks earlier than every
  previous owner's, so you win the race.
- **Tokens** exit by **materializing** the coloured branch — both plain paths refuse a carrier, since
  an RGB-unaware sweep would destroy the allocation.

`estimate_exit_cost` prices a specific coin's walk before you commit to it.

See [deposits & exits](exits.md), the cost model in
[`../spec/PARTIAL-PAYMENT-ECONOMICS.md`](../spec/PARTIAL-PAYMENT-ECONOMICS.md), and the
party-by-party matrix in [`../spec/TRUST-MODEL.md`](../spec/TRUST-MODEL.md).
