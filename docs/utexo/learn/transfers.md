# Transfers

> How partial amounts work under the hood — in-ladder splits, admission floors, exit costs — is
> covered end-to-end in the [granularity deep dive](granularity-deep-dive.md).

## What a transfer is

Mercury transfers move **whole coins** by key handover (like Spark moves whole leaves):

1. **Sender pre-signs the receiver's exit.** For a laddered coin that is a fresh **state tier**
   paying the receiver at a strictly *lower* relative timelock than the one it replaces; for an
   un-laddered coin it is the next link in the signed-once backup chain, at a lower absolute
   locktime. Either way the receiver ends up holding an exit that matures **before** every previous
   owner's.
2. **Sender opens the transfer at the SE** (`get_new_x1`) and signs the coin over to the receiver's
   address, posting an encrypted transfer message through the SE's relay. Opening the transfer arms
   the **pending-transfer lock**: from that moment the coordinator refuses any further sender
   co-signature on that coin, so no rival state can be minted behind the receiver's back. (All of
   the sender's own pre-signs happen *before* the open, which is what makes refusing everything
   afterwards safe.)
3. **Receiver** (async, whenever online) validates everything and calls the SE, which **rotates its
   key share** — the aggregate key is unchanged, so every pre-signed exit stays valid, but from now
   on only receiver+SE can sign and the sender's share is dead.

No block confirmation, no fee, sub-second, fully async. *Evidence:* `sdk41` (laddered transfer),
`sdk49` (the receiver adopts the ladder and unilaterally exits it — proof the handover is
self-custodial), `sdk47` (the full receiver-verification set across a transfer).

## One protocol, two coin shapes

There is **one protocol**. `claim()` establishes a TES-R exit ladder for every fresh confirmed
**root** coin, unconditionally — there is no protocol-version flag and no legacy lane. But not every
coin is laddered, by design, and the transfer mechanics differ between the two shapes:

| | **Laddered** | **Un-laddered** |
|---|---|---|
| Which coins | every plain BTC deposit | RGB **carriers**; split sub-coins funded by an un-broadcast tx |
| Exit structure | `F → T` (trigger, no timelock) `→ X_m` (extension, relative CSV) `→ S_k` (state, relative CSV), all pre-signed and **un-broadcast** | one signed-once backup tx per owner, **absolute** nLocktime |
| A transfer… | co-signs a fresh state `S'` one δ **lower** and discloses the state it replaces | pre-signs the receiver's backup at locktime = previous − interval |
| Idle cost | **zero** — relative timelocks only start counting once the parent confirms, and `T` has no timelock, so nothing matures until someone broadcasts `T`. An idle coin never ages | a real calendar deadline: the root backup matures at `deposit_height + initlock` |
| Unilateral exit | walk the pre-signed chain tier by tier, waiting out each relative timelock (`sdk50`) | broadcast your backup (and branch) and win the race |

The un-laddered shape is **load-bearing and current**, not deprecated: an RGB carrier is
deliberately never laddered because a plain tier spend would sweep the sats and destroy the
allocation (terminal-freeze, [PROTOCOL.md §5.10](../PROTOCOL.md); pinned by `sdk52`), and a sub-coin
whose funding output has never been broadcast cannot root a trigger at all. Everything below that
says "decrementing locktimes" belongs to *that* shape; everything that says "relative CSV" belongs to
the laddered one. Both exits are walked through in [exits](exits.md).

## Transferring a laddered coin: replace-by-lower-timelock

```
F   (on-chain funding, key = owner + SE — the only thing resting on chain)
└─ T      TRIGGER    no timelock, signed once at deposit, never re-signed
   └─ X_m EXTENSION  relative CSV E_m   (renewal replaces it horizontally)
      ├─ S_k  STATE  relative CSV Δ_k   ← the state the SENDER held (now superseded)
      └─ S'   STATE  relative CSV Δ_k − δ, pays the RECEIVER   ← co-signed at transfer
```

