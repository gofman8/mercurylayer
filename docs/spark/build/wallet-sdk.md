# Wallet SDK guide

Crate: `mercury-spark-sdk` (`clients/libs/rust-sdk`). Everything below is on `SparkWallet`.

## Create / restore

```rust
// New wallet (mnemonic generated)
let (wallet, mnemonic) = SparkWallet::initialize(SdkConfig::regtest("alice"), None).await?;

// Re-open a wallet whose database_file still exists (same mnemonic)
let (wallet, _) = SparkWallet::initialize(SdkConfig::regtest("alice"), Some(&mnemonic)).await?;
```

> ⚠️ **The mnemonic ALONE is not a sufficient backup.** It restores the key hierarchy, but off-chain
> statechain funds are only safe if you can *exit* them, and the exit material lives ONLY on your
> disk — the SE cannot re-serve it after a claim: the pre-signed backup ladder, the off-chain exit
> branches, the terminal-ancestor lists, and (for token wallets) the entire RGB stash under a
> *separate* `rgb.mnemonic` inside `rgb_data_dir`. **Losing `wallet.db` or `rgb_data_dir` is total
> loss of every off-chain coin and token, even with the mnemonic.**

### Full backup / restore

```rust
// Back up EVERYTHING (wallet record + exit ladder + branches + terminal parents + RGB seed).
// Re-export after every transfer/claim/split. Store securely — it contains the wallet seed.
let bundle_json = wallet.export_recovery_bundle().await?;
std::fs::write("alice-recovery.json", &bundle_json)?;
// For token wallets, ALSO copy the whole rgb_data_dir (the RGB stash is not embedded in the bundle).

// Restore into a fresh database_file:
let cfg = SdkConfig::regtest("alice"); // point database_file at a fresh path
let (wallet, _) = SparkWallet::import_recovery_bundle(cfg, &bundle_json).await?;
// Then restore rgb_data_dir contents for token balances.
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

// Multi-recipient colored split: ONE SE-co-signed tx carves one piece per recipient
// (its exact amount) plus this wallet's change; each piece ships its own consignment.
// Returns one TransferResult per recipient, in order.
let results = wallet
    .batch_transfer_tokens(&asset_id, &[(bob_address.clone(), 250), (carol_address, 100)])
    .await?;
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

## Refresh (re-anchor)

A statechain coin's decrementing-`nLockTime` backup ladder is a finite budget: as it nears the
floor the coin becomes un-transferable and must be reset on L1. `refresh` is that reset — ONE
SE-co-signed transaction spends the coin's current outpoint into a fresh deposit aggregate (new
`statechain_id`, same owner, fresh full ladder + fresh root deadline). The old outpoint is now
spent, so every previous owner's backup is permanently invalidated. The fresh coin confirms
asynchronously (watcher/`claim()`), like a deposit. Refresh is cooperative (needs the SE); if the
SE is gone, exit unilaterally instead. The coin must be `CONFIRMED`, carry no RGB allocation, and
be large enough to cover the fee above the dust floor.

```rust
// User pays: the on-chain fee comes from the coin, so the refreshed coin is amount − fee.
// fee_rate (sat/vB) is capped at max_fee_rate; None uses the SE-quoted rate.
let r = wallet.refresh(&statechain_id, None).await?;
// r.old_statechain_id (now spent) / r.new_statechain_id / r.new_amount_sats == amount − fee

// Operator pays: same on-chain re-anchor, then a funded `sponsor` wallet reimburses the fee
// OFF-CHAIN (instant, free) so the user's total balance ends ≥ whole. r.rebate_sats reports
// the amount rebated (≥ fee_sats).
let r = wallet.refresh_sponsored(&statechain_id, &sponsor, None).await?;
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
