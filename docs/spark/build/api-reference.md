# API reference — `mercury-spark-sdk`

## `SparkWallet`

### Lifecycle & identity

| Method | Signature | Notes |
|---|---|---|
| `initialize` | `(SdkConfig, Option<&str> mnemonic) -> (SparkWallet, String)` | create or restore; returned mnemonic is the full backup |
| `subscribe` | `() -> broadcast::Receiver<WalletEvent>` | events; multi-consumer |
| `start_background` | `() -> JoinHandle<()>` | claim/deposit watcher at `poll_interval_secs`; `abort()` to stop |
| `get_identity_public_key` | `() -> String` | |
| `get_spark_address` | `() -> String` | stable bech32m statechain address |

### Balance & history

| Method | Signature | Notes |
|---|---|---|
| `get_balance` | `() -> Balance` | `{available_sats, pending_sats, in_transfer_sats, tokens}` |
| `get_token_balances` | `() -> Vec<TokenBalance>` | empty when RGB not configured |
| `get_activities` | `() -> Vec<Activity>` | deposits / sends / receives |

### Deposit

| Method | Signature | Notes |
|---|---|---|
| `get_deposit_address` | `(amount_sats: u64) -> String` | fund with the exact amount; watcher activates |
| `add_prepaid_token` | `(token_id: &str)` | pool pre-paid SE deposit tokens |
| `claim` | `() -> ClaimResult` | one manual watcher pass |

### Send

| Method | Signature | Notes |
|---|---|---|
| `transfer` | `(receiver: &str, amount_sats: u64) -> TransferResult` | exact subset or off-chain split; `used_split` reports which |
| `split_coin` | `(statechain_id: &str, piece_sats: u64) -> (String, String)` | explicit off-chain split (piece id, change id) |
| `transfer_tokens` | `(asset_id: &str, receiver: &str, amount: u64) -> TransferResult` | colored split + handover; consignment in-message |

### Tokens (issuer)

| Method | Signature | Notes |
|---|---|---|
| `get_token_funding_address` | `() -> String` | fund the RGB engine before issuing |
| `issue_token` | `(ticker, name, precision, supply) -> String` | RGB NIA onto a fresh statechain coin; returns `rgb:…` asset id |

### Lightning

| Method | Signature | Notes |
|---|---|---|
| `start_lightning_swap` | `(counterparty: &str, coin: Option<String>) -> LightningSwap` | latch transfer locked on an SE preimage |
| `get_swap_payment_hash` | `(batch_id: &str) -> Option<String>` | counterparty-side verification |
| `settle_lightning_swap` | `(&LightningSwap) -> String` | unlock + preimage (hex) |

### Exit

| Method | Signature | Notes |
|---|---|---|
| `withdraw` | `(to: &str, coins: Option<Vec<String>>, fee_rate: Option<f64>) -> Vec<String>` | cooperative; branches auto-materialize |
| `unilateral_exit` | `(coins: Option<Vec<String>>, to: Option<String>) -> Vec<String>` | branch + stored backup (locktime-gated) |

## Events (`WalletEvent`)

| Event | Payload |
|---|---|
| `DepositConfirmed` | `{address, amount_sats}` |
| `TransferClaimed` | `{statechain_ids}` |
| `TokenTransferClaimed` | `{asset_id, amount, statechain_id}` |
| `BalanceUpdate` | `{balance}` |

## Errors (`SdkError`)

`TokenPaymentRequired{token_id, deposit_address, fee_sats}` · `InsufficientBalance{requested_sats,
available_sats}` · `NoExactAmount{requested_sats}` · `TokensNotConfigured`.

## Types

`Balance`, `TokenBalance`, `TransferResult{receiver_address, total_sats, coins, used_split}`,
`ClaimResult{claimed_transfers, confirmed_deposits}`, `LightningSwap{batch_id, payment_hash,
statechain_id}` — all `serde` serializable for bindings.