Each hop costs **exactly one co-signature** and discloses **exactly one superseded state**. Because
`S'` sits one δ (36 blocks ≈ 6 h on mainnet defaults) below the state it replaces, the new owner's
exit always matures first — a stale owner who broadcasts the old state loses the race by
construction. Nothing here touches the chain, and nothing starts ticking: BIP-68 relative timelocks
only begin counting after the parent *confirms*, and `T` has no timelock at all, so an idle coin sits
at **0 vB of rent with no deadline to miss** (`sdk30` idles a laddered coin 300 blocks and asserts
the exit chain is byte-identical afterwards).

The receiver's protection against a *hidden* extra state is a **census**: the SE publishes an
authoritative signature count for the coin, and the receiver checks it equals exactly the number of
tiers it was handed plus the disclosed superseded ones. Any undisclosed rival shows up as a count
mismatch and the claim is rejected (`sdk46`, `sdk47`, adversarially `sdk54`). The claim also
re-derives everything from public data: `F` on-chain and unspent, `T` spending it with no timelock,
each tier paying the right tweaked aggregate key, and the CSVs matching the SE's published counters.

## Transferring an un-laddered coin: the backup chain

An RGB carrier or an off-chain sub-coin carries no ladder. It transfers the original way: the sender
pre-signs the receiver's backup transaction at **locktime = previous − interval**, so each successive
owner holds a backup that unlocks earlier than every previous owner's, and hands it over with the
same SE key rotation. Sub-coins additionally carry their **exit branch** — the fully-signed,
un-broadcast transactions linking their funding output back to an on-chain root — and the receiver
validates it end-to-end (consensus-valid txs back to a confirmed, unspent root) plus requires every
structural ancestor to be **terminal** at the SE before accepting.

