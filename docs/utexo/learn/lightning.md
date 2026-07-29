# Lightning

Lightning interop uses the **Mercury lightning latch** — the same preimage-swap shape Spark runs
through its SSPs, on a single SE.

Both directions run **on the TES-R ladder**. There is one protocol: `claim()` ladders every fresh
confirmed root coin, and Lightning pays and receives out of those laddered coins — an exact-amount
coin is latched whole, any other amount goes through an in-ladder split. There is no separate
Lightning lane and no protocol flag to choose.

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
roadmap (the repo's `lnd_docker`/token-lnd compose provides the harness). The raw latch primitive
(latch-locked transfer → unlock by preimage) is pinned by `tb04`.

## The SSP swap (real BOLT11 over rgb-lightning-node)

The turnkey path is the **SSP** (`mercury_utexo_sdk::ssp`): a statechain wallet + an
rgb-lightning-node (RLN) daemon that pays/receives real BOLT11 invoices. The user side is two
calls, and they take **any** `impl Ssp` — a local `SspService` (embedded, tests) or a remote
`SspClient` (HTTP to a deployed `mercury-ssp` server). Same code, local or production:

```rust
use mercury_utexo_sdk::ssp::SspClient;

let ssp = SspClient::new("https://ssp.example.com");     // remote SSP over HTTP

// PAY (Mercury -> Lightning): latch a coin to the invoice hash, hand it over, the SSP pays.
// An exact-amount coin is latched whole; otherwise the wallet splits IN-LADDER and latches the
// piece. Returns the preimage — cryptographic proof of payment.
let preimage = wallet.pay_lightning_invoice(&ssp, bolt11).await?;

// RECEIVE (Lightning -> Mercury): get a BOLT11 to hand to a payer; when paid, the SSP releases a
// coin and your background watcher claims it (a WalletEvent::TransferClaimed fires).
let swap = wallet.create_lightning_invoice(&ssp, 20_000).await?;   // swap.invoice -> payer
```

Both directions are the **HTLC preimage swap** on the latch (external-hash variant): the coin is
latch-bound to the invoice's payment hash, and the LN preimage is simultaneously the user's proof
of payment and the SSP's key to unlock the coin. Neither side can cheat.

**Exact and non-exact, both on the ladder.** Real invoices are arbitrary amounts, so each direction
has two shapes and the SDK picks between them for you:

- **PAY.** If the wallet can produce a coin worth exactly invoice+fee it is latched **whole**
  (`sdk63`). Otherwise `pay_lightning_invoice` routes to the in-ladder lane
  (`pay_lightning_invoice_inladder`): the laddered coin is split **in-ladder** into a PIECE that pays
  the SSP and a CHANGE that stays yours, and only the piece is latched (`sdk65`). The split is
  un-broadcast, so a failed payment costs you nothing (`sdk66`).
- **RECEIVE.** The SSP fronts an exact coin when it has one (`sdk64`); when it holds only a large
  laddered coin it splits in-ladder and conveys a piece worth the invoiced amount (`sdk67`).
  `settle_receive` is the same call either way.

> One-call PAY used to be unusable here (defect D3): it minted its coin via `ensure_exact_coin`,
> which cannot split a laddered coin exactly, so it refused *every* laddered coin — i.e. every coin.
> It now falls back to the in-ladder lane, the same way the receive side always has.

The latched in-ladder piece is the **one** coin the system deliberately terminalizes
(`set_spend_budget(piece, 0)`): it sits unclaimed until a Lightning preimage lands, which is exactly
the window the temporary pending-transfer lock does not cover, so the SE is told to co-sign nothing
further over it. Everywhere else children stay non-terminal and first-class; your parent and change
are untouched by this carve-out.

On **receive**, the SSP's invoice is a real **HODL invoice** from the rgb-lightning-node fork:
passing an external `payment_hash` to `/lninvoice` makes it `InvoiceType::Hodl`, so the payer's
incoming HTLC is *held* (not auto-settled) until the SSP has released the coin and can settle with
the SE preimage via `/claimhodlinvoice`. The full HODL lifecycle is wired: **create** (external hash
→ hold), **settle** (`settle_receive` → `/claimhodlinvoice`), and **abort** (`cancel_receive` →
`/cancelhodlinvoice`, which fails the held HTLC back to the payer as an immediate refund and closes
the invoice — used when a swap is abandoned *before* the coin is released, so the SSP never gives up
the coin and the refund without also being paid).

**Failure is trustless.** If a pay never settles (no route, SSP declines), the SSP reveals no
preimage and never claims — the coin stays yours. Once the SE `batch_timeout` elapses, reclaim it:

```rust
match wallet.pay_lightning_invoice_reclaimable(&ssp, bolt11).await {
    Ok(preimage) => { /* paid */ }
    Err((coin_id, _e)) => {
        // ...after the batch_timeout window...
        wallet.reclaim_lightning_payment(&coin_id).await?;   // custody back
    }
}
```

> ⚠️ A client-side **timeout** talking to a remote `SspClient` is NOT proof of failure — the SSP may
> have paid. Verify via the preimage before reclaiming, or you risk the SSP claiming the coin *and*
> you reclaiming it.

Reclaim on a laddered coin is a **local** restore, not a self-transfer: the failed latch already
co-signed an orphan `S'`, so re-transferring would trip the receiver's own census.
`reclaim_lightning_payment` therefore just marks the coin exitable again — the full value is
recoverable, the ladder is intact, and off-chain re-transfer resumes after a `refresh()` re-anchor
(`sdk68`). On the non-exact lane there is nothing to reclaim at all: the split is un-broadcast, so a
failed pay **rolls back** to the whole parent, piece and change dropped (`sdk66`) — which is why the
call above hands back no coin id on that path.

The SSP **pre-payment gate** (SPEC/review C2/C3) protects the SSP: before paying it verifies every
latched coin is a pending transfer *addressed to it* and worth ≥ invoice+fee — a coin sent
elsewhere or an undersized coin is refused before any Lightning money moves (`sdk20`). On top of
that it runs the receiver's **ladder census** before the irreversible Lightning leg
(`peek_pending_transfers` → `ladder_census_ok`, enforced in `execute_pay`): `verify_bundle` for a
whole coin, `verify_conveyed_child` for an in-ladder piece, comparing `num_sigs` against the
enclave's authoritative sig-count so a sender who co-signed a lower-CSV state omitted from the
conveyed ladder is caught. It fails **closed** — an unreadable or mismatched ladder means no
payment. `sdk37` pins that the value the gate prices against is the ladder-committed one (the piece's
exit-reachable value), never an attacker-supplied hint.

Tests: `sdk63` (PAY, exact coin latched whole), `sdk65` (PAY, non-exact via in-ladder split),
`sdk64` (RECEIVE, exact), `sdk67` (RECEIVE, non-exact via in-ladder split), `sdk68` (exact-PAY
failure → reclaim), `sdk66` (non-exact-PAY failure → rollback to the whole parent), `sdk19`
(receive never paid), `sdk24` (receive aborted → HODL cancel refunds the payer), `sdk25`
(delayed-claim attacker vs the coordinated latch clock), `sdk20` (adversarial pre-payment gate),
`sdk37` (what value the gate reads), `sdk21` (remote `SspClient`, both directions), `tb04` (the raw
latch primitive), `sdk53` (the old refusal guard on latching a laddered coin is gone).

> **H4, the two-clock hazard — the receiver half is FIXED, the sender half is why the ⚠️ above
> matters.** Two clocks govern a latch: the batch atomicity window (`batch_timeout`, ~120s) and the
> latch row's own `expires_at`. A Lightning payment settling *between* them used to reveal a valid
> preimage that `unlock_by_preimage` still accepted while the receiver could no longer claim within
> the batch window — "paid but not received". The SE now gates an LN-latch batch's claim window on
> the **latch's** expiry instead of `batch_timeout` (`validate_batch`, server
> `transfer_receiver.rs`), with a grace period so the SSP always has room to settle the HTLC after
> the receiver's last possible claim; `sdk25` pins that a receiver who deliberately stalls past the
> window gets nothing and the payer is refunded. What is *not* collapsed is the sender's side: the
> co-sign lock (`has_open_transfer`) still lapses at `batch_timeout`, so a reclaim can succeed while
> the SSP could still legitimately claim — which is exactly why reclaim requires positive proof of
> non-payment rather than a timeout.

