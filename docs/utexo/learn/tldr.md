# TL;DR

**Mercury Utexo is a Bitcoin L2 with Spark-class UX on a single statechain entity.** Users deposit
BTC (or RGB tokens) onto statechain coins and then transact **off-chain, instantly, with no
per-payment on-chain cost**: payments of any amount, token transfers, and Lightning in both
directions. Every coin stays unilaterally exitable to Bitcoin L1 at any time, without anyone's
permission.

## The one thing to understand: an idle coin never ages

Every plain deposit is **laddered** — `claim()` establishes a **TES-R** ladder (*Trigger /
Extension / State*) over every fresh confirmed root coin, unconditionally. There is no protocol
version flag and no lane to choose:

```
F   on-chain funding output, 2-of-2 (you + SE)
└─ T    TRIGGER     no timelock — signed once at deposit
   └─ X_m  EXTENSION  RELATIVE CSV E_m — renewal replaces it horizontally, off-chain
      └─ S_k  STATE     RELATIVE CSV Δ_k — decrements by δ on every transfer
```

All three tiers are v3/TRUC with a P2A anchor, pre-signed and **un-broadcast**. Their timelocks are
**relative** (BIP-68/112): they only begin counting once the parent confirms — and `T` has no
timelock at all, so **nothing matures until someone broadcasts `T`**. Consequences, and they are the
whole design:

- **No calendar deadline, no expiry, no "exit before your locktime".** A coin sitting still is not
  on a clock.
- **0 vB of idle rent.** On-chain footprint scales with *activity*, not with time.
- **Off-chain forever.** Renewal (re-signing a lower-CSV extension) and rollover (a fresh level) are
  off-chain and unbounded — a coin can live off-chain indefinitely (`sdk43`).

`refresh` is the **re-anchor** primitive — one on-chain transaction that moves a coin to a fresh
funding outpoint and mints a new ladder (`sdk30`). It is not a deadline reset; there is no deadline.

## What you get

- **Instant, free transfers.** A payment is an SE-co-signed **key handover**: the SE rotates its key
  share to the receiver and the sender can no longer co-sign. Under the hood it is
  *replace-by-lower-timelock* — the sender co-signs a fresh state one δ **lower** than the one it
  replaces, so the new owner's state always matures first, and the old state is disclosed as
  superseded and counted by the receiver's census. No blocks, no miner fees, sub-second.
- **Any amount, no denominations.** If a subset of coins sums exactly, they are handed over whole.
  Otherwise the wallet does an **in-ladder split**: a state tier `SP` that spends `X_m.out[0]` — a
  *descendant* of the trigger, not a rival for the funding output — paying a piece child and a change
  child, each with its own two-tier ladder. The admission floor is `min_child_value` = **1306 sat**
  at 2 sat/vB (a child must fund its own extension + state and clear dust). `sdk58`, `sdk59`.
- **Received pieces are first-class.** Claiming a split child completes the standard SE key handover:
  the receiver co-owns `A_child` (invariant across the rotation, which keeps the pre-signed exit chain
  valid) and the sender is **permanently locked out**. A child can be paid onward off-chain — whole
  (`child_retransfer`) or split again (`child_in_ladder_pay`) — one co-signature and one disclosed
  superseded state per hop. `sdk60` (alice → bob → carol with the funding outpoint unspent throughout),
  `sdk17`. See [CHILDREN.md](../CHILDREN.md).
- **Tokens are RGB.** Client-validated assets (not trusted server state) ride the same coins: issue,
  transfer and exit tokens with the same UX as sats. See [tokens](tokens.md).
- **Lightning, both directions.** A HODL-invoice latch couples a coin (or an in-ladder piece) to a
  BOLT11 payment through any SSP: pay `sdk63`, receive `sdk64`, non-exact pay `sdk65`, non-exact
  receive `sdk67`, failure + rollback `sdk66`/`sdk68`, remote SSP over HTTP `sdk21`. A latched piece
  is the one case that stays terminalized (it sits unclaimed past the pending-transfer lock's window).
  See [lightning](lightning.md) and [LIGHTNING.md](../LIGHTNING.md).
- **Self-custody.** 2-of-2 (you + SE) with a pre-signed exit chain. The SE can never move funds alone
  and can never freeze you out. If it disappears, you exit unilaterally (below).

## Two coin shapes — both current

One protocol, two shapes. Which one a coin has is decided by what it carries, not by a version:

- **Laddered** — every plain BTC deposit. Trigger → extension → state, relative CSV, un-broadcast.
- **Un-laddered** — an **RGB carrier** is deliberately never laddered (a plain tier spend would sweep
  the sats and destroy the allocation — terminal-freeze, `sdk52`), and a **split sub-coin whose
  funding is un-broadcast** cannot root a trigger. These keep the signed-once backup and move by
  backup-chain handover, with decrementing absolute locktimes and the calendar duties that come with
  them; they exit by broadcasting their branch root-first (`sdk39` does it for a colored sub-coin two
  splits deep). This path is load-bearing for RGB assets — current, not deprecated.

## Unilateral exit

- **Laddered coin (SE gone):** broadcast `T`, then walk the pre-signed chain **tier by tier**, waiting
  out each relative timelock in turn (extension, then state). Not a single backup broadcast. `sdk50`.
- **Un-laddered coin (RGB carrier, split sub-coin):** broadcast the pre-signed backup and its exit
  branch; your backup's locktime is the earliest of all owners', so you win the race.
- **Cooperative exit (normal):** the SE co-signs a direct spend to your L1 address — one transaction,
  no wait.

**Watching, not deadlines.** Because a laddered coin only starts its clock when someone broadcasts
`T`, there is nothing to do on a calendar — but the trigger is public, and if a prior owner or a
griefer broadcasts it you must answer before their stale state matures. Your current state carries
the strictly lowest CSV, so it matures first and the funds land with you (`sdk51`). The duty is
event-driven with ≥1 day of notice, and it is delegable: a watch bundle holds **no key material**, so
any tower can drive your exit without being able to spend anything (`sdk45`).

See [deposits & exits](exits.md) and, for the normative version, [PROTOCOL.md](../PROTOCOL.md).

## The developer surface

The SDK (`mercury-utexo-sdk`) hides everything operational — UTXOs, ladders, tiers, renewal, coin
selection, splits, consignments, claim polling. Applications deal in **addresses and amounts**.

```rust
let (wallet, mnemonic) = UtexoWallet::initialize(SdkConfig::regtest("alice"), None).await?;
wallet.transfer(&bob_address, 15_000).await?;             // any amount, off-chain
wallet.transfer_tokens(&asset, &bob_address, 250).await?; // exact tokens, off-chain
wallet.pay_lightning_invoice(&ssp, bolt11).await?;        // -> preimage (proof of payment)
```