This shape *does* have a calendar: the root backup matures at `deposit_height + initlock`, so a
received carrier must be materialized on-chain before then. That is automated — the `auto_exit_due`
watchtower pass materializes a received carrier's branch as it nears the deadline (`sdk34`), and the
duty is delegable keyless to an external tower (`sdk45`, whose exported bundle carries no key
material). See [tokens](tokens.md#tokens-over-time--holding-and-doing-nothing).

## Exact amounts: the amount-maker

`transfer(address, amount)` never asks you to think about coins:

- **Exact subset.** If some subset of your coins sums to the amount, each is handed over whole
  (works with any Mercury wallet as receiver).
- **In-ladder split** (laddered parent — the common case). Otherwise the SDK splits one coin
  *inside its own ladder*: a split state `SP` spends `X_m.out[0]`, so it **descends from the
  trigger** instead of competing with it for the funding outpoint. `SP` pays two children — a
  **piece** whose own headless ladder pays the recipient, and a **change** child that stays yours.
  The piece is conveyed straight to the recipient's mailbox together with the key-handover material.
  The whole thing is one SE-co-signed, **un-broadcast** transaction: zero on-chain footprint, and a
  payment that never completes simply rolls back.

  Descending from the trigger is the load-bearing detail: a past owner's retained, no-timelock
  trigger has nothing to race, because the split does not contend for `F`. *Evidence:* `sdk58`
  (11 adversarial cases, all rejected) and `sdk59` (end-to-end split payment, receiver exits).

- **Colored split** (un-laddered parent — every token payment, and any plain sub-coin). One
  SE-co-signed, un-broadcast tx mints an exact piece plus change, both immediately first-class
  off-chain coins; the receiver gets the exit branch inside the transfer message and verifies it
  back to an on-chain root.

The in-ladder split has an **admission floor**: a child is not a bare output — it funds its *own*
extension and state tiers (each burning a committed fee plus a 240-sat P2A anchor) and its final
output must still clear dust. That is `min_child_value` = **1,306 sat** at the deployed 2 sat/vB
committed rate, and both the piece and the change must clear it. The check runs *before* the parent
is made terminal, so an undersized payment is refused rather than stranding the parent.

```
alice coins: [60k, laddered]          alice pays bob 15k:
                                      T.out       = 60,000 − 488  = 59,512
                                      X_0.out     = 59,512 − 488  = 59,024
                                      split total = 59,024 − 574  = 58,450
                                      SP → [piece 15,000 → bob][change 43,450 → alice]
                                      all un-broadcast; F still unspent
```

Spark achieves the same UX with SSP swap pools (fixed leaf denominations swapped server-side); here
exact amounts are a first-class off-chain operation with no third party and no liquidity table.

**Fan-out and fan-in.** `transfer_many` (sats) and `batch_transfer_tokens` (assets) carve N
recipient pieces plus change in a single split — width is far cheaper than paying N people
sequentially. For tokens there is also a **fan-in** form: `colored_combine_transfer` spends N
carriers of one asset into an exact piece plus change (N inputs → 2 outputs), and the receiver then
requires **all N** input carriers to be terminal. The token forms are *plain* splits, i.e. the
un-laddered lane. `transfer_many` routes per parent shape, exactly like `transfer`: a laddered coin
gets a multi-child **in-ladder** split (one `SP` over `X_m.out[0]` paying N children plus change,
each conveyed with the standard key handover), a received child gets the child-level equivalent, and
only an un-laddered coin gets the plain split — so a batch never plain-splits a laddered parent, which
would be the [B1] shape (see
[granularity deep dive §2c](granularity-deep-dive.md#2c-paying-three-people-at-once-width-beats-depth)).
A batch is still not atomic across recipients.

## Receiving

Receiving is passive: share your address (`get_utexo_address()` — stable, reusable). The SDK's
background watcher claims incoming transfers automatically and emits `TransferClaimed` /
`TokenTransferClaimed` events. A claim:

- verifies the transfer signature and, per shape, either the **ladder** (tiers, CSVs, census against
  the SE's signature count) or the **backup chain + exit branch** (consensus-valid back to a
  confirmed, unspent on-chain root, with terminal ancestors);
- completes the SE key rotation, which is what makes the coin yours and locks the sender out;
- for tokens: validates the consignment off-chain and books the verified asset.

## Received pieces are first-class

A piece you received from an in-ladder split is a **first-class coin**, not an exit-only claim. Its
claim completes the standard SE key handover, so the aggregate `A_child` is invariant across the
rotation — which is exactly what keeps its pre-signed exit chain valid — while the sender is
**permanently** locked out. From there you can pay it onward entirely off-chain:

- **whole** (`child_retransfer`): co-sign a fresh state over the child's extension output at a
  strictly lower CSV paying the next recipient, and disclose the replaced state;
- **partially** (`child_in_ladder_pay`): split the child at its own level into a piece and change,
  giving the recipient a grandchild with a depth-2 ancestor chain.

Every hop is one co-signature and one disclosed superseded state, counted by the receiver's
**N-hop census** (`SE signature count == backups + Σ conveyed tiers`) — an undisclosed rival cannot
survive it. *Evidence:* `sdk60` (alice → bob → carol, whole re-transfer, funding outpoint unspent
throughout) and `sdk17` (multi-hop with a partial second hop). The child is deliberately *not*
terminalized; the one exception is a Lightning-latched piece, which is terminalized because it sits
unclaimed past the pending-transfer lock's window (see [lightning](lightning.md)). Design notes:
[CHILDREN.md](../CHILDREN.md).

## Keeping a coin transferable

A transfer spends one rung of the state ladder, and the extension tier has its own budget, so a
long-lived coin needs maintenance — all of it **off-chain**:

- **Renewal** replaces the current extension horizontally with one at a lower CSV, resetting the
  state ladder to full height. Zero on-chain bytes.
- **Rollover**, when the extension budget is exhausted, turns the current state into a self-split
  paying the same aggregate and hangs a **fresh level** off it — again zero on-chain bytes. Coins can
  therefore live off-chain indefinitely (`sdk43` renews, rolls over, renews again and then exits
  through the whole deep chain; `sdk44` pins the schedule arithmetic).
- **`refresh` is the re-anchor primitive**, not a deadline reset: one cooperative on-chain
  transaction spends `F` into a brand-new aggregate, minting a new statechain id with a brand-new
  ladder and permanently killing every exit right rooted at the old `F` (`sdk30`). Use it to escape a
  ladder that has run out of headroom, or to consolidate — never because a clock is running out, as
  a laddered coin has no clock.

## Token transfers

`transfer_tokens(asset, address, amount)` = a **colored** off-chain split (the piece sub-coin carries
exactly `amount` of the asset; change keeps the rest) + the same handover, on the un-laddered lane,
because carriers are never laddered. The RGB consignment rides the transfer message and the receiving
wallet validates it client-side. See [tokens](tokens.md).
