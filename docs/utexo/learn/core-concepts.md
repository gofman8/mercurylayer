# Core concepts

> The conceptual tour. The normative descriptions are [`../spec/PROTOCOL.md`](../spec/PROTOCOL.md)
> (the TES-R protocol), [`../spec/CHILDREN.md`](../spec/CHILDREN.md) (first-class split children),
> [`../spec/LIGHTNING.md`](../spec/LIGHTNING.md) (the HODL latch),
> [`../spec/TRUST-MODEL.md`](../spec/TRUST-MODEL.md) (who trusts whom) and
> [`../spec/SPEC.md`](../spec/SPEC.md) (REQ / INV / ERR). Where this page and a spec document
> disagree, the spec wins.
>
> The `sdkNN` / `rgbNN` citations name E2E flows against the regtest stack. The unit suites are green
> and the E2E crate compiles, but **no E2E flow has been run since the flat backup chain was removed
> on 2026-09-06** — they were re-derived, not re-measured. Read them as pending run.

## The statechain entity (SE)

The SE is the co-signing service: an API server plus a database (the **coordinator**) and a lockbox
enclave holding **one half of a 2-of-2 key for every coin** (blind MuSig2). It:

- **co-signs blindly** — it receives a session commitment, never the transaction: no amounts, no
  outpoints, no destinations, no colours, and no confirmation state — it co-signs the ladder over a
  funding transaction that is still in the mempool exactly as it co-signs anything else;
- **rotates its key share on transfer**, so previous owners lose the ability to co-sign;
- **enforces per-coin gates** at `/sign/first` and `/sign/second` (`server/src/endpoints/sign.rs`):
  a Schnorr signature by the coin's own auth key (else 401), the per-coin `single_use` rule, the
  optional `epoch_deadline` after which it stops co-signing, the monotonic **spend budget** (which
  may only tighten — `set_sig_budget`, `server/src/database/deposit.rs`), and a **pending-transfer
  lock** that denies the sender any co-signature while a transfer of that coin is open;
