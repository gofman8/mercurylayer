# Lightning

Lightning interop is a **HODL-invoice latch**: a statechain coin is transferred under a `batch_id`
whose claim is held until a SHA256 preimage lands, and that same preimage settles a Lightning HTLC.
Both directions, both amount shapes, running over the pinned `rgb-lightning-node` `pr-90` (LDK 0.2.2)
fork. No new cryptographic primitive.

Both directions run **on the TES-R ladder**. `claim()` ladders every fresh confirmed root coin, and
Lightning pays and receives out of those laddered coins — an exact-amount coin is latched whole, any
other amount goes through an in-ladder split. There is no separate Lightning lane and no protocol
flag to choose. One configuration caveat is load-bearing here: laddering needs a **pinned enclave
attestation identity**, no network compiles one in, and neither `SdkConfig` constructor supplies one,
so an embedder that sets neither `SdkConfig::attestation_identity` nor `UTEXO_ATTESTATION_IDENTITY`
gets flat coins — and an un-laddered coin is not payable over Lightning at all (see the census
below). [`../spec/SPEC.md`](../spec/SPEC.md) §0.4 row V-6 registers it. The normative account of the
latch is [`../spec/LIGHTNING.md`](../spec/LIGHTNING.md).

## The latch

The SE holds a per-batch secret and gates the claim on it. Two flavours ship, one per direction:

| | SE-minted preimage | External hash |
|---|---|---|
| Client call | `mercuryrustlib::lightning_latch::create_pre_image` | `create_external_hash_latch` |
| SE endpoint | `post_paymenthash` | `post_paymenthash_external` |
| Who learns the preimage | the SE mints it and releases it via `get_preimage` once the latch is unlocked | nobody at the SE — it stores only `H` and learns the preimage when `unlock_by_preimage` presents it |
| Used by | RECEIVE (LN → coin) | PAY (coin → LN) |
| Latch lifetime | 3 000 s default, `RECEIVE_LATCH_TIMEOUT`-configurable | 90 000 s |

Either way the coupling is the same shape: the counterparty will not move Lightning money unless the
SE-registered hash matches the invoice, and the coin cannot be claimed without the preimage that
settles the HTLC.

**The direct, peer-to-peer form** — no SSP, no BOLT11 handling in the wallet:

```
alice (has coin)                LSP (runs a Lightning node)
   │ start_lightning_swap(lsp_addr, None)
   │  SE: fresh preimage; hash H
   │  coin → LSP, latch-locked ───▶│ get_swap_payment_hash(batch_id) == H ✓
   │                               │ pays alice's invoice locked to H (HTLC)
   │ invoice paid ✓                │
   │ settle_lightning_swap ───────▶│ claim unlocks → LSP owns the coin
   │  ← preimage (settles the HTLC)│
```

- `start_lightning_swap(counterparty_address, statechain_id: Option<String>) -> LightningSwap`
  (`{batch_id, payment_hash, statechain_id}`) — picks the largest confirmed coin when none is named,
  and **never** an RGB carrier: handing a carrier over as plain BTC destroys the allocation, so it
  fails closed if RGB state cannot be read.
- `get_swap_payment_hash(batch_id) -> Option<String>` — the counterparty's check against the SE.
- `settle_lightning_swap(&swap) -> String` — `confirm_pending_invoice` then `retrieve_pre_image`, in
  one call.

Alice cannot take the Lightning payment *and* keep the coin: settling is what reveals the unlock. If
nothing settles, the transfer never completes and the coin stays hers. `tb04` pins the raw primitive.

## The SSP — real BOLT11 over rgb-lightning-node

The turnkey path is the **SSP** (`mercury_utexo_sdk::ssp`): a statechain wallet plus an
rgb-lightning-node (RLN) daemon that pays and receives real BOLT11 invoices. The SSP is Spark's SSP
role; any party with a Lightning node and this SDK can serve it.

The user side is two calls, and both take **any** `impl Ssp` — a local `SspService` (embedded, in
process, what the tests drive) or a remote `SspClient` (HTTP against a deployed `mercury-ssp`
server). The trait is four methods, `info` / `quote_pay` / `execute_pay` / `create_receive`, and the
local implementation forwards to `SspService`'s inherent methods verbatim, so the two are the same
code path either way.

