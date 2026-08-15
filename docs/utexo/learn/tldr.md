# TL;DR

**Mercury Utexo is a Bitcoin L2 with Spark-class UX on a single statechain entity.** Users deposit
BTC (or RGB assets) onto statechain coins and then transact **off-chain, instantly, with no
per-payment on-chain cost**: payments of any amount, token transfers, and Lightning in both
directions. Every coin stays unilaterally exitable to Bitcoin L1, without anyone's permission.

Normative detail lives in [`../spec/`](../spec/README.md). This page is the whole system on one
screen.

## The one thing to understand: an idle ladder never ages

`claim()` establishes a **TES-R** ladder (*Trigger / Extension / State*) over every fresh confirmed
**root** coin, unconditionally — no setting selects it, and a laddered coin cannot be conveyed as a
flat one (`sdk71`):

```
F   on-chain funding output, 2-of-2 (you + SE)
└─ T    TRIGGER     no timelock — signed once at deposit
   └─ X_m  EXTENSION  RELATIVE CSV E_m — renewal replaces it horizontally, off-chain
      └─ S_k  STATE     RELATIVE CSV Δ_k — decrements by δ on every transfer
```

All three tiers are v3/TRUC with a 240-sat P2A anchor, pre-signed and **un-broadcast**. Their
timelocks are **relative** (BIP-68/112): they only begin counting once the parent confirms — and `T`
has no timelock at all, so **nothing matures until someone broadcasts `T`**. Consequences:

- **No CSV-side clock.** A ladder sitting still is not ageing toward anything.
- **0 vB of idle rent.** On-chain footprint scales with *activity*, not with time.
- **Off-chain forever.** Renewal (re-signing a lower-CSV extension) and rollover (a fresh level) are
  off-chain and unbounded — a coin can live off-chain indefinitely (`sdk43`).

**The honest half of that sentence.** The ladder is not the coin's only pre-signed material. A
**flat backup chain** over `F` is retained alongside it, carrying absolute locktimes
`L_k = L_0 − k·interval`, one decrement per whole-coin hop. So a coin that has been *received* sits
on `min(L_k)` — a real calendar height held by its prior owners — inside a root epoch of
`initlock` = **10,000 blocks ≈ 69.4 days** on mainnet (`TesrParams::flat_ladder_params_const`,
`lib/src/tesr.rs`). `SDK_E2E=86` measures exactly this. Laddering deletes the CSV-side ageing and
nothing else; `deadline_safety_due` (`clients/libs/rust-sdk/src/refresh.rs`) is what defends the
remaining height, and `refresh` — one ~112-vB on-chain **re-anchor** to a fresh funding outpoint with
a fresh ladder (`sdk30`) — is how you buy a new epoch.

## A payment is a split, and the piece is a real coin

Payments are arbitrary amounts. An arbitrary amount matches a coin you already hold only by
coincidence, so essentially every payment is an **in-ladder split**: a state tier `SP` spending
`X_m.out[0]` — a *descendant* of the trigger, never a rival for the funding output. `SP` is signed at
`SPINE_CSV = 0` (`clients/libs/rust/src/tesr.rs`) and carries K+1 payload outputs:

- **piece children** — one per payee, each hosting its own extension + state rungs
  (`establish_child`), conveyed with a full key handover;
- **the spine tip** — the sender's own change, ONE cap tier directly over `SP.out[K]` and no
  extension (`establish_spine_tip_journalled`). The next payment is a **spine batch**
  (`spine_batch_split`) over that tip, so payment 1 and payment 1000 are the same object and each
  payment adds exactly **one** transaction to the sender's exit chain.

Floors are rate evaluations, not constants: at the shipped committed rate of **3 sat/vB**
(`TesrParams::committed_fee_rate`) a piece must clear `min_child_value` = **1,560 sat** and a tip
`min_spine_tip_value` = **945 sat** (`lib/src/tesr.rs`). Both are checked *before* the parent is
terminalized, so a piece too small to establish never strands its parent. `sdk58` (12 tamperings,
each rejected for the named reason it targets), `sdk59` (the end-to-end split payment).

