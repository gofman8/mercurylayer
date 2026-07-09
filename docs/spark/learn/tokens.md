# Tokens on RGB

> How partial token amounts work — colored splits, raw units vs precision, the 1500-sat piece,
> exit behaviour — is covered end-to-end in the [granularity deep dive](granularity-deep-dive.md).

## Hello RGB

The token standard here is **RGB**, not a server-side token ledger. An RGB asset is a
client-validated contract: issuance and every transfer are cryptographic state transitions
committed inside Bitcoin transactions (tapret/opret), validated by the *receiving wallet* from a
**consignment** (the transition history). Nobody — not the SE, not an indexer — is trusted for
token state.

On this layer, allocations live on **statechain coins and off-chain sub-coins**, so token
payments inherit everything sats have: instant off-chain transfers, exact amounts via colored
splits, branch-verified receiving, and a unilateral exit path (materialize the branch on-chain —
see [Exits with tokens](#exits-with-tokens); it is not the plain one-tx sweep sats get).

| Spark BTKN | Here |
|---|---|
| `createToken` (name, ticker, decimals, maxSupply) | `issue_token` (NIA, full supply at issuance) / `issue_inflatable_token` (IFA) |
| `mintTokens` | `mint_tokens` — IFA on-chain inflate, newly-minted allocation bound to a fresh statechain coin (NIA supply stays fixed); [SPEC §7](../SPEC.md#7-tokens-rgb) REQ-20, `sdk09` |
| `transferTokens` | `transfer_tokens` — colored off-chain split + handover |
| `batchTransferTokens` | `batch_transfer_tokens` — one colored split, N pieces + change ([SPEC §7](../SPEC.md#7-tokens-rgb), `sdk09`) |
| `freezeTokens` / `unfreezeTokens` | **N/A by design** — see below |
| `burnTokens` | `burn_tokens` — burns engine-held free balance on-chain; statechain-bound supply must exit first ([SPEC §7](../SPEC.md#7-tokens-rgb)) |
| token identifier (`btkn1…`) | RGB contract id (`rgb:…`) |

## Issuance

`issue_token(ticker, name, precision, supply)`:

1. issues the NIA contract in the wallet's RGB engine,
2. deposits the full supply onto a **fresh statechain coin** in one colored on-chain
   transaction (the only on-chain tx a token ever needs until exit),
3. registers the coin as the asset carrier.

From then on the supply moves off-chain.

## Transfer lifecycle

```
alice: 1000 TKN on coin C
alice.transfer_tokens(TKN, bob, 250)
  → colored split (off-chain): C → [piece: 250 TKN + 1500 sats][change: 750 TKN + rest]
  → consignment (proof of the 250-TKN assignment) attached to the transfer message
  → piece handed over to bob (key handover, branch included)
bob's watcher:
  → validates the branch (consensus) and the consignment (RGB, off-chain resolver)
  → books 250 TKN under the consignment's VERIFIED contract id
balances: alice 750 / bob 250 — zero on-chain footprint
```

If no single carrier holds the amount, the SDK **combines** several carriers of the asset (N
inputs → exact piece + change) in one SE-co-signed colored combine tx (`colored_combine_transfer`
in `tokens.rs`). This changes which coins are consumed and the shape of the receiver's branch: the
receiver validates the multi-input branch and requires **all N input carriers to be terminal**.

## Why there is no freeze

Spark tokens can be issuer-frozen because operators enforce token state. RGB is
**client-validated**: token state has no central enforcement point, so an issuer freeze list has
no consensus meaning — a "frozen" holder's transfers would still validate for any receiver that
doesn't honour the list. We treat this as a *feature of the trust model* (tokens behave like
bearer instruments) and intentionally do not fake a freeze. Issuers needing freeze semantics
should not issue on client-validated rails.

## Exits with tokens

A token-carrying coin refuses the plain exit operations: `withdraw` and `unilateral_exit` exclude
carriers from their defaults and hard-error when a carrier is named (an RGB-unaware sweep would
destroy the allocation), and the `auto_exit_due` watchtower skips them. Token exit means
**materializing the coin's branch on-chain** — today a manual broadcast of the stored branch rows
(or a co-descendant's exit doing it for free), not a one-call SDK operation: the RGB anchors
confirm with the branch and the allocation becomes an ordinary on-chain RGB holding, spendable
with any rgb-lib wallet. Onward movement of the settled allocation still needs the SE (no colored
unilateral path is shipped). Step-by-step:
[granularity deep dive §5.6](granularity-deep-dive.md); normative:
[GRANULARITY-SPEC](../GRANULARITY-SPEC.md) GRN-REQ-14 / GRN-INV-14.

## Tokens over time — holding, and doing nothing

*"If I issue or receive tokens and then do nothing for a year, do I lose them?"* No — tokens are
never lost by inactivity. But how you got them changes what still works (all verified end-to-end by
`SDK_E2E=32`, which idles the chain well past every backup-ladder horizon):

**Tokens you issued or minted (a *flat* carrier, funded on-chain).** The carrier's own backup
ladder floors after a horizon (~7 days on the deployed profile), but that does **not** stop you
sending: a token transfer is a colored *split*, and the piece it mints gets a **fresh** ladder
(`create_tx1`, `qt=0`), so you can keep sending forever with the SE. There is **no clawback risk**
(nothing sits above a flat carrier in the tree). The only limit: movement needs the SE — a *plain*
unilateral exit is refused (it would sweep the sats and destroy the allocation), so an issued
carrier's tokens stay anchored on-chain until the SE co-signs a colored spend. So: not lost, always
sendable cooperatively; SE-dependent to move without a colored exit path.

**Tokens you received (a *sub-coin* carrier).** Also not lost, and here you have a **SE-free**
option: the exit branch is locktime-zero, so broadcasting it *materializes* the allocation on-chain
any time — even a year later — as long as the shared root is still unspent. Two caveats: a lone
1,500-sat received piece is below the carrier floor, so it can't be re-sent on its own (hold,
combine with another piece, or exit); and there is a **real clawback danger with long inactivity**.
Past the root deadline (~7 days), the *sender's* own backup matures, and a malicious sender can
sweep the shared funding out from under you. You are safe only if you materialize before then — and
today the `auto_exit_due` watchtower **skips token carriers**, so received tokens have **no
automatic protection**. Treat a received off-chain token like any off-chain sub-coin: exit (or move)
it well before the deadline; don't leave it sitting for a year.

**Summary:** never lost; cooperative operations (with the SE) work throughout; a received token's
unilateral materialization works forever *if you don't miss the root deadline*.