```rust
use mercury_utexo_sdk::ssp::SspClient;

let ssp = SspClient::new("https://ssp.example.com");     // remote SSP over HTTP

// PAY (Mercury -> Lightning). Returns the preimage — cryptographic proof of payment.
let preimage = wallet.pay_lightning_invoice(&ssp, bolt11).await?;

// RECEIVE (Lightning -> Mercury). Hand `swap.invoice` to the payer; when it is paid the SSP
// releases a coin and your background watcher claims it (WalletEvent::TransferClaimed).
let swap = wallet.create_lightning_invoice(&ssp, 20_000).await?;
```

BOLT11 creation and payment stay inside the SSP's Lightning node — deliberately outside the wallet's
trust base. `sdk21` drives both directions against a remote `SspClient`.

## Exact and non-exact, both on the ladder

Real invoices are arbitrary amounts, so each direction has two shapes and the SDK picks between them.

**PAY.** `pay_lightning_invoice` delegates to `pay_lightning_invoice_reclaimable`, which quotes,
then tries `ensure_exact_coin(amount + fee)`. If that succeeds the whole coin is latched with
`create_external_hash_latch` and conveyed (`sdk63`). If it cannot mint an exact coin — and it cannot
split a laddered coin exactly — it falls back to `largest_laddered_coin_for_pay` →
`pay_lightning_invoice_inladder`, which splits the parent **in-ladder** into a PIECE that pays the
SSP and a CHANGE that stays yours, and latches only the piece (`sdk65`). The piece is oversized by
`IN_LADDER_TIER_RESERVE` = 2 000 sats, because the piece's own exit burns two tier fees before it
pays anything; the surplus accrues to the piece, and the SSP is priced on what the census reads, not
on the nominal.

Without that fallback the one-call API would refuse every laddered coin — i.e. every coin
([SPEC §8.1](../spec/SPEC.md) REQ-42).

**RECEIVE.** `create_lightning_invoice` hands the SSP your address; `SspService::create_receive`
fronts an exact coin when `ensure_exact_coin` can mint one (`sdk64`), and otherwise takes
`largest_laddered_coin` and splits it in-ladder, conveying a piece worth the invoiced amount plus its
own 2 000-sat tier reserve — which the SSP bears, as its cost of fronting a non-exact amount
(`sdk67`). The latch there is `InLadderLatch::ClassicMinted`, the SE-minted-preimage flavour.
`settle_receive` operates on the piece's statechain id and is the same call either way.

**The latched piece is the one coin the system deliberately terminalizes.** It sits unclaimed until a
Lightning preimage lands, and the temporary pending-transfer lock cannot cover that window
indefinitely: for a batch transfer the lock runs to the latch's own expiry, and a latch that expires
unsettled leaves the piece conveyed and unclaimed with the lock lapsed — the receiver can never
complete the handover, and the sender is free again. So `in_ladder_pay` calls
`set_spend_budget(piece_sid, 0)` in its `[LN carve-out]` block:
the SE will co-sign nothing further over that piece, closing the post-expiry rival window
permanently ([SPEC §8.2](../spec/SPEC.md) INV-30). Ordering is load-bearing — `set_spend_budget`
authenticates with the piece's own auth key, so it must run *before* conveyance, while the sender
still holds it. Everywhere else children stay non-terminal and first-class
([`../spec/CHILDREN.md`](../spec/CHILDREN.md)); the parent and the change slice are untouched.

## HODL on the receive side

On RECEIVE the SSP's invoice is a real **HODL invoice** from the fork: passing an external
`payment_hash` to `/lninvoice` makes it `InvoiceType::Hodl`, so the payer's incoming HTLC is *held*
rather than auto-settled. The full lifecycle is wired through `RlnClient`:

* **create** — `ln_invoice` (sats) or `ln_invoice_asset` (RGB), each passing the latch's hash as
  `payment_hash` with a 3 600 s expiry — comfortably above the 3 000 s latch, which is the point;
