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

**A carrier is the un-laddered shape.** There is one protocol: `claim()` establishes a TES-R ladder
(trigger → extension → state, relative CSV, un-broadcast) for every fresh confirmed root coin. An RGB
**carrier** is the deliberate exception — it is *never* laddered, because a plain tier spend would
sweep the sats and destroy the allocation ([PROTOCOL.md §5.10](../PROTOCOL.md) rule 1, terminal-freeze;
pinned by `sdk52`, and re-checked after a year of idling by `sdk32`). A carrier instead keeps its
signed-once backup and moves by colored split + backup-chain handover. This is not a legacy lane — it
is the load-bearing shape for RGB, and it is why the calendar deadlines described under [Tokens over
time](#tokens-over-time--holding-and-doing-nothing) still apply to tokens while a plain laddered coin
has none.

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
destroy the allocation). Token exit means **materializing the coin's branch on-chain**: broadcasting
the stored branch rows (or a co-descendant's exit doing it for free) — the RGB anchors confirm with
the branch and the allocation becomes an ordinary on-chain RGB holding, spendable with any rgb-lib
wallet. Onward movement of the settled allocation still needs the SE (no colored unilateral path is
shipped). The `auto_exit_due` watchtower now does this **automatically** for a received carrier
nearing its clawback deadline (branch-only, emitting `TokenCarrierMaterialized`), so you no longer
have to materialize by hand to stay safe (SPEC §9.5 / REQ-33, `sdk34`). Step-by-step:
[granularity deep dive §5.6](granularity-deep-dive.md); normative:
GRANULARITY-SPEC GRN-REQ-14 / GRN-INV-14.

## Tokens over time — holding, and doing nothing

*"If I issue or receive tokens and then do nothing for a year, do I lose them?"* No — tokens are
never lost by inactivity. But how you got them changes what still works (all verified end-to-end by
`SDK_E2E=32`, which idles the chain well past every deposit-backup horizon):

**Tokens you issued or minted (a *flat* carrier, funded on-chain).** The carrier has **no ladder to
age on** — terminal-freeze keeps it off the T/X/S tiers entirely (`sdk32` asserts `tesr::load → None`
at issuance *and* after `initlock + 500` idle blocks). What it does have is its one signed-once deposit
backup, whose absolute locktime matures at `deposit_height + initlock` (~7 days on the deployed
profile). That maturity does **not** stop you sending: a token transfer is a colored *split*, a path
that never touches a ladder, and each piece it mints gets its **own fresh** backup at the full initial
locktime (`create_tx1`, `tx_n = 1`), so you can keep sending forever with the SE. There is **no
clawback risk** (nothing sits above a flat carrier in the tree). The only limit: movement needs the SE
— a *plain* unilateral exit is refused (it would sweep the sats and destroy the allocation), so an
issued carrier's tokens stay anchored on-chain until the SE co-signs a colored spend. So: not lost,
always sendable cooperatively; SE-dependent to move without a colored exit path.

**Tokens you received (a *sub-coin* carrier).** Doubly un-laddered: it is a carrier (terminal-freeze)
*and* a split sub-coin whose funding is un-broadcast, which cannot root a trigger. Also not lost, and
here you have a **SE-free** option: the exit branch is locktime-zero, so broadcasting it *materializes*
the allocation on-chain any time — even a year later — as long as the shared root is still unspent
(`sdk32` broadcasts the branch after two "years" of idle blocks and settles the 250 units on-chain
without the SE). One caveat remains: a lone 1,500-sat received piece is below the carrier floor, so it
can't be re-sent on its own (hold, combine with another piece, or exit).
The former clawback danger is now handled automatically: past
the root deadline (~7 days) the *sender's* own backup matures and a malicious sender could otherwise
sweep the shared funding, but the `auto_exit_due` watchtower now **auto-materializes** a received
carrier as it nears that deadline (broadcasting its branch, emitting `TokenCarrierMaterialized`),
spending the shared root in time so the clawback can never land — the same automatic protection plain
coins get (SPEC §9.5 / REQ-33, `sdk34`). The background watcher runs this pass by default
(`SdkConfig::auto_exit`); the duty is also delegable keyless to any external watchtower via
`export_watch_bundle` (SPEC REQ-34, `sdk45` — the exported bundle carries **no key material**, and a
second independent tower is idempotent, both asserted there). A carrier is exported *without* its
backup tx, so a delegated tower can only ever materialize the branch, never sweep (destroy) the
allocation. A lone piece should still be combined or exited before it can be spent onward.

**Summary:** never lost; cooperative operations (with the SE) work throughout; a received token's
unilateral materialization works forever if the shared root is unspent — and the watchtower now keeps
it unspent for you by materializing before the root deadline, so even an offline receiver is
protected. Note the asymmetry with sats: a plain **laddered** coin has no calendar at all (its exit is
a relative CSV on an un-broadcast trigger, so idling costs nothing and expires nothing), while a token
carrier keeps a root deadline precisely because terminal-freeze holds it off the ladder. That is the
deliberate price of anchoring RGB in signed-once transactions, not a leftover.
