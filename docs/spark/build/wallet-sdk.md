# Wallet SDK guide

Crate: `mercury-spark-sdk` (`clients/libs/rust-sdk`). Everything below is on `SparkWallet`.

## Create / restore

```rust
// New wallet (mnemonic generated — persist it!)
let (wallet, mnemonic) = SparkWallet::initialize(SdkConfig::regtest("alice"), None).await?;

// Restore
let (wallet, _) = SparkWallet::initialize(SdkConfig::regtest("alice"), Some(&mnemonic)).await?;
```

`SdkConfig` fields: SE URL, electrum, network, sqlite `database_file`, `confirmation_target`,
RGB proxy + data dir (token support), `poll_interval_secs`, optional `deposit_token_id`.
Presets: `SdkConfig::regtest(name)`, `SdkConfig::mainnet(name, se_url, electrum_url)`.

## Addresses & identity

```rust
let address = wallet.get_spark_address().await?;       // stable bech32m ml1…/tml1…
let id_pub  = wallet.get_identity_public_key().await?;
```

## Balance

```rust
let b = wallet.get_balance().await?;
// b.available_sats  — spendable now
// b.pending_sats    — deposits below confirmation target
// b.in_transfer_sats— outgoing, unclaimed by the receiver
// b.tokens          — Vec<TokenBalance{asset_id, ticker, balance, …}>
```

## Deposit

```rust
let addr = wallet.get_deposit_address(100_000).await?;  // fund with exactly 100k sats
```

The background watcher confirms and activates it (`DepositConfirmed` event). Deposit slots
consume SE deposit tokens: the SDK fetches them; if the SE charges, you get
`SdkError::TokenPaymentRequired { token_id, deposit_address, fee_sats }` — pay it and retry, or
pre-pay and pool with `wallet.add_prepaid_token(&token_id)`.

## Transfer (exact amounts, off-chain)

```rust
let r = wallet.transfer(&bob_address, 15_000).await?;
// r.used_split == true when the SDK minted the exact amount via an off-chain split
```

Explicit split (rarely needed — `transfer` does it internally):

```rust
let (piece_id, change_id) = wallet.split_coin(&statechain_id, 15_000).await?;
```

## Receive (auto-claim + events)

```rust
let mut events = wallet.subscribe();
let handle = wallet.start_background();          // poll loop; abort() to stop
while let Ok(ev) = events.recv().await {
    match ev {
        WalletEvent::TransferClaimed { statechain_ids } => { /* sats arrived */ }
        WalletEvent::TokenTransferClaimed { asset_id, amount, .. } => { /* tokens arrived */ }
        WalletEvent::DepositConfirmed { amount_sats, .. } => { /* deposit live */ }
        WalletEvent::BalanceUpdate { balance } => { /* refresh UI */ }
    }
}
// One-shot instead of background: wallet.claim().await?
```

## Tokens

```rust
// hold + send (issuance: see the issuer guide)
let tokens = wallet.get_token_balances().await?;
wallet.transfer_tokens(&asset_id, &bob_address, 250).await?;
```

## Lightning swap legs

```rust
// coin → Lightning payment, via an LSP:
let swap = wallet.start_lightning_swap(&lsp_address, None).await?;
// … LSP verifies get_swap_payment_hash(&swap.batch_id) and pays your invoice for swap.payment_hash …
let preimage = wallet.settle_lightning_swap(&swap).await?;  // unlocks the coin for the LSP
```

## Exit

```rust
// Cooperative (immediate, 1 tx per coin; sub-coin branches auto-materialize):
wallet.withdraw("bc1p…", None /* all coins */, None /* fee rate */).await?;

// Unilateral (no SE; branch + pre-signed backup, locktime-gated):
wallet.unilateral_exit(None, None).await?;
```

## History

```rust
let activities = wallet.get_activities().await?;  // deposits, sends, receives
```

## Errors worth handling

| Error | Meaning | Action |
|---|---|---|
| `TokenPaymentRequired` | SE charges for deposit slots | pay `fee_sats` to `deposit_address`, retry |
| `InsufficientBalance` | amount > spendable | top up / lower amount |
| `TokensNotConfigured` | token call without RGB config | set `rgb_proxy_url` + `rgb_data_dir` |
