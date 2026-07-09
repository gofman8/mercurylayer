# API reference — `mercury-utexo-sdk`

## `UtexoWallet`

### Lifecycle & identity

| Method | Signature | Notes |
|---|---|---|
| `initialize` | `(SdkConfig, Option<&str> mnemonic) -> (UtexoWallet, String)` | create or restore; returned mnemonic is the full backup |
| `subscribe` | `() -> broadcast::Receiver<WalletEvent>` | events; multi-consumer |
| `start_background` | `() -> JoinHandle<()>` | claim/deposit watcher at `poll_interval_secs`; `abort()` to stop |
| `get_identity_public_key` | `() -> String` | |
| `get_utexo_address` | `() -> String` | stable bech32m statechain address |
| `export_recovery_bundle` | `() -> String` | canonical encrypted backup blob (see wallet-sdk) |
| `import_recovery_bundle` | `(SdkConfig, bundle_json: &str) -> (UtexoWallet, String)` | restore from an exported bundle |
| `sign_message_with_identity_key` | `(message: &[u8]) -> String` | detached signature (hex) over the message |
| `validate_message_with_identity_key` | `(message: &[u8], signature_hex: &str, public_key_hex: &str) -> bool` | static verifier for the above |

### Balance & history

| Method | Signature | Notes |
|---|---|---|
| `get_balance` | `() -> Balance` | `{available_sats, pending_sats, in_transfer_sats, tokens}` |
| `get_token_balances` | `() -> Vec<TokenBalance>` | empty when RGB not configured |
| `get_activities` | `() -> Vec<Activity>` | deposits / sends / receives |
| `get_transfers` | `() -> Vec<Activity>` | sends / receives only (Spark's `getTransfers`) |
| `get_transfer` | `(utxo: &str) -> Option<Activity>` | single activity by `txid:vout` or `txid` |
| `list_coins` | `() -> Vec<CoinInfo>` | coin inventory with `status` + `off_chain` flag |

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
| `transfer_tokens` | `(asset_id: &str, receiver: &str, amount: u64) -> TransferResult` | colored split OR multi-carrier combine + handover; consignment in-message (combines several carriers when one carrier is insufficient — sdk31) |
| `transfer_many` | `(recipients: &[(String, u64)]) -> Vec<TransferResult>` | one off-chain split → N pieces (one per recipient) + change |
| `refresh` | `(statechain_id: &str, fee_rate: Option<f64>) -> RefreshResult` | user-pays on-chain re-anchor; resets ladder + root deadline; old backups invalidated |
| `refresh_sponsored` | `(statechain_id: &str, sponsor: &UtexoWallet, fee_rate: Option<f64>) -> RefreshResult` | same re-anchor, then operator sponsor rebates the fee off-chain (`rebate_sats` ≥ `fee_sats`) |
| `rebate_refresh_fee` | `(to_utexo_address: &str, fee_sats: u64) -> TransferResult` | sponsor side; thin wrapper over `transfer` |

### Tokens (issuer)

| Method | Signature | Notes |
|---|---|---|
| `get_token_funding_address` | `() -> String` | fund the RGB engine before issuing |
| `get_token_l1_address` | `() -> String` | alias of `get_token_funding_address` |
| `issue_token` | `(ticker, name, precision, supply) -> String` | RGB NIA onto a fresh statechain coin; returns `rgb:…` asset id |
| `issue_inflatable_token` | `(ticker, name, precision, supply, inflation_amounts: Vec<u64>) -> String` | inflatable (CFA-style) issuance with reserved inflation rights |
| `mint_tokens` | `(asset_id: &str, inflation_amounts: Vec<u64>) -> (String, u64)` | mint against reserved inflation rights; returns `(txid, minted)` |
| `burn_tokens` | `(asset_id: &str, amount: u64) -> String` | burn held supply; returns txid |
| `batch_transfer_tokens` | `(asset_id: &str, transfers: &[(String, u64)]) -> Vec<TransferResult>` | one colored tx to many recipients |
| `query_token_transactions` | `(asset_id: &str) -> Vec<TokenTx>` | RGB-engine transfer history (`getTokenTransactions`) |

### Lightning

| Method | Signature | Notes |
|---|---|---|
| `start_lightning_swap` | `(counterparty: &str, coin: Option<String>) -> LightningSwap` | latch transfer locked on an SE preimage |
| `get_swap_payment_hash` | `(batch_id: &str) -> Option<String>` | counterparty-side verification |
| `settle_lightning_swap` | `(&LightningSwap) -> String` | unlock + preimage (hex) |
| `latch_tokens` | `(asset_id: &str, receiver_address: &str, token_amount: u64, payment_hash: &str) -> (String, String)` | colored transfer latched on an external payment hash |

### Invoices

Self-describing payment requests. An invoice encodes the recipient's utexo address plus the requested
amount, optional asset (sats when absent), memo, and expiry, and a payer fulfills it in one call.

| Method | Signature | Notes |
|---|---|---|
| `create_sats_invoice` | `(amount: u64, memo: Option<String>, expiry_unix: Option<u64>) -> String` | sats request payable to this wallet; returns a `utexoinv1…` string |
| `create_tokens_invoice` | `(asset_id: &str, amount: u64, memo: Option<String>, expiry_unix: Option<u64>) -> String` | token request payable to this wallet |
| `fulfill_utexo_invoice` | `(invoice: &str) -> TransferResult` | decode, check expiry, then `transfer`/`transfer_tokens` to the embedded address; errors if expired |

Free functions (re-exported from the crate root): `encode_utexo_invoice(&UtexoInvoice) -> String`
encodes as `utexoinv1<hex(json)>`; `decode_utexo_invoice(&str) -> UtexoInvoice` parses one back
(errors on a missing `utexoinv1` prefix or bad hex).

### Exit

| Method | Signature | Notes |
|---|---|---|
| `withdraw` | `(to: &str, coins: Option<Vec<String>>, fee_rate: Option<f64>) -> Vec<String>` | cooperative; branches auto-materialize |
| `unilateral_exit` | `(coins: Option<Vec<String>>, to: Option<String>) -> Vec<String>` | branch + stored backup (locktime-gated) |
| `auto_exit_due` | `(margin_blocks: u32) -> Vec<String>` | watchtower pass: force-exit plain sub-coins / MATERIALIZE token carriers (branch-only) within `margin_blocks` of the deadline; run by the background watcher each poll when `auto_exit` (default on, margin `auto_exit_margin_blocks`=288) |
| `auto_refresh_due` | `(margin_blocks: u32) -> Vec<RefreshResult>` | re-anchor confirmed non-carrier coins whose ladder headroom ≤ margin; run by the background watcher and before every `transfer`/`transfer_many` when `auto_refresh` (default on, margin `auto_refresh_margin_blocks`=144) |
| `export_watch_bundle` | `() -> String` | KEYLESS watch bundle (JSON `WatchBundle`): per off-chain coin, branch txs + deadline (+ backup for plain coins; carriers are branch-only) — no key material; safe to hand to untrusted watchtowers |
| `watchtower::watch_pass` | `(bundle: &WatchBundle, electrum: &Client, margin_blocks: u32) -> (Vec<String>, Vec<String>)` | free function: one keyless watch iteration (acted ids, errors) from a bundle + electrum only — no wallet/DB/SE/keys; idempotent across multiple towers |
| `estimate_exit_cost` | `(statechain_id: &str) -> ExitCostEstimate` | projected unilateral-exit cost for a coin |
| `get_withdrawal_fee_quote` | `(statechain_ids: Option<Vec<String>>) -> WithdrawalFeeQuote` | cooperative-withdrawal fee quote |

## Events (`WalletEvent`)

| Event | Payload |
|---|---|
| `DepositConfirmed` | `{address, amount_sats}` |
| `TransferClaimed` | `{statechain_ids}` |
| `TokenTransferClaimed` | `{asset_id, amount, statechain_id}` |
| `BalanceUpdate` | `{balance}` |
| `ExitBranchConflict` | `{statechain_id}` — a competing tx is spending the branch root; fee-bump/re-attempt |
| `ExitDeadlineApproaching` | `{statechain_id, deadline_block, tip}` — sub-coin near its exit-race deadline; `auto_exit_due` acts on it |
| `CoinRefreshed` | `{old_statechain_id, new_statechain_id, fee_sats}` — auto-refresh re-anchored a coin; re-export the recovery/watch bundles |
| `TokenCarrierMaterialized` | `{statechain_id, deadline_block, tip}` — the watchtower settled a received token carrier on-chain (branch-only) before its clawback deadline |

## Errors (`SdkError`)

`TokenPaymentRequired{token_id, deposit_address, fee_sats}` · `InsufficientBalance{requested_sats,
available_sats}` · `NoExactAmount{requested_sats}` · `TokensNotConfigured`.

## Types

`Balance`, `TokenBalance`, `TransferResult{receiver_address, total_sats, coins, used_split}`,
`ClaimResult{claimed_transfers, confirmed_deposits}`, `LightningSwap{batch_id, payment_hash,
statechain_id}`, `UtexoInvoice{version, address, amount, asset_id, memo, expiry_unix}`,
`RefreshResult{old_statechain_id, new_statechain_id, old_amount_sats, new_amount_sats, fee_sats,
refresh_txid, rebate_sats}`, `TokenTx`, `ExitCostEstimate`, `WithdrawalFeeQuote`,
`WatchBundle{version, wallet_name, entries}` / `WatchEntry{statechain_id, token_carrier,
deadline_block, branch_txs, backup_tx?, backup_locktime?}` — all `serde` serializable for bindings.
