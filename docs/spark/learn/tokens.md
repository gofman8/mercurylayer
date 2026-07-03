# Tokens on RGB

## Hello RGB

The token standard here is **RGB**, not a server-side token ledger. An RGB asset is a
client-validated contract: issuance and every transfer are cryptographic state transitions
committed inside Bitcoin transactions (tapret/opret), validated by the *receiving wallet* from a
**consignment** (the transition history). Nobody — not the SE, not an indexer — is trusted for
token state.

On this layer, allocations live on **statechain coins and off-chain sub-coins**, so token
payments inherit everything sats have: instant off-chain transfers, exact amounts via colored
splits, branch-verified receiving, and unilateral exit.

| Spark BTKN | Here |
|---|---|
| `createToken` (name, ticker, decimals, maxSupply) | `issue_token` — RGB NIA, full supply at issuance |
| `mintTokens` | NIA: supply fixed at issuance. IFA (inflatable) assets support inflation — roadmap |
| `transferTokens` | `transfer_tokens` — colored off-chain split + handover |
| `batchTransferTokens` | loop of `transfer_tokens` (batch API roadmap) |
| `freezeTokens` / `unfreezeTokens` | **N/A by design** — see below |
| `burnTokens` | send-to-unspendable (NIA) / IFA burn — roadmap |
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

## Why there is no freeze

Spark tokens can be issuer-frozen because operators enforce token state. RGB is
**client-validated**: token state has no central enforcement point, so an issuer freeze list has
no consensus meaning — a "frozen" holder's transfers would still validate for any receiver that
doesn't honour the list. We treat this as a *feature of the trust model* (tokens behave like
bearer instruments) and intentionally do not fake a freeze. Issuers needing freeze semantics
should not issue on client-validated rails.

## Exits with tokens

Cooperative or unilateral, the coin's exit materializes the branch on-chain; the RGB anchors
confirm with it and the allocation becomes an ordinary on-chain RGB holding, spendable with any
rgb-lib wallet.
