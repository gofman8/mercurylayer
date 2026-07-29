# Issuer SDK guide

Token issuance is part of `UtexoWallet` (any wallet can be an issuer — RGB has no privileged
issuer role beyond holding the contract's issuance rights).

> **The coin your asset lives on is deliberately un-laddered.** Every plain BTC deposit is laddered
> at claim with TES-R (trigger → extension → state, relative CSV, pre-signed and un-broadcast — see
> [getting started §4](getting-started.md)). An RGB **carrier** never is: a plain tier spend would
> sweep the carrier's sats and destroy the allocation, so `claim()` explicitly skips carriers when it
> establishes ladders (terminal-freeze, [PROTOCOL.md §5.10](../PROTOCOL.md) rule 1, pinned by
> `sdk52`: in one wallet the plain coin carries a ladder, the carrier carries none). A carrier keeps
> the **signed-once backup** and moves by colored split + backup-chain handover. That is the current,
> load-bearing shape for RGB — not a legacy lane — and it is why the calendar in
> [Over time](#over-time-what-you-and-your-holders-actually-have-to-watch) applies to tokens while
> plain laddered coins have no deadline at all.

Token calls need RGB configured (`rgb_proxy_url` + `rgb_data_dir`), otherwise you get
`SdkError::TokensNotConfigured`. `SdkConfig::regtest(name)` fills both in; `SdkConfig::mainnet(…)`
leaves them `None` for you to set.

## Launch a token (NIA — fixed supply)

```rust
let (issuer, _) = UtexoWallet::initialize(SdkConfig::regtest("issuer"), None).await?;

// 1. Fund the RGB engine (one-time): issuance needs a colorable UTXO + witness fees.
let fund = issuer.get_token_funding_address().await?;
// send ~100k sats to `fund` and confirm…  (sdk52 sends 100k; sdk09 sends 500k because it also mints)

// 2. The carrier occupies a statechain slot like any deposit — it consumes one deposit token.
issuer.add_prepaid_token(&token_id).await;   // or handle SdkError::TokenPaymentRequired and retry

// 3. Issue. The full supply lands on a fresh statechain coin: the asset's CARRIER.
let asset_id = issuer.issue_token("DEMO", "Demo Token", /*precision*/ 2, /*supply*/ 1_000_000).await?;
println!("token id: {asset_id}");   // rgb:…  — share this as the token identifier
```

One on-chain transaction total: the colored deposit that binds the supply to a carrier coin funded
with 10,000 sats. After it confirms — `claim()`, or the `DepositConfirmed` watcher event — the entire
supply transacts off-chain (`sdk02`).

Two practical consequences of the carrier being a special coin:

- **Its sats are not spendable BTC.** Carrier value is excluded from `available_sats` /
  `pending_sats` / `in_transfer_sats` and surfaces only through `balance.tokens`, because a plain-BTC
  spend of that outpoint would destroy the allocation. Budget the 10,000 sats as part of the asset,
  not as change. (`sdk09` counts confirmed coins rather than sats for exactly this reason.)
- **It never gets a ladder**, so no `LadderEstablished` event fires for it and there is nothing to
  renew or roll over.

## Inflatable supply (IFA): mint and burn

```rust
// 1000 units now, plus a 500-unit inflation right reserved for later.
let asset = issuer.issue_inflatable_token("IFT", "Inflatable Token", 0, 1_000, vec![500]).await?;

// Realize the inflation right. This IS on-chain — inflation is a contract state transition, there is
// no off-chain variant: one inflate tx in the RGB engine, then the minted supply is bound to a FRESH
// carrier coin (another colored deposit). mint_tokens waits for the inflate to confirm, so the chain
// must be advancing (on regtest: run a miner).
let (inflate_txid, minted) = issuer.mint_tokens(&asset, vec![500]).await?;   // minted == 500

// Burn engine-held (free) balance — also on-chain. Supply already bound to a statechain carrier must
// be exited back into the engine first.
let burn_txid = issuer.burn_tokens(&asset, 100).await?;
```

An NIA has no post-issuance mint: its supply is fixed by the contract at issuance. Issue an IFA if
you need inflation, and declare the inflation rights up front — holders can read them off the
contract. Issue → mint → distribute is verified end to end by `sdk09`.

After a mint you hold the asset across **two carriers**. That is normal; distribution handles it
(see combine, below).

## Distribute

```rust
// Single recipient — off-chain, instant.
issuer.transfer_tokens(&asset_id, &user_address, 5_000).await?;

// Many recipients — ONE SE-co-signed colored split carves one exact piece per recipient plus your
// change; each piece ships its own consignment. One TransferResult per recipient, in order.
let results = issuer
    .batch_transfer_tokens(&asset_id, &[(bob_address.clone(), 200), (carol_address, 300)])
    .await?;
```

What a distribution actually does: the carrier is terminalized at the SE (one final co-signature),
the colored split is co-signed once, and each piece becomes a fresh sub-coin carrying
`TOKEN_PIECE_SATS = 1_500` sats of packaging plus the exact token amount as payload. Each piece gets
its **own fresh signed-once backup at the full initial locktime** — the ladder mechanics never enter
this path, which is why an issuer can keep distributing indefinitely no matter how long the carrier
has been sitting still. Holders receive, validate the consignment client-side, and re-transfer with
the standard wallet SDK; there is no issuer involvement after issuance.

Slots for the piece and the change are **derived** from the carrier (free SE vouchers) — distributing
does not burn paid onboarding tokens the way issuance does.

If no single carrier holds the amount — the normal case after a mint — the SDK **combines** several
carriers of the asset into one SE-co-signed colored combine tx (N inputs → exact piece + change).
The receiver then validates the multi-input branch and requires **all N input carriers** to be
terminal (`sdk31`).

A distribution is refused **before** anything is co-signed rather than stranding the carrier:

- the carrier must cover `1_500 + fee reserve` (the reserve is 1% of carrier value, clamped to
  300–2,000 sats);
- both outputs (piece and change) must stay above the minimum viable output at the *live* feerate — a
  sub-coin has to be able to fund its own backup. At high feerates the transfer errors out with the
  carrier untouched.

Token amounts are **raw u64 units**; `precision` is contract metadata the SDK never scales
(`sdk29`). `transfer_tokens(&asset, &addr, 5_000)` on a precision-2 asset moves 5,000 raw units =
50.00 display units.

## Query

```rust
let balances = issuer.get_token_balances().await?;
// [{ asset_id, ticker, name, precision, balance /* settled */, total /* incl. unsettled */ }]

let txs = issuer.query_token_transactions(&asset_id).await?;
// [{ kind, status, amount, txid }]
```

## Semantics vs Spark's issuer SDK

| Spark | Here | Note |
|---|---|---|
| `createToken(isFreezable, maxSupply…)` | `issue_token` (NIA) / `issue_inflatable_token` (IFA) | metadata immutable — same as Spark; NIA supply fixed at issuance, IFA declares its inflation rights up front |
| `mintTokens` | `mint_tokens` | **shipped** for IFA: on-chain inflate + bind to a fresh carrier; an NIA has nothing to realize (`sdk09`) |
| `burnTokens` | `burn_tokens` | on-chain, engine-held balance only; statechain-bound supply must exit first |
| `transferTokens` | `transfer_tokens` | colored off-chain split + backup-chain handover |
| `batchTransferTokens` | `batch_transfer_tokens` | one colored split, N pieces + change (`sdk09`) |
| `freezeTokens` | **intentionally absent** | client-validated assets have no consensus-meaningful freeze; see [tokens](../learn/tokens.md) |
| `getIssuerTokenBalance` | `get_token_balances` | |
| token id `btkn1…` | `rgb:…` contract id | |

## Trust properties for holders

- **Supply is bounded by the contract.** You cannot inflate an NIA at all, and an IFA only up to the
  inflation rights declared at issuance — both are visible to every holder from the contract itself.
- **Transfers validate client-side from consignments.** The SE never vouches for token state; a
  receiver books the amount under the consignment's *verified* contract id.
- **Plain sweeps of a carrier are refused, not silently destructive.** `withdraw` and
  `unilateral_exit` exclude carriers from their defaults and hard-error if a carrier is named
  explicitly ("carries an RGB allocation…"). `refresh` (the on-chain re-anchor) likewise rejects a
  coin holding an allocation, and the maintenance refresh pass skips carriers.
- **A received piece exits without the SE.** Token exit means **materializing the branch**:
  broadcasting the stored branch rows settles the RGB anchors on-chain and the allocation becomes an
  ordinary on-chain RGB holding, spendable with any rgb-lib wallet. This works at depth — a piece two
  colored splits deep materializes by broadcasting its branch root-first (`sdk39`). Onward movement
  of the *settled* allocation still needs the SE; no colored unilateral path is shipped.

## Over time: what you (and your holders) actually have to watch

Tokens are never lost by inactivity — `sdk32` idles the chain a "year" past every deployed horizon
and everything below still holds. But because carriers are un-laddered, calendars still exist here,
unlike for plain BTC coins:

**Your issued/minted carrier is flat** (funded on-chain, no ancestor above it). It has no ladder to
age on — `sdk32` asserts `tesr::load → None` at issuance *and* after `initlock + 500` idle blocks — no
ancestor that could claw it back, and therefore no deadline duty at all: `auto_exit_due` inspects it
and correctly skips it. It does carry its one signed-once deposit backup, maturing at
`deposit_height + initlock` (~7 days on the deployed profile), but that maturity does not gate
distribution: a colored split never touches a ladder and mints fresh full-locktime backups for its
outputs. The one standing limitation is cooperative-only movement — a plain unilateral exit is
refused, so an issued carrier's supply stays anchored where it is until the SE co-signs a colored
spend.

**A holder's received piece is a sub-coin carrier** — doubly un-laddered: a carrier
(terminal-freeze) *and* a split sub-coin whose funding is un-broadcast, which cannot root a trigger.
It can be materialized SE-free at any time while the shared root is unspent. It does have a root
deadline: past it, the *sender's* signed-once backup matures and a malicious sender could sweep the
shared funding. Holders using this SDK are protected by default — the `auto_exit_due` watchtower
(`SdkConfig::auto_exit`, run by `start_background`) materializes the carrier as the deadline nears,
branch-only, emitting `WalletEvent::TokenCarrierMaterialized` (SPEC REQ-33, `sdk34`). The duty is
delegable keyless via `export_watch_bundle` (`sdk45`): the exported bundle carries **no key
material**, a second independent tower is idempotent, and a carrier entry is exported *without* its
backup tx — a delegated tower can only ever materialize the branch, never sweep and destroy the
allocation.

**Advice worth putting in your holder docs:** hold, combine, or materialize — never "refresh before
your locktime expires" (refresh does not apply to carriers) and never a plain unilateral exit. Also
warn that a lone 1,500-sat received piece is below the carrier floor and cannot be re-sent on its own
until it is combined with another piece of the same asset.

## Where next

- [Wallet SDK guide](wallet-sdk.md) — every holder-side operation with examples.
- [Tokens on RGB](../learn/tokens.md) — the conceptual model, freeze rationale, exit behaviour.
- [Granularity deep dive](../learn/granularity-deep-dive.md) — colored splits, raw units vs
  precision, the 1,500-sat piece, exits at depth.
- [PROTOCOL.md §5.10](../PROTOCOL.md) — RGB integration and the terminal-freeze rules normatively.
- [Testing guide](testing-guide.md) — running the token E2Es (`sdk02`, `sdk09`, `sdk29`, `sdk31`,
  `sdk32`, `sdk34`, `sdk39`, `sdk52`).