**Received pieces are first-class.** Claiming a child completes the standard SE key handover: the
child aggregate `A_child` is invariant across the rotation, which is what keeps its pre-signed exit
chain valid, and the sender is **permanently locked out**. A child pays onward off-chain — whole
(`child_retransfer`) or split again (`child_in_ladder_pay`) — one co-signature and one disclosed
superseded state per hop. `sdk60` (alice → bob → carol with `F` unspent throughout), `sdk17`.
See [`../spec/CHILDREN.md`](../spec/CHILDREN.md).

## What you get

- **Instant, free transfers.** A transfer is an SE-co-signed **key handover**: the SE rotates its key
  share to the receiver, and the sender's share is dead. Under the hood it is
  *replace-by-lower-timelock* — the sender co-signs a fresh state one δ **lower** than the one it
  replaces, so the new owner's state always matures first, and the replaced state is disclosed as
  superseded and counted by the receiver's census. No blocks, no miner fees, sub-second.
- **Any amount, no denominations.** Exact subsets are handed over whole; everything else is an
  in-ladder split (above).
- **Tokens are RGB.** Client-validated assets, not trusted server state: issue, transfer and exit
  tokens with the same UX as sats. See [tokens](tokens.md).
- **Lightning, both directions.** A HODL-invoice latch couples a coin (or an in-ladder piece) to a
  BOLT11 payment through any SSP: pay `sdk63`, receive `sdk64`, non-exact pay `sdk65`, non-exact
  receive `sdk67`, failure + rollback `sdk68`/`sdk66`, remote SSP over HTTP `sdk21`. A latched piece
  is the one case that stays terminalized. See [lightning](lightning.md) and
  [`../spec/LIGHTNING.md`](../spec/LIGHTNING.md).
- **Self-custody.** 2-of-2 (you + SE) with a pre-signed exit chain. The SE can never move funds alone
  and can never freeze you out.

## Two coin shapes

One protocol, two shapes. Which one a coin has is decided by what it carries; the transfer message's
`protocol_version` reports that shape to the receiver rather than selecting it:

- **Laddered** — every plain BTC deposit. Trigger → extension → state, relative CSV, un-broadcast.
- **Un-laddered** — `SdkConfig::colored_ladder` ships **false** on both presets
  (`SdkConfig::regtest` / `::mainnet`, `clients/libs/rust-sdk/src/config.rs`), so an **RGB carrier**
  takes the flat signed-once backup shape: a plain tier spend would sweep the sats and destroy the
  allocation (terminal-freeze, `sdk52`). A **split sub-coin whose funding is un-broadcast** likewise
  cannot root a trigger. These coins move by backup-chain handover with decrementing absolute
  locktimes and the calendar duties that come with them (`auto_exit_due`, `exit_deadline_block`;
  `sdk34` materializes a carrier before its deadline, `sdk32` documents the residual window). They
  exit by broadcasting their branch root-first — `sdk39` does it for a coloured sub-coin two splits
  deep. This path is load-bearing for RGB.

A coloured ladder exists behind the flag (`sdk74` establish, `sdk75` exit, `sdk77` coloured in-ladder
split), and turning it on is the route to one coin type.

## Unilateral exit

- **Laddered coin (SE gone):** broadcast `T`, then walk the pre-signed chain **tier by tier**, waiting
  out each relative timelock in turn. Not a single broadcast — `unilateral_exit` (`sdk50`), and
  `sdk45` drives the same walk from a bundle holding **no key material**.
- **Un-laddered coin:** broadcast the pre-signed branch and its backup; your locktime is the earliest
  of all owners', so you win the race.
- **Cooperative exit (normal):** `withdraw` — the SE co-signs a direct spend to your L1 address, one
  transaction, no wait. A received in-ladder child has no confirmed outpoint to spend, so it routes
  to the walk instead.

