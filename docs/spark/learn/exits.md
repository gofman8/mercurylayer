# Deposits and exits

> Exit timing, deadlines and pricing are specified normatively in
> [INVALIDATION-SPEC.md](../INVALIDATION-SPEC.md) (§6) and explained in depth — with cost tables —
> in [invalidation-deep-dive.md](invalidation-deep-dive.md) and
> [invalidation-economics.md](../research/invalidation-economics.md).
> Token-carrier coins exit differently — both plain paths refuse them (an RGB-unaware sweep destroys
> the allocation), so a carrier exits by **materializing** its branch. The `auto_exit_due` watchtower
> now does this automatically for a received carrier nearing its clawback deadline (branch-only,
> emitting `TokenCarrierMaterialized`), so token pieces get the same automated deadline protection
> plain coins do (SPEC §9.5 / REQ-33, `sdk34`); an issued/flat carrier has no ancestor and is left
> untouched. See [tokens.md "Exits with tokens"](tokens.md#exits-with-tokens) and
> [GRANULARITY-SPEC.md](../GRANULARITY-SPEC.md) GRN-INV-14.

## Deposits

`get_deposit_address(amount)` performs the SE handshake and returns a taproot address whose key
is `you + SE`. Send the exact amount; the SDK's watcher detects it, waits for confirmations,
creates the first backup transaction (your unilateral exit) and flips the coin to spendable —
emitting `DepositConfirmed`. One deposit = one coin = one tree root.

Deposit slots consume a **deposit token** from the SE's token server (anti-spam/fee mechanism).
The SDK requests one automatically; when tokens require payment it surfaces
`TokenPaymentRequired` with the payment details (apps can also pre-pay and pool tokens).

*Static addresses:* Mercury deposit addresses are per-coin. Reuse is detected and handled
(duplicate coins can be swept), but for privacy and cleanliness the SDK treats an address as
one expected deposit — `get_deposit_address` is cheap; call it per receive. (Spark's rotating
"static" addresses are the same idea server-rotated.)

## Cooperative exit (the normal path — one transaction)

`withdraw(l1_address, coins?)`: the SE co-signs a **fresh direct spend** of each coin to your L1
address. Immediate (no timelock), one on-chain tx per coin. Off-chain sub-coins are materialized
first: the SDK broadcasts their exit branch, then the withdraw spend (branch txs carry no
locktime — like Spark's zero-timelock split nodes — so materialization is instant).

Spark needs an SSP with an on-chain connector-tx swap for cooperative exits; here the SE's
co-signature on a direct spend does the whole job.

## Unilateral exit (the SE is gone)

`unilateral_exit(coins?)` broadcasts, per coin:

1. the **exit branch** (for sub-coins): the chain of pre-signed split/combine txs from the
   on-chain root down to the coin's funding output — consensus-final immediately;
2. the coin's **latest pre-signed backup tx** — locked to a future height by the decrementing
   scheme. You broadcast it once its locktime passes; every previous owner's backup unlocks
   *later* than yours, so you always have a window where only you can claim.

Nothing in this path talks to the SE. The obligations are timeliness ones: exit (or refresh)
before your locktime floor approaches, as in vanilla Mercury.

## Refresh (re-anchor)

Instead of exiting, you can **reset** a coin's lifetime on-chain. `refresh(statechain_id, fee_rate?)`
re-anchors the coin into a fresh aggregate (one SE-co-signed on-chain tx, ~112 vB), spending the old
outpoint — which invalidates all old backups and hands the coin a fresh ladder and root deadline. The
fee is drawn from the coin (user-pays); `refresh_sponsored(...)` layers an off-chain operator rebate on
top so the user ends ≥ whole. This is the shipped lifetime-extension option — cheaper than a full exit
and re-deposit. See [invalidation-economics.md](../research/invalidation-economics.md) §4b for the cost
model.

## Timelock summary

| Transaction | Locktime |
|---|---|
| deposit backup #1 | `now + initlock` at co-sign time |
| each transfer's new backup | previous − `interval` |
| split/branch txs | none (immediately broadcastable) |
| sub-coin's first backup | fresh ladder for the sub-coin |
| cooperative withdraw | none |
