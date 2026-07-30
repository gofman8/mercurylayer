# Core concepts

> The normative description of the shipped protocol is [PROTOCOL.md](../PROTOCOL.md) (TES-R);
> first-class split children are specified in [CHILDREN.md](../CHILDREN.md) and the Lightning latch
> in [LIGHTNING.md](../LIGHTNING.md). This page is the conceptual tour.

## The statechain entity (SE)

The SE is Mercury Layer's co-signing service: a server plus a lockbox (key enclave) holding **one
half of a 2-of-2 key for every coin** (blind MuSig2). It:

- co-signs spends without seeing what it signs (blind signing — it is handed 32-byte sighashes: no
  amounts, no addresses, no colors),
- rotates its key share on transfer, so previous owners lose the ability to co-sign,
- refuses conflicting state: one active spend chain per coin, plus a **pending-transfer lock** that
  denies the sender any co-signature while a transfer of that coin is open, plus the optional
  per-coin `single_use` hard rule and `epoch_deadline` after which it stops co-signing new state,
- **publishes counters** — how many signatures it has ever produced for a coin. That public number
  is what lets a receiver prove no hidden state exists (see [verification at claim](#verification-at-claim)).

The SE **cannot move funds** (it has one of two keys) and **cannot freeze you out** (you hold a
complete pre-signed exit chain). This is the same role Spark's operator set plays — collapsed to one
entity, so the trust assumption is "the SE is honest about refusing conflicting state" instead of
"1 of n operators is honest". Either way, unilateral exit never needs the operator's cooperation.

## A coin is a timelock ladder (TES-R)

A **coin** is a statechain: an on-chain funding UTXO **F** whose key is `owner + SE`, plus a
pre-signed, **un-broadcast** three-tier chain hanging off it — the **TES-R ladder**
(Trigger / Extension / State, with rollover):

```
F   on-chain funding UTXO, key = owner + SE       ← the only thing resting on-chain
└─ T    TRIGGER     no timelock, signed ONCE at deposit, never re-signed
   └─ X_m EXTENSION  relative CSV E_m  — renewal replaces it horizontally (E_{m+1} = E_m − δE)
      └─ S_k STATE   relative CSV Δ_k  — each transfer decrements (Δ_{k+1} = Δ_k − δ), pays owner k
```

All three tiers are v3/TRUC transactions with a 240-sat P2A anchor. Each carries a small committed
fee so it relays and confirms on its own; the anchor lets anyone (owner, watchtower, operator)
attach a live-rate fee child during a spike.

**The one property everything else follows from**: the tiers use *relative* (BIP-68/112 CSV)
timelocks, which only start counting once the **parent confirms** — and T has no timelock at all. So
**nothing matures until someone broadcasts T on-chain**. An idle coin never ages. There is no
calendar deadline, no expiry, nothing to "exit or refresh before", and **0 vB of idle rent** — a coin
can sit untouched for years and cost nothing. And when trouble does start it is loud: an adversary
must publish T on-chain first, and no hostile transaction can become valid for at least ~144 blocks
(~1 day) after that.

Shipped mainnet defaults (served by `/info/config`): state tier D0 = 1,440 blocks with δ = 36
(≈6 h of head start per hop); extension tier E0 = 720 with δE = 36, forced rollover at m = 15. The
worst-case unilateral wait on a fresh flat coin is E + Δ ≈ 2,160 blocks ≈ **15 days**, shrinking by
36 blocks with every hop.

### The ladder lives off-chain, indefinitely

- **A transfer** co-signs a fresh state one δ **lower** than the one it replaces
  (Decker–Wattenhofer *replace-by-lower-timelock*), so the new owner's state always matures **first**.
  The state it replaces is disclosed to the receiver as superseded — and counted.
- **Renewal**: when the next state CSV would fall below the floor, the SDK co-signs a fresh extension
  `X_{m+1}` at a lower CSV inside the transfer. It strictly undercuts every older extension in the
  race for `T.out[0]`, so every pre-renewal state hangs on an extension that can now never confirm.
  Zero on-chain bytes.
- **Rollover**: at epoch exhaustion a 1-in-1-out off-chain self-split mints a fresh level with a
  fresh hop budget. Also zero on-chain bytes, at the cost of one more depth level.
- Both are unbounded: `sdk43` drives renew → rollover → renew past exhaustion and then exits
  unilaterally through the whole deep chain, with the funding outpoint never touched.

`refresh` still exists, but it means something different now: it is the **re-anchor** primitive —
one on-chain transaction (~112 vB) that moves the coin to a fresh funding outpoint and mints a new
ladder. It is used to cap exit depth or to restore a coin after a hostile trigger, not to reset a
deadline; a laddered coin has no deadline to reset (`sdk30`).

### Defending, and exiting

If someone broadcasts T — a past owner racing a stale state, or a pure griefer — the coin is not in
danger, it is on notice:

- **Cooperative de-trigger** (the normal response): owner + SE key-path-spend `T.out[0]` immediately
  into a fresh funding output and rebuild the ladder. That spend has no timelock, so it confirms
  unopposed inside the ≥144-block window during which no adversary transaction is even valid. One
  ~111-vB transaction; griefing costs the attacker more than the victim.
- **Ladder defense** (SE unreachable): the owner — or a keyless watchtower holding the watch bundle —
  broadcasts the current extension at +E and the current state at +Δ. Because the current state is
  strictly the lowest-CSV one, it matures first and the funds land at the owner's key (`sdk51`).
- **Unilateral exit** is a **walk**, not a single broadcast: T, then X once its relative timelock has
  run, then S once its own has (`sdk50` — and `sdk45` performs the same walk from a watch bundle that
  contains **no key material** at all, with a second independent tower proven idempotent).

## One protocol, two coin shapes

Not every coin is laddered — by design, and both shapes are current. There is no version flag to
choose between them: `claim()` ladders every fresh confirmed **root** coin unconditionally.

| | **Laddered** | **Un-laddered** |
|---|---|---|
| Which coins | every plain BTC deposit | **RGB carriers**; split sub-coins whose funding is un-broadcast |
| Exit material | T → X_m → S_k, relative CSV, un-broadcast | one signed-once backup tx, absolute nLockTime |
| A transfer | co-signs a state one δ lower | pre-signs the receiver's backup at `previous − interval` |
| Aging | never — nothing ticks until T is broadcast | absolute locktimes; a received coin has a root deadline |
| Owner duty while idle | none | materialize the branch before the deadline (automatic) |

Why the second shape exists:

- an **RGB carrier is deliberately never laddered**. RGB transitions may anchor only in signed-once
  transactions, and a plain tier spend (T/X/S) would destroy the allocation. This is the
  *terminal-freeze* rule, and it is load-bearing for tokens — current, not deprecated (`sdk52` pins
  it: in one wallet the plain coin carries a ladder and the token carrier carries none, and an
  off-chain RGB transfer still settles).
- a **split sub-coin over un-broadcast funding cannot root a trigger** — a trigger needs a confirmed
  prevout to spend, and a v3 tier cannot relay over an unconfirmed parent. This is checked against
  the chain fail-closed, never inferred.

So the decrementing-locktime machinery — backup chains, root deadlines, materialize-before-the-deadline
— is entirely real; it simply belongs to this shape. A received carrier's deadline duty is handled
for you by the SDK's watchtower, which materializes the colored branch before the clawback window
opens (`sdk34`; `sdk32` documents the residual window if nobody acts).

## Sub-coins, splits and combines (Spark: trees and leaves)

Payments that are not an exact subset of your coins are made by **splitting off-chain**.

On a laddered coin, a split is an **in-ladder split**: a state tier `SP` spending `X_m.out[0]` — a
**descendant of the trigger**, never a rival for the funding outpoint F. That is the whole security
argument: a past owner's retained no-timelock trigger has nothing to race, because the split does not
compete for F. `SP` pays a **piece child** and a **change child**, and each child hosts its own
extension + state tiers (no trigger needed — `SP` is itself un-broadcast, so nothing ticks until it
confirms).

```
F (on-chain root)
└─ T ── X_m
         └─ SP  (state tier, un-broadcast, exact amounts, Σout = Σin − fee)
              ├─ piece child   → paid to Bob     → its own X/S tiers
              └─ change child  → stays with Alice → its own X/S tiers
```

- **Width is free**: carving N pieces is one off-chain transaction.
- **Depth costs on exit**: a unilateral exit walks 3 + 2d transactions and waits out each tier's CSV,
  so the SDK caps depth (default 3) and re-anchors past it.
- **There is an admission floor**: a child funds its own two tiers plus dust, so the smallest mintable
  piece is `min_child_value` = **1,306 sat at 2 sat/vB**. The floor is checked *before* the parent is
  terminalized, so a too-small piece is refused cleanly instead of stranding the parent (`sdk58` —
  11 adversarial cases, all rejected; `sdk59` — the end-to-end split payment).

### Received children are first-class

A received piece is a real coin, not an exit-only claim. The claim completes the standard SE **key
handover**: the child aggregate `A_child` is *invariant* across the rotation
(`sender_share + SE_old == receiver_share + SE_new`), which is exactly what keeps the pre-signed
child exit chain valid, while the sender is **permanently locked out**. The receiver can then pay the
child onward off-chain — **whole** (`child_retransfer`) or **split again** (`child_in_ladder_pay`,
which carries a depth-2 ancestors chain). Each hop costs exactly one co-signature and discloses
exactly one superseded state, which the receiver's census counts and proves out-raced. Evidence:
`sdk60` (alice → bob → carol, the funding outpoint unspent throughout) and `sdk17` (a partial second
hop).

### Combines

A **combine** goes the other way: one SE-co-signed transaction `CB` spends N sub-coins into fewer (or
one) outputs, carrying a per-input relative timelock (BIP-112 is per-input). The output's ancestry
becomes the *union* of all N inputs' ancestries plus the combine tx, so the tree becomes a **DAG** at
that node — but it is still a tree over *outpoints* (only disjoint input ancestries are combined; a
shared ancestor is rejected). This is why the receiver requires **Σ-inputs terminal ancestors**: one
terminal per structural input, not one per branch.

## Transfers

A transfer is a **key handover** — no block, no fee, sub-second, fully async:

1. the sender co-signs the receiver's new state (laddered) or pre-signs the receiver's backup
   (un-laddered), and posts an encrypted transfer message through the SE's relay;
2. the receiver validates everything, then calls the SE, which **rotates its key share** — from that
   moment only receiver + SE can sign, and the sender's share is dead.

While a transfer is open, the SE holds a **pending-transfer lock** on that coin and refuses the
sender any further co-signature until the transfer completes or times out, so no rival can be minted
during the receiver's verification window.

Transfers move whole coins; arbitrary amounts come from picking an exact subset of coins or minting
an exact piece with an in-ladder split (see [transfers](transfers.md)).

### Verification at claim

The receiver trusts nothing and re-derives everything from public data — any deviation is a reject:

- **F is on-chain, confirmed, unspent** and pays the expected aggregate key;
- the conveyed structure is consensus-valid back to that root: T spends F with no timelock, tier
  outputs pay the correct publicly-tweaked keys, the current extension's CSV matches the SE's
  published counters, and the new state's CSV is exactly one δ below the current one, with headroom;
- the **census**: the SE's public signature count equals exactly the expected number of co-signed
  transactions (`se_num_sigs == flat_backups + Σ conveyed_tiers`, where `flat_backups` counts the
  signed-once backup transactions conveyed with the coin; generalized to N hops for children).
  A hidden, undisclosed rival state shows up as a count mismatch — this is the linchpin (`sdk46`,
  `sdk47`, `sdk54`, `sdk55`, `sdk58`);
- for sub-coins, per-level branch validation plus Σ-inputs terminal ancestors;
- for tokens, the RGB consignment is client-validated (un-broadcast witness transactions allowed).

## Tokens

Tokens are **RGB assets**: client-validated contracts whose allocations live on coins and sub-coins.
The server knows nothing about tokens — validation is done by the receiving wallet against
cryptographic consignments. Two rules matter at concept level:

- RGB transitions anchor **only in signed-once transactions** — colored splits/combines and colored
  self-transitions. Plain ladder tiers are sats-only and would destroy an allocation, which is why a
  carrier is never laddered.
- **Terminal-freeze**: a colored transaction only ever spends outputs of *terminalized* structure, so
  no ancestor of an RGB anchor is ever re-signed and no superseded colored witness exists anywhere.

See [tokens](tokens.md).

## Lightning

Lightning works **both directions on the ladder**, through a **HODL-invoice latch**: the SE's
co-signature — including any renewal bundled into the same transfer — is gated on the payment
preimage, so the off-chain state moves if and only if the Lightning payment settles. Paying and
receiving both work for exact amounts and for non-exact ones carved by an in-ladder split (`sdk63`
pay, `sdk64` receive, `sdk65` non-exact pay, `sdk67` non-exact receive), with tested failure and
rollback paths (`sdk68`, `sdk66`).

The latched piece is the **one** case that stays terminalized: it sits unclaimed past the
pending-transfer lock's window (the payment provider settles on its own schedule), so a permanent
lockout replaces the temporary one. Every other in-ladder child relies on the census plus the key
handover instead. See [lightning](lightning.md).

## Exits

- **Cooperative (normal, 1 tx)**: the SE co-signs a fresh direct spend of the coin to your L1
  address — no timelock, no waiting. One exception: a *received in-ladder child* has no confirmed
  outpoint to spend (its funding `SP.out[j]` is un-broadcast), so `withdraw` routes it to the
  unilateral walk instead, whose final state already pays your own key.
- **Unilateral (SE gone)**: never needs anyone's cooperation. On a **laddered** coin you walk the
  pre-signed chain tier by tier, waiting out each relative timelock — T, then X, then S, plus two
  more transactions per split level. On an **un-laddered** coin you broadcast the branch and then the
  signed-once backup once its absolute locktime passes; your backup unlocks earlier than every
  previous owner's, so you win the race.
- **Tokens** exit by **materializing** the colored branch — both plain paths refuse a carrier, since
  an RGB-unaware sweep would destroy the allocation.

See [exits](exits.md) and, for the party-by-party matrix, [TRUST-MODEL.md](../TRUST-MODEL.md).