**Watching, not deadlines.** Nothing on the ladder matures until `T` is public, but `T` *is* public:
if a prior owner or a griefer broadcasts it you must answer before their stale state matures. Your
current state carries the strictly lowest CSV, so it matures first (`defend_ladders`, `sdk51`). The
normal answer is the **cooperative de-trigger** — owner + SE key-path-spend `T.out[0]` at zero CSV
wait (`detrigger_to_owner`, `sdk89`). The duty is event-driven with ≥144 blocks of notice and
delegable: a watch bundle holds no key material. A keyless tower cannot **fee-bump** a stuck tier —
that needs an owner-funded input, and `SdkConfig::fee_bump` ships as `None`.

## What a payment costs

Priced on the **leaf** lane — the payee of a split — because after the first payment that is
everyone ([`../spec/PARTIAL-PAYMENT-ECONOMICS.md`](../spec/PARTIAL-PAYMENT-ECONOMICS.md)):

| per payment | block space | vs ~154 vB on chain |
|---|---:|---|
| spent onward off-chain | **0 vB** | this is the product |
| swept and settled | ~105 vB | 1.47× better — the cap without the discharge round |
| shipped default | 418 vB | 2.7× worse |
| walked out unilaterally | 250 – 2,719 vB | worse than on-chain |

The walked range is `293·d + 375` vB over `3 + 2d` transactions; its top is the mainnet cap of depth
**8** / **19 transactions**, which is *derived* from the schedule and the epoch (`max_split_depth`,
`max_exit_txs`, `lib/src/transfer/receiver.rs`), not a chosen literal. The swept row is the row that
turns the lane positive for the median user, and it is the one no product surface reaches:
`combine_leaves` (`clients/libs/rust/src/combine.rs`) is driven end to end against a live SE and a
live chain by `sdk83`, and has no caller outside it — no wallet method exposes it. Quote the 418-vB
shipped default when you need a number that ships.

## What is not built

Stated here rather than left for a reader to discover:

- **The discharge round** (`../spec/SPEC.md` §5.4) is design. Its SE enforcement point is empty, so
  the round's economics may not be quoted as shipped.
- **De-trigger restoration.** The de-trigger itself is proven end to end; what does not exist is a
  rebuilt `F′` and a fresh `T′/X′_0/S′_0`, so getting back off-chain after one is a fresh deposit.
- **The conveyance window.** Between conveyance and claim the coordinator holds the payer off with a
  hard-coded one-hour wall clock (`OPEN_TRANSFER_WINDOW_SQL`). `sdk91` measures it directly: a payer
  who skips their own client and POSTs `/sign/first` gets **HTTP 409** inside the window and **HTTP
  200** with a `server_pubnonce` once the row ages past it. `sdk90` shows an honest client is stopped
  by two independent *local* gates first — which a cheating payer does not run. The owner latch that
  would replace the clock is specified and not built.
- **The enclave attestation identity.** The census is only as good as the key that signs it, and
  `TesrParams::attestation_identity_const` returns `None` for every network today, so the identity
  must be configured (`SdkConfig::attestation_identity`) — the client refuses rather than falling back
  to the key the coordinator serves.

## The developer surface

The SDK (`mercury-utexo-sdk`) hides everything operational — UTXOs, ladders, tiers, renewal, coin
selection, splits, spine batches, consignments, claim polling. Applications deal in **addresses and
amounts**.

```rust
let (wallet, mnemonic) = UtexoWallet::initialize(SdkConfig::regtest("alice"), None).await?;
wallet.transfer(&bob_address, 15_000).await?;              // any amount, off-chain
wallet.transfer_tokens(&asset, &bob_address, 250).await?;  // exact tokens, off-chain
wallet.pay_lightning_invoice(&ssp, bolt11).await?;         // -> preimage (proof of payment)
```

Next: [core concepts](core-concepts.md) for the tour, [transfers](transfers.md),
[deposits & exits](exits.md), and [`../spec/PROTOCOL.md`](../spec/PROTOCOL.md) for the normative
protocol.
