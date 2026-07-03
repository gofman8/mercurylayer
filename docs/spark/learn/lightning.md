# Lightning

Lightning interop uses the **Mercury lightning latch** — the same preimage-swap shape Spark runs
through its SSPs, on a single SE.

## The latch

A coin can be transferred under a `batch_id` whose claim is **locked** until the sender confirms.
The SE holds a fresh preimage for the batch and publishes its SHA256 (**payment hash**). That
couples a statechain transfer to a Lightning payment:

```
alice (has coin)                LSP (runs a Lightning node)
   │ start_lightning_swap(LSP)     │
   │  SE: preimage; hash H         │
   │  coin → LSP, latch-locked ───▶│ get_swap_payment_hash(batch) == H ✓
   │                               │ pays alice's invoice locked to H (HTLC)
   │ invoice paid ✓                │
   │ settle_lightning_swap ───────▶│ claim unlocks → LSP owns the coin
   │  ← preimage (settles HTLC)    │
```

- The LSP will not pay unless the SE-registered hash matches the invoice.
- Alice cannot take the Lightning payment *and* keep the coin: settling reveals the unlock.
- If nothing settles, the transfer never completes and the coin stays alice's.

The **LSP is Spark's SSP role** — any party with a Lightning node and this SDK can serve it. The
reverse direction (receive on Lightning, pay with a coin) runs the same legs with roles swapped.

## What the SDK exposes

- `start_lightning_swap(counterparty, coin?) → {batch_id, payment_hash, coin}` — initiator leg.
- `get_swap_payment_hash(batch_id)` — counterparty verification against the SE.
- `settle_lightning_swap(swap) → preimage` — unlock + preimage reveal (settles hodl invoices).

BOLT11 invoice creation/payment stays in the LSP's Lightning node (LND etc.) — deliberately
outside the wallet's trust base. A turnkey LSP daemon bundling these legs with an LND backend is
roadmap (the repo's `lnd_docker`/token-lnd compose provides the harness).

## Difference from Spark

Spark splits the preimage across operators with verifiable secret sharing (needed because any
single operator could otherwise collude). With one SE the preimage simply lives at the SE —
same trust unit as the rest of the layer, one fewer moving part.