* **settle** — `settle_receive` waits for `invoice_status` to reach `Claimable` (money locked at the
  SSP's node), *then* calls `confirm_pending_invoice` to release the coin, *then* retrieves the SE
  preimage, *then* `claim_hodl` → `/claimhodlinvoice`;
* **abort** — `cancel_receive` → `cancel_hodl` → `/cancelhodlinvoice`, failing the held HTLC back to
  the payer as an immediate refund and closing the invoice. Valid only *before* the coin is released;
  once it is released the SSP must claim, not cancel.

The ordering is the safety property: the SSP owns the coin throughout its risk window and the HTLC is
held before release, so **neither side can be robbed**. RECEIVE needs no operator trust for safety
(`sdk19` unpaid, `sdk24` aborted-and-refunded, `sdk25` delayed claim).

## The SSP's pre-payment gate

PAY is where the trust actually sits, because the Lightning leg is irreversible. Before
`send_payment`, `SspService::execute_pay` runs four checks in order, all fail-closed:

1. **Hash binding.** `get_payment_hash(batch_id)` must equal the decoded invoice's payment hash.
2. **Addressed to us.** `get_statechain_ids_by_batch_id` names the latched coins; every one must
   appear in `peek_pending_transfers`, which only returns transfers this wallet can **decrypt with
   its own auth key** — so membership is proof of address, not a claim.
3. **Ladder census.** Each pending entry carries `ladder_census_ok`, computed by
   `peek_pending_transfers` itself. There is no shape that passes trivially. `protocol_version` is an
   exact-set **shape selector**, not an ordinal (`ADMISSIBLE_PROTOCOL_VERSIONS = [0, 2, 4]`,
   enforced by `admissible_shape`), and the census dispatches on it:
   * **2**, a root-ladder conveyance → `prepay_flat_census`, whose every step returns `Err` and whose
     only caller maps `Err` to `ladder_census_ok = false`; there is no success path that skips a
     check. Four bindings, all required: the funding output must have been read **from the chain**
     (a branch-supplied, un-broadcast `tx0` is sender-controlled and binds nothing);
     `verify_flat_backup_lane`, so a coloured backup a prior owner still holds over `F` cannot be
     conveyed as a flat one; the owner-exit binding (`bundle.owner_exit_address == my_backup`), so a
     perfectly coin-bound ladder that pays a third party on exit cannot take the SSP's Lightning
     money; and `verify_bundle_bound`, which pairs the coin binding — statechain id, funding
     outpoint, on-chain value, the on-chain aggregate scriptPubKey, and the coordinator's recorded
     `se_aggregate_pubkey` for that sid (absent ⟹ reject) — with the exact-equality count
     `num_sigs == flat_backups + tiers + disclosed superseded` against the
     **enclave-authoritative** sig-count, so a hidden lower-CSV `S*` inflates the count and the
     census fails, and a self-signed decoy ladder over an attacker-controlled outpoint cannot pass
     by being merely self-consistent.
   * **4**, a child conveyance → `verify_conveyed_child`, bound to the *latched* sid, not already
     adopted, over a live on-chain parent root. Its census-bound exit value — never a
     sender-declared field — is what the value gate then prices against.
   * **0**, the un-laddered carrier lane, and anything outside the set → refused. An un-laddered
     branch coin is **not payable over Lightning**, and that is intended, not an omission.

   A refusal carries `ladder_census_refusal`, the sentence naming *which* check refused, so an
   operator is not handed a six-way disjunction.
4. **Value.** For sats, `check_latched_coins` requires the summed census-bound amounts to cover
   invoice + fee. For an RGB invoice, `validate_pending_token_ex` derives the contract id and the
   amount the consignment **cryptographically assigns** to the coin — never the attacker-controlled
   envelope hint — and both a wrong asset and an undersized one are refused while the Lightning money
   is still the SSP's. A post-claim balance-delta check remains as a backstop.

`sdk20` drives the adversarial gate; `sdk37` pins what value it reads — the ladder-committed,
exit-reachable value, and the pre-payment predicate being the *same shared function* the claim path
runs, so the gate can never be weaker than the check that later books the coin.

## Failure and rollback

If a pay never settles the SSP reveals no preimage and never claims, so the coin is still yours.
Recovery differs by lane, because the two lanes leave different residue:

* **Non-exact PAY** (`sdk66`): the split is un-broadcast, so `pay_lightning_invoice_inladder` calls
  `rollback_inladder_split` — the optimistic booking is dropped, the piece and change disappear, and
  you recover the **whole parent**. There is nothing to reclaim, which is why the reclaimable API
  hands back no coin id on that path.
* **Exact whole-coin PAY** (`sdk68`): the failed latch already co-signed an orphan `S'`, so a
  self-transfer reclaim would leave the disclosed ladder one tier short of the enclave sig-count and
  `verify_bundle` would reject it forever. `reclaim_lightning_payment` detects the ladder and
  restores the coin **locally** as exitable instead. The value is intact and recoverable through
  `unilateral_exit`; off-chain re-transfer stays orphan-bricked until a `refresh()` re-anchor.

`Err` is `(coin_statechain_id, error)`, and the id is the **empty string** when nothing was latched —
a quote or mint failure, and the non-exact lane, which has already rolled its own split back. Branch
on it; there is no `Option`:

```rust
match wallet.pay_lightning_invoice_reclaimable(&ssp, bolt11).await {
    Ok(preimage) => { /* paid */ }
    Err((coin_id, e)) if coin_id.is_empty() => { /* nothing latched — nothing to reclaim */ }
    Err((coin_id, _e)) => {
        // ...only after POSITIVELY confirming the SSP did not pay...
        wallet.reclaim_lightning_payment(&coin_id).await?;
    }
}
```

> **A client-side timeout is NOT proof of failure.** The SSP may have paid. Two error shapes must
> never auto-trigger a reclaim: a transport timeout against a remote `SspClient`, and an
> `execute_pay` error raised *after* the preimage was revealed (the SSP's watcher will still claim).
> On a laddered coin the reclaim is a purely local status restore — it does not consult the SE — so
> nothing stops you from marking a coin yours that the SSP legitimately owns, and then racing it
> on-chain. Confirm non-payment (no preimage exists for the invoice hash) before calling it.

## The clocks

Three gates share one clock, and the arrangement is what makes the swap atomic:

* **The receiver's claim window.** `validate_batch` (`server/src/endpoints/transfer_receiver.rs`)
  gates an LN-latch batch on `lightning_latch.expires_at` minus a grace period
  (`RECEIVE_LATCH_GRACE`, 300 s default) rather than on the short `batch_timeout`, so the SSP always
  has room to settle the HTLC after the receiver's last possible claim.
* **The SSP's payout.** `get_preimage` (`server/src/database/lightning_latch.rs`) releases only on
  `locked = false AND expires_at > now()` — the same latch clock, right up to expiry.
* **The sender's co-sign lock.** `has_open_transfer` (`server/src/database/transfer_sender.rs`)
  holds through `OPEN_TRANSFER_WINDOW_SQL`: a non-batch transfer keeps a one-hour rule, while a
  batch transfer stays open for `MAX(lightning_latch.expires_at)` — deliberately *without*
  subtracting the claim gate's grace, because the lock must outlive the claim window and never the
  reverse. `MAX` rather than one row, for the same reason: a `batch_id` spans one latch row per
  statechain id, and holding until the last of them is the safe direction. On the non-batch branch
  this window is measured: `sdk91` skips the client and POSTs `/sign/first` with the payer's own
  still-valid owner credential, and gets **HTTP 409** while the row is inside the hour and **HTTP
  200 with a server pubnonce** once it is older — so that window is the only *server*-side gate on
  that path, and `sdk90` shows the two gates that stop an honest client are both local.

The latch expiry is set shorter than the payer's HODL HTLC, so either the receiver completes in
window and the SSP can settle, or both fail and the payer is refunded. `sdk25` pins it live: a
receiver who deliberately stalls past the window gets nothing and the payer is refunded.

**What is open.** Nothing *binds* the latch expiry to the payer's HTLC CLTV. The designed mitigation
— a `statechain_transfer.lock_expiry` read by `has_open_transfer` and both sign gates, enforcing
`lock_expiry ≥ HTLC CLTV + grace` — is **not built**, and no test pins the invariant. The risk is
bounded in practice because the CLTVs in use sit inside the window, but it is asserted nowhere. It is
the top open item of this design ([`../spec/LIGHTNING.md`](../spec/LIGHTNING.md) §9 residual 3).

**Also designed, not built:** additionally requiring `key_updated = true` before `get_preimage`
releases, binding the SSP's payout to the coin actually being delivered. It must never be applied
globally — `settle_lightning_swap` retrieves the preimage *before* the receiver completes, so a
global gate deadlocks that lane (settle needs the preimage → the preimage needs `key_updated` →
`key_updated` needs the batch unlocked → which settle itself provides). RECEIVE rests on the
coordinated-clock atomicity above instead, which stands on its own evidence.

## RGB assets over Lightning

The latch also carries **RGB assets**, over rgb-lightning-node **colored channels**. The user API is
unchanged; the SSP bridges the two rails and stays asset-neutral (it converts units between rails,
minus fee).

```rust
// Both RGB lanes need a LOCAL SspService — the remote HTTP surface is sats-only (see below).
let ssp: SspService = /* embedded SSP wallet + RlnClient */;

// PAY: an RGB Lightning invoice, paid from a statechain RGB coin — asset auto-detected.
let preimage = wallet.pay_lightning_invoice(&ssp, rgb_bolt11).await?;
// RECEIVE: takes &SspService concretely, not &impl Ssp.
let swap = wallet.create_lightning_invoice_asset(&ssp, &asset_id, 250).await?;
```

`RlnClient::decode` reads the invoice's `asset_id` / `asset_amount` off `decodelninvoice`; a quote
carrying them routes PAY through `latch_tokens`, which hands the SSP a **colored coin** batch-locked
to the invoice hash with the consignment riding the transfer message. RECEIVE goes through
`latch_tokens_se_preimage` + `SspService::create_receive_asset`: the SSP colored-transfers an asset
coin to the user under an SE-held preimage and issues an RGB HODL invoice on that hash, so releasing
the coin is a precondition of the SSP taking the Lightning asset.

Four boundaries are worth knowing before building on this, and the first one gates the rest:

* **RGB PAY needs a LADDERED carrier, which the shipped configuration does not produce.** A carrier
  is deliberately not laddered when `SdkConfig::colored_ladder` is `false` — a plain tier spend would
  destroy the allocation (terminal freeze, [`../spec/PROTOCOL.md`](../spec/PROTOCOL.md) §5.10, pinned
  by `sdk52`) — and `transfer_sender` conveys such a coin as `protocol_version` **0**, the
  un-laddered lane. `prepay_flat_census` refuses that shape at its version floor, so the SSP declines
  to pay after the carrier has already been conveyed. Reaching the PAY lane therefore means running
  with `colored_ladder` on, so the carrier holds a coloured ladder and conveys as shape 2. RECEIVE is
  the SSP's own coin and does not sit behind this gate. See [tokens.md](tokens.md).
* **Non-exact RGB PAY does not exist.** `pay_lightning_invoice_inladder` refuses an asset quote by
  name.
* **An envelope with nothing to resolve it against is refused, by name.** The pre-pay gate requires
  either a flat exit branch (`branch_txs`) or a coloured child's own witness chain
  (`child_witness_txids`) to validate the consignment before paying. With neither, the SSP cannot
  verify the allocation and a Lightning payment is irreversible, so it refuses rather than resting on
  an RGB resolver happening to fail. Latch a coloured carrier at the root instead.
* **RGB is local-SSP only, in both directions.** `create_lightning_invoice_asset` takes `&SspService`,
  not `&impl Ssp`. PAY is nominally generic over `impl Ssp`, but the `mercury-ssp` server's `/quote`
  handler emits only `amount_sats` / `fee_sats` / `payment_hash` / `ssp_address` — so an RGB invoice
  quoted through a remote `SspClient` comes back with `asset_id: None` and is treated as a sats
  invoice — and `/receive` has no asset variant at all. A remote asset endpoint is a follow-up.

**Status.** `sdk23` drives the Lightning half end to end through `RlnClient` — issue → colored
channel → asset invoice → decode → pay → balance shift — and `sdk37` part [4] pins the pre-payment
consignment validation on `validate_pending_token`, the flat-branch form of the same shared
predicate. A **full cross-rail swap is not built**; it remains a follow-up
([`../spec/LIGHTNING.md`](../spec/LIGHTNING.md) §9 residual 6).

## Before exposing a public SSP

The `mercury-ssp` server (`clients/apps/ssp-server`) is a Rocket app mounting exactly
`/info /quote /pay /receive`. It is fine as a single-tenant or testnet service and is **not** safe to
expose on the internet as it stands:

- **Auth and rate limiting.** `/pay` and `/receive` are unauthenticated and unthrottled — a
  fund-moving surface open to any caller. `/receive` in particular commits SSP capital *before* the
  payer is on the hook (it latches a coin and opens a HODL invoice per call, then spawns settlement
  in the background), so an anonymous loop is liquidity-griefing DoS. Put it behind an API key or
  signed-request guard plus a rate limiter, bind it to localhost behind a reverse proxy, and commit
  the coin only once the payer's HTLC is pending.
- **Idempotency.** `/pay` has no per-`batch_id` dedup, so concurrent or duplicate calls waste worker
  slots and can double-attempt. Serialize per batch, cache the preimage, and add a
  `/pay/status?batch_id` endpoint — that status query is the real fix for the reclaim ambiguity
  above, and `reclaim_lightning_payment`'s own safety note points at it.
- **HTTP status codes.** Failures return `200 {"error": …}` (the `err` helper), so edge rate
  limiters, WAFs and monitoring cannot see abuse. Return 4xx/5xx; `SspClient::request` already checks
  status before the body — and treats an `error` field or a non-object body as a failure either way —
  so this is a server-only change.

## Trust, in one paragraph

**RECEIVE needs no operator trust.** The SSP fronts its own coin, the inbound HTLC is held before the
coin is released, and the SE-minted preimage is released only inside a latch window that expires
before the payer's HODL HTLC.

**PAY rests on the existing statechain operator-trust model.** After the pre-pay census the SSP —
co-located with the coordinator and blind SE as one trust domain — holds a broadcastable
receiver-paying state `S'`, and the design trusts the operator not to broadcast it on a rolled-back
swap. That residual is the statechain's existing bar, not something Lightning introduces: serving
`S'` pre-pay is what lets the census run at all. The **untrusted counterparty cannot be robbed** (the
coin is frozen by the pending lock and `locked2` clears only on the genuine merchant preimage) and
**cannot rob the SSP** (the census reads the enclave-authoritative sig-count). Adaptor signatures
would remove the SSP-broadcast-on-rollback trust, but only on a PTLC-external lane — and PTLCs are
absent from the pinned LDK 0.2.2 stack, which is why this design uses HODL plus BOLT11 preimages and
nothing else. Full matrix: [`../spec/TRUST-MODEL.md`](../spec/TRUST-MODEL.md).

## Difference from Spark

Spark splits the preimage across operators with verifiable secret sharing, because any single
operator could otherwise collude. With one SE the preimage simply lives at the SE — the same trust
unit as the rest of the layer, one fewer moving part.

---

Tests: `sdk63` (PAY exact), `sdk65` (PAY non-exact), `sdk64` (RECEIVE exact), `sdk67` (RECEIVE
non-exact), `sdk66` (non-exact PAY failure → rollback to the whole parent), `sdk68` (exact PAY
failure → local reclaim), `sdk19` (receive never paid), `sdk24` (receive aborted → HODL cancel
refunds the payer), `sdk25` (delayed claim vs the coordinated latch clock), `sdk20` (adversarial
pre-payment gate), `sdk37` (what value the gate reads), `sdk21` (remote `SspClient`, both
directions), `sdk23` (RGB over colored channels), `sdk53` (a laddered coin can be latched), `tb04`
(the raw latch primitive).