## RGB assets over Lightning

The swap also moves **RGB assets**, not just sats — statechain RGB ⇄ Lightning RGB, both ways, over
rgb-lightning-node **colored channels**. The user API is unchanged; the SSP bridges the two rails:

```rust
// PAY: an RGB Lightning invoice, paid from a statechain RGB coin.
let preimage = wallet.pay_lightning_invoice(&ssp, rgb_bolt11).await?;   // asset auto-detected
// RECEIVE: get an RGB invoice; when paid, an asset-carrying coin lands on your statechain.
let swap = wallet.create_lightning_invoice_asset(&ssp, &asset_id, 250).await?;
```

Mechanics: `RlnClient::decode` reads the invoice's `asset_id`/`asset_amount`; for a PAY the wallet
hands the SSP a **colored coin** batch-locked to the invoice hash (`latch_tokens` — the RGB
consignment rides the transfer message), the SSP pays the asset over its colored channel, and the LN
preimage unlocks the coin (the SSP now holds the asset on statechain). For a RECEIVE the SSP
colored-transfers an asset coin to the user under an **SE-held preimage** (`latch_tokens_se_preimage`)
and issues an RGB **HODL invoice** (same fork mechanism as the sats path — external payment hash →
`InvoiceType::Hodl`); when the payer pays the asset over LN, the HTLC is held until the SSP releases
the coin and claims it via `/claimhodlinvoice` (or refunds via `/cancelhodlinvoice` on abort). The
SSP stays asset-neutral (it converts asset units between the rails, minus fee).

An RGB **carrier** coin is deliberately **never laddered** — a plain tier spend would destroy the
allocation (terminal-freeze, `PROTOCOL.md` §5.10, pinned by `sdk52`) — so the colored lane rides the
signed-once backup shape and transfers by backup-chain handover. That is a live, load-bearing shape
under the one protocol, not an older one: only the sats lanes ladder.

The pre-payment gate keeps the C2 recipient check for both; the sats C3 amount check applies to sats
invoices. For RGB the gate now validates the consignment **before** paying (`validate_pending_token`
derives the contract id and the amount the consignment cryptographically assigns to the coin, not the
attacker-controlled envelope hint), so a wrong-asset or undersized colored coin is refused while the
Lightning money is still ours; the post-claim balance-delta check remains as a backstop. This had to
move pre-payment because a HODL swap forces the SSP to pay before it can claim. Tests: `sdk23` drives
the full colored-channel asset payment (issue → colored channel → asset invoice → decode → pay →
balance shift) via `RlnClient`; `sdk37` part [4] pins the pre-payment consignment validation.

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