- **publishes counters** — the lockbox's lifetime `sig_count`, served as `num_sigs` through
  `GET /info/statechain/<id>`, plus the per-node budget/terminality receipt at
  `GET /statechain/spend_budget/<id>`. That count is what lets a receiver prove no hidden state
  exists ([verification at claim](#verification-at-claim)).

The SE **cannot move funds** (it holds one of two keys) and **cannot freeze out a laddered coin**
(you hold a complete pre-signed exit chain). This is the role Spark's operator set plays, collapsed
to one entity: the trust assumption becomes "the SE deletes the previous owner's share" rather than
"1 of n operators is honest". Either way, unilateral exit never needs the operator's cooperation —
*provided the coin has a ladder*. Since 2026-09-06 that is the only exit material there is, so a coin
whose ladder could not be established has no unilateral exit and can leave only through the
cooperative withdrawal, which does need the SE. See [what the ladder does not leave
behind](#what-the-ladder-does-not-leave-behind) and the attestation bullet below.

Two properties an implementer must not design around:

- **The SE has no trustworthy chain access.** It runs in an operator-controlled container on an
  operator-controlled network, so "the SE checked the chain" reduces to "the operator says so."
- **The lockbox and the coordinator are run by the same operator.** Their separation is a software
  boundary inside one administrative domain, not two parties. There is also no enclave-residency
  attestation a client checks — what the enclave key *does* sign is the numbers the census rests on
  (`utexo/sig_count/v2`), and the client verifies that signature against a **pinned attestation
  identity**, never against a key served in the same response. `TesrParams::attestation_identity_const`
  (`lib/src/tesr.rs`) pins **regtest's** — a pure function of the dev seed this repo commits, so it is
  a fact about the source and not about a running server — and returns `None` for mainnet and for
  every public testnet (`testnet`, `testnet3`, `testnet4`, `signet`), where no enclave is provisioned.
  On those networks the identity must be configured (`SdkConfig::attestation_identity`); with
  neither, the client **refuses** rather than degrading. That refusal now has teeth on the deposit
  path too: the SDK `claim()` establish pass calls `get_statechain_info` for every root coin, so on an
  unpinned, unconfigured network it records `LadderSkipReason::AttestationIdentityUnpinned` and
  ladders nothing at all — plain coins included — leaving every deposit booked but with **no exit
  material**, cooperatively withdrawable and nothing else. (The mercuryrustlib `update_coins` lane,
  `LadderAtSight::Plain`, does not make that call and ladders without a pin.) No mainnet enclave is
  provisioned, so this is a not-yet-deployable state rather than a live regression.

## A coin is a timelock ladder (TES-R)

A **coin** is a statechain: an on-chain funding UTXO `F` whose key is `owner + SE`, plus a
pre-signed, **un-broadcast** three-tier chain hanging off it — the **TES-R ladder** (Trigger /
Extension / State, with rollover). The ladder is the coin's **only** exit material, and it is
established at the **first mempool sighting** of the funding transaction, before it confirms: by
`coin_status::check_deposit` itself under `LadderAtSight::Plain`, or — for an SDK wallet, whose
watcher runs under `LadderAtSight::Defer` — by `claim()`'s establish pass in the same tick, plain or
coloured.

The two lanes fail differently, and the difference matters. Under `LadderAtSight::Plain` a deposit
whose ladder cannot be established is **not booked**: `check_deposit` rolls the coin back to
`INITIALISED` and errors, and the next pass retries. Under `LadderAtSight::Defer` the deposit **is**
booked (`IN_MEMPOOL`) and the ladder is a separate step, so a coin can end a pass booked with no exit
material and a recorded `LadderSkipReason`. It is then neither conveyable nor unilaterally exitable —
only cooperatively withdrawable — until a later `claim()` ladders it.

```
F   on-chain funding UTXO, key = owner + SE       ← the only thing resting on-chain
└─ T    TRIGGER     no timelock, signed ONCE at first sight of F, never re-signed
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

### What the ladder does *not* leave behind

The ladder is the coin's only pre-signed material. There is **no flat backup chain** over `F`: no
absolute-locktime backup is co-signed at deposit (`create_tx1` is deleted) and none at any hop, so
there are no locktimes `L_k`, no `min(L_k)` held by prior owners, no root epoch, and `coin.locktime`
is `None` for the coin's whole life. INV-5 (the decrementing backup chain) is RETIRED 2026-09-06;
INV-27 ("idle coins never age") is unconditional. `initlock` and `interval` survive in
`/info/config` and `TesrParams::flat_ladder_params_const` only as **compatibility constants**:
`initlock` is the fixed exit window the split-depth cap measures a leaf's exit walk against, and
`interval` is applied to nothing.

So: **laddering removes ageing, full stop.** The only spends of `F` in a past owner's hands are
their retained copies of the same `T` — no timelock, so the current owner or their watcher can
always pre-empt them by broadcasting it first — and the superseded states below it, which lose the
CSV race. Nothing a past owner holds ever matures on its own.

**Say what that means for the person holding the coin:** a coin can no longer be taken from its owner
on a fixed date by someone who used to own it. There is no height to diary, no deadline your wallet
must be online to survive, and no maintenance you owe just to keep what you have — being offline for a
year costs you nothing. The one obligation left is *reactive*, and it starts only if somebody
broadcasts the trigger; a keyless third-party tower can carry it for you. What this cost is on the
other side is stated under the attestation pin below: a coin that could not be laddered has no
unilateral exit at all. `deadline_safety_due`
(`clients/libs/rust-sdk/src/refresh.rs`) still runs unconditionally every tick, and on a laddered
wallet it has **no subject**: both of its remedies are applied to coins near a `locktime`, and no
coin has one.

### The ladder lives off-chain, indefinitely

- **A transfer** co-signs a fresh state one δ **lower** than the one it replaces
  (Decker–Wattenhofer *replace-by-lower-timelock*), so the new owner's state matures **first**. The
  replaced state is disclosed to the receiver as superseded — and counted. The message conveys
  **no** `backup_transactions`; the receiver requires that vector to be empty.
- **Renewal**: when the next state CSV would fall below the floor, two blind co-signs mint a fresh
  extension `X_{m+1}` at a lower CSV (`renew` / `renew_auto` — over the ordinary `/sign/first` +
  `/sign/second` pair). It strictly undercuts every older extension in the race for `T.out[0]`, so
  every pre-renewal state hangs on an extension that can now never confirm. Zero on-chain bytes.
  There is no SE-side renewal counter machine and none is planned: `m` and `k` are fields of the
  client's own bundle.
- **Rollover**: at epoch exhaustion a 1-in-1-out off-chain self-split mints a fresh level with a
  fresh hop budget. Also zero on-chain bytes, at the cost of one more depth level.
- Both are unbounded: `sdk43` drives renew → rollover → renew past exhaustion and then exits
  unilaterally through the whole deep chain, with `F` never touched.
- Both are **library calls not yet invoked on the transfer path** (`mercuryrustlib::tesr::renew_auto`
  / `rollover_auto`, `renew_child` for a leaf, `renew_colored_ladder` on the SDK for a coloured
  root). Renewal is by hand today; a transfer that reaches the floor is refused with the remedy
  named. A coin's off-chain life is bounded by renewals and rollover only, and the on-chain cadence
  is the cooperative re-anchor at that cap.

`refresh` is the **re-anchor** primitive — one on-chain transaction (~112 vB) that moves the coin to
a fresh funding outpoint, where a new ladder is established at first sight through the same deposit
path (`sdk30`). It caps exit depth and kills every retained trigger copy and superseded state rooted
at the old outpoint; it resets no calendar, because there is none. `refresh_sponsored` lets an
operator pay the rebate, sized as `max(fee + DUST_LIMIT, min_child_value)` so it clears the split
admission floor. The coloured re-anchor (`colored_reanchor`) is a manual call.

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
  `sdk51`). The pass runs over every **live** coin — `is_live_for_defence`: `IN_MEMPOOL`,
  `UNCONFIRMED` or `CONFIRMED` — so a ladder is defended from the block its deposit is first seen in,
  and it is event-driven: nothing on a coin is due on a height.
- **Unilateral exit** is a **walk**, not a single broadcast: `T`, then `X` once its relative timelock
  has run, then `S` once its own has (`unilateral_exit`, `sdk50`; `sdk45` performs the same walk from
  a bundle containing **no key material** at all, with a second independent tower proven idempotent).
  It keys on the same liveness rule, and a coin with no ladder row is refused by name — there is no
  flat exit fallback, because there is no flat backup.

**What a keyless tower cannot do, normatively:** fee-bump. A CPFP child spending the P2A anchor needs
a funding input the tower does not hold, so if the mempool floor rises above a tier's committed rate
the tower has no move — it says so rather than retrying. The party that funds a bump is the coin
**owner**, and `SdkConfig::fee_bump` ships as `None` on both presets, so fee bumping ships with no
fee source. An operator may optionally run a *funded* tower; it still holds no coin keys, and its
capacity is the number of **confirmed** fee UTXOs it holds, not its balance (TRUC allows one
unconfirmed ancestor, so chained rescues are refused at any price).

## One protocol, one coin shape — three positions in a ladder

Every coin is laddered, and the ladder is the only exit material there is. What differs is *where* a
coin sits, and — for a token carrier — what colour its rungs are. Nothing selects any of this: it
follows from what the coin carries, and the transfer message's `protocol_version` reports it to the
receiver as one of exactly two shapes — `2`, a root-ladder conveyance, or `4`, a child conveyance
with key handover (`ADMISSIBLE_PROTOCOL_VERSIONS`). The un-laddered shape `0` no longer exists and
cannot be received.

| | **Root** | **Split child** | **Spine tip** |
|---|---|---|---|
| Which coins | every deposit, from first sight of `F` — plain, or coloured if it is a carrier | the payee's leg of an in-ladder split | the sender's change leg of one |
| Funding | `F`, **on chain** (or in the mempool) | `SP.out[j]`, **un-broadcast** | `SP.out[K]`, **un-broadcast** |
| Exit material | `T → X_m → S_k`, relative CSV, un-broadcast | the whole chain back to the parent's `F`: `T → X_m → SP → ext_child → state_child` | the same, capped by ONE tier over `SP.out[K]` |
| A payment out of it | `in_ladder_pay` — a split state `SP` over `X_m.out[0]` | `child_in_ladder_pay` — the same at the child's own level | `spine_batch_pay` — the next batch `SP_{i+1}` over `SP_i.out[K]`, retiring the previous cap |
| Ageing | none — no clock of any kind (INV-27, unconditional) | none — its exposure is the parent's trigger being broadcast, an event, not a height | same as a child |

The "split child" column is the *fully laddered* band of a payee's leg, and there are three thinner
ones below it, chosen from the leg's value alone (`LeafShape::for_value`, `SplitLegRole`): a
**thin piece** with one rung instead of two, a **stub** whose `SP.out[j]` simply pays the payee's own
key (no rung, no SE slot, no statechain id), and a **tail** — sub-dust, coin-backed, no rung, at most
one per split. A leg in the lower two bands cannot put itself on chain alone; it settles when its
group does. Each has its own conveyance verifier, and each refuses a conveyed flat backup by name
(`refuse_conveyed_flat_backups`, on the child, tail, stub and spine-tip lanes alike).

`parent_shape` (`clients/libs/rust-sdk/src/transfer.rs`) is the one resolution of that question, and
it is a **refusing** function: a coin carrying none of the three is a coin to repair, not a shape to
route. `parent_shape_opt` is the probe form, for the one caller — `has_exit_material` — where absence
is data rather than a fault: such a coin is excluded from every payment plan and reported in
`TransferQuote::no_exit_material_coins`.

Two things about that table are worth spelling out, because each used to be a separate "shape":

- **A carrier is laddered like any other coin — its rungs are just coloured.** RGB transitions may
  anchor only in signed-once transactions, and a *plain* tier spend is sats-only: it would sweep the
  carrier and destroy the allocation. That is the *terminal-freeze* rule, it is load-bearing for
  tokens, and it is why a carrier may never be given a plain ladder. The answer is a **coloured**
  ladder — every tier carrying its own valid RGB state transition, so the walk *moves* the allocation
  instead (`sdk74` establish, `sdk75` exit, `sdk77` coloured in-ladder split; `colored_ladder_health`
  reports on one). An issuance books its allocation at broadcast, so the carrier is colourable from
  its first mempool sighting. `SdkConfig::colored_ladder` selects it by **reading the enclave pin** —
  `TesrParams::attestation_identity_const(network).is_some()` — rather than stating a bool, because
  colouring a carrier buys nothing without an identity to verify its terminality against. Regtest is
  pinned and ships **on**; mainnet is off solely because **no mainnet enclave is provisioned yet**,
  and pinning a real identity flips it with no other change ([`../spec/SPEC.md`](../spec/SPEC.md)
  §0.4 rows V-1 and V-6). A carrier that cannot be coloured — below the coloured floor, or with its
  RGB state momentarily unreadable — has **no exit material** until a later pass colours it
  (`LadderSkipReason::RgbCarrier`), and it cannot be conveyed: there is no flat lane to fall back to.
  The *unpinned* case is not carrier-specific and is worse: with no attestation identity the SDK
  establish pass ladders no coin of any kind (`AttestationIdentityUnpinned`).
  A plain ladder found over a carrier is recorded `PlainLadderOverCarrier`, and that one has **no
  remedy**: `colored_reanchor` refuses a plain ladder by name and a plain `refresh` would destroy the
  allocation, so the coin exits as satoshis and the allocation is stranded. Avoid creating the state
  — do not move an allocation onto an outpoint that is already plain-laddered.
- **A split sub-coin's funding is un-broadcast, and colouring cannot change that.** `SP.out[j]` is an
  output of a transaction nobody has broadcast, so the sub-coin can never root a **trigger** of its
  own — a trigger needs a confirmed prevout, and a v3 tier cannot relay over an unconfirmed parent.
  This is checked against the chain fail-closed, never inferred. Far from a defect, it is the source
  of the whole property: an un-broadcast funding output costs 0 vB of rent and ages toward nothing.
  What it means practically is that a child's exit material reaches back through its parent, and that
  a coloured child's exit chain — every tier carrying the allocation — is what settles it on chain.
  (The `branch-` rows of the retired coloured split/combine lane, which used to play that role for a
  sub-coin, are no longer minted: `register_split_subcoins_n` and `register_combine_subcoins` refuse
  by name.)

A received coloured child has no clawback window to beat: no ancestor holds a matured spend of `F`,
so its only exposure is the parent's trigger being broadcast, and `defend_ladders`' per-block child
loop answers that event. There is no scheduled materialization for it and nothing to schedule
against.

**The shape that is gone is the un-laddered one.** `ParentShape::Unladdered`, `split_coin`, the
plain off-chain split and `ManyRoute::PlainSplit` are deleted; the flat conveyance lane and its
licence classifier (`transfer_sender::assert_flat_conveyance_is_legitimate`, `PermanentLicence`,
`clients/libs/rust/src/transfer_sender.rs`) refuse by name, and `is_legitimate_flat_reason` answers
`false` for every recorded reason. That deletion **closes [B1] by construction**: a plain split spent
the coin's funding output `F` directly, which is the same output a prior owner's retained,
un-timelocked trigger spends, so that owner could void the split and destroy the payee's sub-coin —
and the payee could not detect the exposure. There is no longer a route that spends `F` except `T`
itself, so there is nothing for a retained trigger to race. The in-ladder split, which carves out of
`X_m.out[0]` and is therefore a *descendant* of the trigger, is the only split there is.

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
- **There are admission floors, they are rate evaluations, and the two legs do not share one.** At
  the shipped 3 sat/vB a rung costs `committed_fee + P2A` = 615 sat, so `min_child_value` = 2·615 +
  330 = **1,560 sat** and `min_spine_tip_value` = 615 + 330 = **945 sat** (`lib/src/tesr.rs`;
  `DUST_LIMIT` = 330). The **change** leg is floored at the shape its lane actually builds — 945 on
  the plain-root, spine-batch and coloured lanes. The **piece** is admitted at **1 satoshi**:
  `split_output_floors` (`clients/libs/rust-sdk/src/transfer.rs`) reads
  `SplitLegRole::Tail.min_value`, because refusing at 1,560 would make every amount below it
  unpayable when a cheaper leg shape exists. What the payee's leg is *built* as is decided from its
  value by `LeafShape::for_value`, the same function the floor reads — ≥ 1,560 a two-rung `Piece`,
  945–1,559 a one-rung `ThinPiece`, 330–944 a `Stub` (`SP.out[j]` pays the payee's own key: no rung,
  no SE slot), below 330 a `Tail` (sub-dust, coin-backed, no rung, at most one per split). Only the
  two-rung and one-rung bands exit unaided; the other two ride out on their group's exit, which is
  the owner's accepted trade rather than a defect. How much of that range today's wallet API actually
  reaches is narrower than the builder's: the single-recipient `in_ladder_pay` lane passes no
  ladderless leg (a stub-band amount is refused there by name, before any co-sign), and
  `transfer_many` still floors every recipient at `min_split_output` — dust plus a 112-vB backup fee,
  a floor named after a transaction the protocol no longer builds. Every floor is checked *before*
  the parent is
  terminalized, so a too-small piece is refused cleanly instead of stranding the parent to
  unilateral-exit-only. `sdk58` (12 tamperings of the authoritative inputs, each rejected for the
  *named* reason it targets, so a rejection for any other cause fails the test), `sdk59` (the
  end-to-end split payment), `sdk81` (recovery of an interrupted split).
- **Depth costs on exit, and the cap is derived — against a fixed window, not a calendar.** A
  unilateral exit walks `3 + 2d` transactions and waits out each tier's CSV, so `max_split_depth` /
  `max_exit_txs` (`lib/src/transfer/receiver.rs`) derive the ceiling from the receiver's own
  schedule and one constant, `initlock` — the exit window the walk must fit inside, with
  `exit_slack_margin` added — giving **depth 8 / 19 transactions** on mainnet, 54 / 111 on regtest,
  enforced by `enforce_split_depth_cap_shaped` and `enforce_exit_chain_length`. A laddered coin has
  no epoch deadline, so there is no "remaining window" to read off a tip; what the cap bounds is the
  length and latency of the walk a leaf inherits. A spine level costs the walk one tier and a
  two-tier level two, so levels are charged by shape.

### Received children are first-class

A received piece is a real coin, not an exit-only claim. The claim completes the standard SE **key
handover**: the child aggregate `A_child` is *invariant* across the rotation
(`sender_share + SE_old == receiver_share + SE_new`), which is exactly what keeps the pre-signed
child exit chain valid, while the sender is **permanently locked out**. The receiver can then pay the
child onward off-chain — **whole** (`child_retransfer`, which spends zero sats and adds zero depth)
or **split again** (`child_in_ladder_pay` / `child_in_ladder_pay_many`). Each hop costs exactly one
co-signature and discloses exactly one superseded state, which the receiver's census counts and
proves out-raced. `sdk60` (alice → bob → carol, `F` unspent throughout), `sdk17` (a partial second
hop), `sdk76` (a received parent splitting — re-derived to an empty parent chain, pending run),
`sdk84` (child renewal).

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
a shared ancestor is rejected). A combine spends `SP.out[j]` directly and never broadcasts the leaf's
own tiers, so it is also how a small leaf realises far more of its face value than a walk does.

The **coloured** combine is **retired** (2026-09-06), and this is a capability the corpus used to
claim. `colored_combine_transfer` (`clients/libs/rust-sdk/src/tokens.rs`) spent every input carrier's
funding output `F` directly and gave each output a flat backup; with no flat backup it has no exit
material, so both its caller and its own body now call `refuse_legacy_colored_split_lane`, which
refuses unconditionally, and its registration step `register_combine_subcoins` refuses by name too.
The practical effect: a token payment larger than any **single** carrier holds is refused, not
combined. (`sdk31`, which drove that lane, has not been re-derived and would now fail.) The
**sats-leaf** driver `combine_leaves` (`clients/libs/rust/src/combine.rs`) is untouched — `sdk83`
drove it end to end against a live SE and a live chain, N leaves into one UTXO with blind and mempool
attempts both refused — but it has no caller outside that test, so no wallet method reaches it. That
is why [`../spec/PARTIAL-PAYMENT-ECONOMICS.md`](../spec/PARTIAL-PAYMENT-ECONOMICS.md)'s swept row is
not a number that ships.

## Transfers

A transfer is a **key handover** — no block, no fee, sub-second, fully async:

1. the sender co-signs the receiver-paying state `S'` one δ lower than the current one — that is
   the **only** thing pre-signed for the receiver; no flat backup is built, and the message carries
   an empty `backup_transactions` vector — then posts an encrypted transfer message through the
   SE's relay;
2. the receiver validates everything, then calls the SE, which **rotates its key share** — from that
   moment only receiver + SE can sign, and the sender's share is dead.

While a transfer is open, the SE holds a **pending-transfer lock** on that coin and refuses the
sender any further co-signature. Two ordering rules make that safe: all of the sender's own pre-signs
happen *before* `get_new_x1` opens the transfer, and any lane that co-signs a superseding state moves
the coin out of the live set **durably** before the co-sign, so a watchtower is never armed to
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

- **`F` is on-chain, unspent** and pays the expected aggregate key — the receiver takes `F` from the
  bundle, fetches `tx0` from the chain and binds with `coin_authority_from_tx0`, cross-checked
  against the coordinator's recorded aggregate; it books `locktime = None`;
- the conveyed structure is consensus-valid back to that root: `T` spends `F` with no timelock, tier
  outputs pay the correct publicly-tweaked keys, each tier's **signed** nSequence lies inside the
  band its kind allows (`[e_floor, e0]` for an extension, `[d_floor, d0]` for a state, exactly
  `SPINE_CSV = 0` for a split tier) and its **declared** CSV is bound to that signed number
  (`bind_declared_csv`), so a schedule that contradicts the signatures is refused rather than
  believed;
- **no flat backup and no branch material travels with a ladder.** `backup_transactions` must be
  empty — `verify_flat_backup_lane` refuses any non-empty vector on either lane, and
  `refuse_conveyed_flat_backups` does the same on the child, tail and stub lanes — and
  `refuse_branch_material` refuses exit-branch transactions or terminal-parent ids beside a ladder;
- the **census** — exact equality `se_num_sigs == tiers + superseded`, with the flat term pinned to
  **0** by that emptiness (`PARENT_V2_BASELINE = 0`, `CHILD_V2_BASELINE = 0`). A hidden, undisclosed
  rival state shows up as a count mismatch. This is the linchpin, and it only proves anything if the
  count is the *enclave's*: `get_statechain_info` sends a fresh random nonce and refuses any answer
  not carrying a `utexo/sig_count/v2` attestation over (`statechain_id`, `num_sigs`, budget, nonce),
  verified against the pinned identity. The census generalizes to N hops for children, per segment.
  `sdk46`, `sdk47`, `sdk54`, `sdk58`, `sdk60`, `sdk70` (those touched to pin the zero flat term are
  re-derived, pending run);
- **terminality** is likewise derived from the enclave-signed payload (`attested_terminal`) for the
  parent and every intermediate segment, keeping the coordinator's answer only as a cross-check that
  refuses on disagreement;
- for tokens, the RGB consignment is client-validated, with un-broadcast witness transactions
  allowed.

## Tokens

Tokens are **RGB assets**: client-validated contracts whose allocations live on coins and sub-coins.
The server knows nothing about tokens — validation is done by the receiving wallet against
cryptographic consignments. Two rules matter at concept level:

- RGB transitions anchor **only in signed-once transactions** — coloured self-transitions and
  coloured *tiers*. Plain ladder tiers are sats-only and would destroy an allocation, which is why a
  carrier may never be given a **plain** ladder; the ladder it is given instead is a coloured one,
  wherever an enclave attestation identity is pinned, and a carrier that cannot be coloured has no
  exit material until it can be.
- **Terminal-freeze**: a coloured transaction only ever spends outputs of *terminalized* structure,
  so no ancestor of an RGB anchor is ever re-signed and no superseded coloured witness exists
  anywhere. The lane rule that used to sit beside it — "every conveyed flat backup on a coloured
  bundle must be plain", so that a prior owner could not keep a coloured spend of `F` — has become a
  refusal of the whole category: `verify_flat_backup_lane` refuses *any* conveyed flat backup, plain
  or coloured, on either lane. A prior owner holds no spend of `F` that could ever mature.

Token pieces are sized from the coloured floors rather than chosen: `TOKEN_PIECE_SATS` = **4,074**
(`clients/libs/rust-sdk/src/tokens.rs`) is the coloured root-ladder floor computed at twice the
committed rate, so a piece still clears its floor if that rate ever doubles. See [tokens](tokens.md).

## Lightning

Lightning works **both directions on the ladder**, through a **HODL-invoice latch**: the SE's
co-signature is gated on the payment preimage, so the off-chain state moves if and only if the
Lightning payment settles. PTLCs do not exist in the routing network, so no adaptor-signature
construction is available; the latch uses HODL plus a BOLT11 preimage only. The latch no longer
requires the coin to carry a `locktime` — no coin has one.

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
  your L1 address; its locktime is taken from the current tip alone, so there is nothing to wait for.
  Since 2026-09-06 `withdraw::execute` reads **no** backup rows (it used to refuse "no backup
  transaction associated with this statechain ID", which after the flat chain's removal would have
  been every coin), and where several rows share a statechain id it prefers the **live** one
  (`CONFIRMED` or `IN_TRANSFER`) rather than the lowest locktime, there being none. One exception: a
  *received in-ladder child* has no
  confirmed outpoint to spend (its funding `SP.out[j]` is un-broadcast), so it routes to the
  unilateral walk instead, whose final state already pays your own key.
- **Unilateral (SE gone)**: never needs anyone's cooperation. You walk the pre-signed chain tier by
  tier, waiting out each relative timelock — `T`, then `X`, then `S`, plus the tiers of each split
  level — from the block the deposit is first seen in. A **coloured** carrier walks the identical
  chain, and because every tier is a valid RGB transition the walk moves the allocation to your own
  key rather than sweeping it away.
- **A coin with no ladder has no *unilateral* exit.** There is no last arm: `unilateral_exit`
  refuses such a coin by name — "a coin's only exit material is its ladder, and there is no flat
  backup to fall back to" — and `transfer_sender::execute` refuses to convey it. What still works is
  the **cooperative** withdrawal, which since 2026-09-06 reads no backup rows at all
  (`withdraw::execute`); so the coin is not lost, but getting it out needs the SE. `claim()` retries
  the transient reasons every pass. A carrier that cannot be coloured is one member of this class
  (`RgbCarrier`) and an unpinned network puts *every* SDK coin in it
  (`AttestationIdentityUnpinned`). `materialise_carrier` only ever finds the legacy `branch-` rows of
  the retired split/combine lane, and a carrier minted since has nothing for it to broadcast.

`estimate_exit_cost` prices a specific coin's walk before you commit to it: for a laddered coin the
tier walk's signed vsize, `wait_blocks: 0` while idle, and both deadline fields `None` — "laddered,
event-driven".

See [deposits & exits](exits.md), the cost model in
[`../spec/PARTIAL-PAYMENT-ECONOMICS.md`](../spec/PARTIAL-PAYMENT-ECONOMICS.md), and the
party-by-party matrix in [`../spec/TRUST-MODEL.md`](../spec/TRUST-MODEL.md).
