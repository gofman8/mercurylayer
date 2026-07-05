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

## The SSP swap (real BOLT11 over rgb-lightning-node)

The turnkey path is the **SSP** (`mercury_spark_sdk::ssp`): a statechain wallet + an
rgb-lightning-node (RLN) daemon that pays/receives real BOLT11 invoices. The user side is two
calls, and they take **any** `impl Ssp` — a local `SspService` (embedded, tests) or a remote
`SspClient` (HTTP to a deployed `mercury-ssp` server). Same code, local or production:

```rust
use mercury_spark_sdk::ssp::SspClient;

let ssp = SspClient::new("https://ssp.example.com");     // remote SSP over HTTP

// PAY (Mercury -> Lightning): mint the exact coin, latch it to the invoice hash, hand it over,
// the SSP pays. Returns the preimage — cryptographic proof of payment.
let preimage = wallet.pay_lightning_invoice(&ssp, bolt11).await?;

// RECEIVE (Lightning -> Mercury): get a BOLT11 to hand to a payer; when paid, the SSP releases a
// coin and your background watcher claims it (a WalletEvent::TransferClaimed fires).
let swap = wallet.create_lightning_invoice(&ssp, 20_000).await?;   // swap.invoice -> payer
```

Both directions are the **HTLC preimage swap** on the latch (external-hash variant): the coin is
latch-bound to the invoice's payment hash, and the LN preimage is simultaneously the user's proof
of payment and the SSP's key to unlock the coin. Neither side can cheat.

**Failure is trustless.** If a pay never settles (no route, SSP declines), the SSP reveals no
preimage and never claims — the coin stays yours. Once the SE `batch_timeout` elapses, reclaim it:

```rust
match wallet.pay_lightning_invoice_reclaimable(&ssp, bolt11).await {
    Ok(preimage) => { /* paid */ }
    Err((coin_id, _e)) => {
        // ...after the batch_timeout window...
        wallet.reclaim_lightning_payment(&coin_id).await?;   // full custody back
    }
}
```

> ⚠️ A client-side **timeout** talking to a remote `SspClient` is NOT proof of failure — the SSP may
> have paid. Verify via the preimage before reclaiming, or you risk the SSP claiming the coin *and*
> you reclaiming it.

The SSP **pre-payment gate** (SPEC/review C2/C3) protects the SSP: before paying it verifies every
latched coin is a pending transfer *addressed to it* and worth ≥ invoice+fee — a coin sent
elsewhere or an undersized coin is refused before any Lightning money moves (`sdk20`).

Tests: `sdk05` (pay), `sdk06` (receive), `sdk18` (pay-failure + reclaim), `sdk19` (receive-failure),
`sdk20` (adversarial gate), `sdk21` (remote `SspClient`, both directions).

> **Known hazard (server-side, H4).** Two clocks govern a latch: the batch atomicity window
> (`batch_timeout`, ~120s) and the external-latch row's `expires_at` (~25h). A Lightning payment
> settling *between* those reveals a valid preimage that `unlock_by_preimage` still accepts while the
> receiver can no longer claim within the batch window — "paid but not received". The SDK surfaces
> and bounds this (reclaim only after `batch_timeout`), but the reconciliation is a **server-side
> fix** (collapse to one authoritative latch clock); flagged to the SE team, out of SDK scope.

**RGB assets over Lightning** (colored channels / asset invoices) are a scoped follow-up: the RLN
backend supports them, but the SDK/SSP swap path is sats-only today. Reuses the same `Ssp` /
`SspClient` seam (planned `sdk23`).

### Before exposing a public SSP (production hardening)

The `mercury-ssp` server is fine as a single-tenant / testnet service but is NOT yet safe to expose
on the internet (adversarial-review findings, all server-side):

- **Auth + rate-limit.** `/pay` and `/receive` are unauthenticated and unthrottled — a fund-moving
  surface open to any caller. `/receive` in particular commits SSP capital *before* the payer is on
  the hook (it splits + latches a coin and opens a HODL invoice per call), so an anonymous loop is a
  liquidity-griefing DoS. Put the server behind an API key / signed-request guard + a rate limiter,
  bind it to localhost behind a reverse proxy, and only commit the coin once the payer's HTLC is
  pending.
- **Idempotency.** `/pay` has no per-`batch_id` dedup; concurrent/duplicate calls waste worker slots
  and can double-attempt. Serialize per batch and cache the preimage; add a `/pay/status?batch_id`
  endpoint so a client that timed out re-queries instead of reclaiming (this is the real fix for the
  reclaim-ambiguity hazard — see `reclaim_lightning_payment`'s SAFETY note).
- **Proper HTTP status codes.** The server answers `200 {"error":..}` on failures, so edge
  rate-limiters/WAFs and monitoring can't see abuse. Return 4xx/5xx; the SDK's `SspClient` already
  keys on status first, so this is a server-only change.

## Difference from Spark

Spark splits the preimage across operators with verifiable secret sharing (needed because any
single operator could otherwise collude). With one SE the preimage simply lives at the SE —
same trust unit as the rest of the layer, one fewer moving part.
