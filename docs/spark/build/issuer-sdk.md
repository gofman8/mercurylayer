# Issuer SDK guide

Token issuance is part of `SparkWallet` (any wallet can be an issuer — RGB has no privileged
issuer role beyond holding the contract's issuance rights).

## Launch a token

```rust
let (issuer, _) = SparkWallet::initialize(SdkConfig::regtest("issuer"), None).await?;

// 1. Fund the RGB engine (one-time): issuance builds a colored on-chain tx.
let fund = issuer.get_token_funding_address().await?;
// send ~100k sats to `fund` and confirm…

// 2. Issue. Full supply lands on a fresh statechain coin of this wallet.
let asset_id = issuer.issue_token("DEMO", "Demo Token", /*precision*/ 2, /*supply*/ 1_000_000).await?;
println!("token id: {asset_id}");   // rgb:…  — share this as the token identifier
```

One on-chain transaction total. After it confirms (watcher event), the entire supply transacts
off-chain.

## Distribute

```rust
issuer.transfer_tokens(&asset_id, &user_address, 5_000).await?;  // off-chain, instant
```

Each call carves an exact allocation onto a sub-coin and hands it over; holders receive, validate
and re-transfer with the standard wallet SDK — no issuer involvement after issuance.

## Query

```rust
let balances = issuer.get_token_balances().await?;
// [{ asset_id, ticker: "DEMO", name, precision, balance, total }]
```

## Semantics vs Spark's issuer SDK

| Spark | Here | Note |
|---|---|---|
| `createToken(isFreezable, maxSupply…)` | `issue_token` | supply fixed at issuance (NIA); metadata immutable — same as Spark |
| `mintTokens` | — | NIA has no post-issuance mint; RGB IFA (inflatable) assets are the roadmap for mint/burn parity |
| `freezeTokens` | **intentionally absent** | client-validated assets have no consensus-meaningful freeze; see [tokens](../learn/tokens.md) |
| `getIssuerTokenBalance` | `get_token_balances` | |
| token id `btkn1…` | `rgb:…` contract id | |

## Trust properties for holders

- Supply is fixed by the contract — the issuer cannot inflate an NIA asset.
- Transfers validate client-side from consignments; the SE never vouches for token state.
- Any holder can exit unilaterally; on-chain the asset is a standard RGB holding.
