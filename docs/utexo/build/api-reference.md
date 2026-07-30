# API reference — `mercury-utexo-sdk`

Crate: `mercury-utexo-sdk` (`clients/libs/rust-sdk`). Every method below is `async` on `UtexoWallet`
unless marked otherwise; signatures elide `&self` and the `anyhow::Result<…>` wrapper (the one
exception, `pay_lightning_invoice_reclaimable`, is spelled out in full).

## Coin shapes — read this first

There is **one protocol**. `claim()` ladders every fresh confirmed **root** coin with TES-R, always —
`deposit_protocol_version` and the `UTEXO_PROTOCOL_DEFAULT` env are deleted, and nothing opts out.
But not every coin is laddered, **by design**, and a handful of methods behave differently on each
shape. Both shapes are current; neither is legacy.

| | **LADDERED** (TES-R) | **UN-LADDERED** |
|---|---|---|
| Which coins | every plain BTC deposit (laddered at `claim()`) | RGB **carriers** (never laddered — a plain tier spend would destroy the allocation) and **split sub-coins** whose funding tx is un-broadcast (they cannot root a trigger) |
| Exit material | `F → T` (TRIGGER, no timelock) `→ X_m` (EXTENSION, relative CSV `E_m`) `→ S_k` (STATE, relative CSV `Δ_k`) — all v3/TRUC with a P2A anchor, pre-signed and **un-broadcast** | one signed-once backup tx with an absolute `nLockTime`, plus the un-broadcast exit branch from the on-chain root |
| Ageing | **none.** BIP-68 relative locks only start counting once the parent confirms, and `T` carries no timelock, so nothing matures until someone broadcasts `T`. An idle coin never ages: no calendar deadline, 0 vB of idle rent | absolute locktime + a deposit-anchored **exit-race deadline** (`estimate_exit_cost().exit_deadline_block`) that a watchtower must beat |
| Transfer | co-sign a fresh state one `δ` **lower** than the one it replaces, so the new owner's state matures first; the replaced state is disclosed as superseded and counted by the receiver's census | key handover of the backup chain (the receiver's backup sits one `interval` below the sender's) |
| Non-exact payment | **in-ladder split**: a state tier `SP` spending `X_m.out[0]` — a descendant of the trigger, never a rival for `F` — paying a piece child + a change child (sdk58, sdk59) | plain `split_coin`: one SE-co-signed, un-broadcast tx into two sub-coins |
| Maintenance | off-chain **renewal** (a lower-CSV extension) and **rollover** (a fresh level), unbounded — a coin can live off-chain forever (sdk43). `refresh` is the on-chain **re-anchor**, not a deadline reset (sdk30) | `auto_exit_due` / a keyless watch bundle, before the exit-race deadline (sdk45) |
| Unilateral exit | `unilateral_exit` walks the pre-signed chain tier by tier, waiting out each relative timelock; idempotent, call once per block until `complete` (sdk50) | broadcast the exit branch, then the backup once its `nLockTime` matures |

The un-laddered lane is load-bearing for RGB assets (sdk52, sdk39) — it is how every token carrier
moves. See [PROTOCOL.md](../PROTOCOL.md) §5.2–5.4 for the tiers and the in-ladder split,
[CHILDREN.md](../CHILDREN.md) for received split children, [LIGHTNING.md](../LIGHTNING.md) for the
HODL-invoice latch.

## `UtexoWallet`

### Lifecycle & identity

| Method | Signature | Notes |
|---|---|---|
| `initialize` | `(SdkConfig, Option<&str> mnemonic) -> (UtexoWallet, String)` | create or restore; the returned mnemonic restores keys only — see `export_recovery_bundle` |
| `subscribe` | `() -> broadcast::Receiver<WalletEvent>` | events; multi-consumer |
| `start_background` | `() -> JoinHandle<()>` | every `poll_interval_secs`: `claim()`, then `auto_refresh_due` only when `background_auto_refresh` (default **off**), then the `auto_exit_due` pass when `auto_exit` (default on); `abort()` to stop |
| `get_identity_public_key` | `() -> String` | |
| `get_utexo_address` | `() -> String` | stable bech32m statechain address |
| `export_recovery_bundle` | `() -> String` | the ONLY complete backup: wallet record + every backup row (ladders, child bundles, `branch-*`, `parents-*`) + the RGB engine seed. Plain JSON containing the wallet seed — store it securely, and for token wallets also copy `rgb_data_dir` (see wallet-sdk) |
| `import_recovery_bundle` | `(SdkConfig, bundle_json: &str) -> (UtexoWallet, String)` | restore from an exported bundle |
| `sign_message_with_identity_key` | `(message: &[u8]) -> String` | detached signature (hex) over the message |
| `validate_message_with_identity_key` | `(message: &[u8], signature_hex: &str, public_key_hex: &str) -> bool` | **static** verifier for the above |
| `client_config` / `wallet_name` | `() -> &ClientConfig` / `() -> &str` | **sync** accessors; used by SSP-side integrations that drive `mercuryrustlib` directly |

### Balance & history

| Method | Signature | Notes |
|---|---|---|
| `get_balance` | `() -> Balance` | `{available_sats, pending_sats, in_transfer_sats, tokens}` |
| `get_token_balances` | `() -> Vec<TokenBalance>` | empty when RGB not configured |
| `get_activities` | `() -> Vec<Activity>` | deposits / sends / receives |
| `get_transfers` | `() -> Vec<Activity>` | sends / receives only (Spark's `getTransfers`) |
| `get_transfer` | `(utxo: &str) -> Option<Activity>` | single activity by `txid:vout` or `txid` |
| `list_coins` | `() -> Vec<CoinInfo>` | coin inventory with `status` + `off_chain` (true when the coin has a stored exit branch, i.e. its funding is un-broadcast) |

### Deposit

| Method | Signature | Notes |
|---|---|---|
| `get_deposit_address` | `(amount_sats: u64) -> String` | fund with the exact amount; watcher activates |
| `add_prepaid_token` | `(token_id: &str)` | pool pre-paid SE deposit tokens |
| `claim` | `() -> ClaimResult` | one manual watcher pass: confirm deposits, claim incoming transfers, book incoming consignments, and **establish the TES-R ladder** on every fresh confirmed root coin (emits `LadderEstablished`) |

`claim()`'s laddering step is unconditional but skips, by design: RGB carriers (terminal-freeze —
a laddered carrier would be destroyed by a tier spend), coins whose funding `F` is not on-chain, and
coins that already carry a ladder (idempotent). It fails **closed**: if RGB state is momentarily
unreadable the pass establishes nothing and the next `claim()` retries.

### Send

| Method | Signature | Notes |
|---|---|---|
| `transfer` | `(receiver: &str, amount_sats: u64) -> TransferResult` | the one call you normally need; picks the route (below). `used_split` reports whether a split was needed |
| `quote_transfer` | `(amount_sats: u64) -> TransferQuote` | all-in preview: `network_fee_sats` + `renewal_fee_sats` (the on-chain re-anchor this send would trigger, 0 when none is due) + `fundable` + `stuck_coins` |
| `transfer_many` | `(recipients: &[(String, u64)]) -> Vec<TransferResult>` | ONE off-chain split → N pieces (one per recipient) + change. Routes on the parent's shape exactly like `transfer`: a laddered root goes through `in_ladder_pay_many`, a received child through `child_in_ladder_pay_many`, an un-laddered coin through the plain split — so a laddered parent is never plain-split ([B1]). Every piece AND the change must clear the route's floor (the in-ladder floor is the larger: each child funds its own two tiers). On the in-ladder routes each piece is conveyed straight to its recipient's mailbox (they adopt it at claim) rather than handed over afterwards. Not atomic across recipients (sdk69, sdk11) |
| `in_ladder_pay` | `(parent_statechain_id: &str, recipient_address: &str, piece_sats: u64, latch: InLadderLatch) -> (piece_sid, change_sid, Option<(batch_id, payment_hash)>)` | explicit in-ladder split payment of a **laddered** coin: the state tier `SP` spends `X_m.out[0]` and pays a piece child (conveyed to the recipient with the standard key handover) + a change child (kept, persisted as an exitable claim). `latch` is `None` for a plain payment (sdk59) |
| `child_in_ladder_pay` | `(child_statechain_id: &str, recipient_address: &str, piece_sats: u64) -> (piece_sid, change_sid)` | the same, one level down: split a **received child** into two grandchildren (depth-2 ancestors chain), paying one onward (sdk17) |
| `split_coin` | `(statechain_id: &str, piece_sats: u64) -> (piece_id, change_id)` | plain off-chain split of an **un-laddered** coin into two self-owned sub-coins. Hard-errors on a laddered parent ([B1]: a prior owner's retained no-timelock trigger spends the same `F` and could void the split) and on an RGB carrier |
| `ensure_exact_coin` | `(sats: u64) -> String` | mint/find a coin of exactly `sats` (the amount-maker behind single-coin flows). Splits the smallest **un-laddered** coin that fits; errors when only laddered coins are available — Lightning pay falls back to the in-ladder lane in that case |
| `transfer_tokens` | `(asset_id: &str, receiver: &str, amount: u64) -> TransferResult` | colored split OR multi-carrier combine + handover; consignment in-message (combines several carriers when one is insufficient — sdk31) |

**How `transfer` routes.** It first runs the pre-spend auto-refresh hook (when `auto_refresh` is on),
then plans over confirmed, non-carrier coins:

- exact subset of whole coins → plain key handover per coin;
- a **received child** sent whole → `tesr::child_retransfer`, co-signing a fresh lower-CSV state over
  `ext_child.out[0]` for the new recipient and disclosing the replaced state (sdk60);
- non-exact out of a **laddered root** → `in_ladder_pay`;
- non-exact out of a **received child** → `child_in_ladder_pay`;
- non-exact out of an **un-laddered** coin → `split_coin`, then hand the piece over.

**Admission floor for split payments.** A child funds its own two tiers (extension + state) and must
still clear dust, so both the piece and the change must be ≥ `min_child_value` =
`2·(committed_fee + P2A) + dust` = **1306 sat at the default 2 sat/vB** (the plain backup-fee floor
also applies; the larger binds). The guard runs **before** the parent is terminalized, so a rejected
payment leaves the parent fully spendable (sdk58).

**Received children are first-class.** The claim completes the standard SE key handover, so the
receiver co-owns `A_child` (invariant across the rotation, which is what keeps the pre-signed exit
chain valid) and the sender is permanently locked out. A child can be paid onward off-chain — whole
or split — one co-signature and one disclosed superseded state per hop, counted by the receiver's
census (sdk60: alice → bob → carol with the funding outpoint unspent throughout).

### Maintenance (re-anchor)

`refresh` is the **re-anchor** primitive: ONE SE-co-signed on-chain tx moves the coin's value to a
fresh funding outpoint, which mints a brand-new statechain id and a brand-new ladder. It is not a
deadline reset — a laddered coin has no deadline to reset. Reach for it to (a) put an un-laddered
sub-coin or a stale-shaped coin back on a fresh root, (b) invalidate every previous owner's backup
by spending the old outpoint, or (c) unbrick a coin after a failed latch. Renewal and rollover of a
live ladder are off-chain and cost nothing (sdk43); re-anchoring is the only step that touches L1
(sdk30). Refresh is **cooperative** (it needs the SE); if the SE is gone, exit unilaterally.

| Method | Signature | Notes |
|---|---|---|
| `refresh` | `(statechain_id: &str, fee_rate: Option<f64>) -> RefreshResult` | user-pays: the fee comes from the coin (1-in-1-out P2TR, 112 vB), so the fresh coin is `amount − fee`. Errors on a non-`CONFIRMED` coin, an RGB carrier, or `CoinBelowMaintenanceCost` |
| `refresh_sponsored` | `(statechain_id: &str, sponsor: &UtexoWallet, fee_rate: Option<f64>) -> RefreshResult` | same re-anchor, then the sponsor rebates the fee off-chain. The rebate is `max(fee + dust, min_child_value)` — the smallest off-chain-payable amount — so `rebate_sats ≥ fee_sats` and the user ends ≥ whole |
| `rebate_refresh_fee` | `(to_utexo_address: &str, fee_sats: u64) -> TransferResult` | sponsor side; thin wrapper over `transfer` |
| `auto_refresh_due` | `(margin_blocks: u32) -> Vec<RefreshResult>` | maintenance pass: re-anchor confirmed non-carrier coins whose signed-once backup `nLockTime` is within `margin_blocks` of the tip. Carriers are skipped (a plain re-anchor destroys the allocation — their protection is `auto_exit_due`); coins below their own fee are skipped, not failed. Emits `CoinRefreshed` per coin. Called before every `transfer`/`transfer_many` when `auto_refresh` (default on, margin 144), and from the background watcher only when `background_auto_refresh` (default off) |

### Tokens (issuer)

RGB carriers ride the un-laddered lane end to end: never laddered, never plain-split, never
plain-exited.

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

Lightning works in **both directions on the ladder** through an SSP (Utexo Service Provider: a
statechain wallet plus an RLN node) using a **HODL-invoice latch**. PAY: the user latches a coin to
the invoice's payment hash and hands it over; the SSP censuses it, pays the BOLT11, and the LN
preimage is simultaneously the user's proof of payment and the SSP's key to unlock the coin. RECEIVE:
the SSP latches a coin to an SE-held preimage and issues a HODL invoice on that hash; it can only
retrieve the preimage — and so claim the HTLC — after releasing the coin. Exact amounts use a whole
coin (sdk63 pay, sdk64 receive); non-exact amounts use the in-ladder split, with the piece child
latched (sdk65 pay, sdk67 receive). Failures roll back (sdk66, sdk68). The same calls work against a
remote SSP over HTTP (sdk21).

The **latched piece is the one case that stays terminalized**: it sits unclaimed past the
pending-transfer lock's window, so the SE co-signs nothing further over it.

**User side** — `ssp` is any `&impl Ssp` (in-process `SspService` or remote `SspClient`):

| Method | Signature | Notes |
|---|---|---|
| `pay_lightning_invoice` | `(ssp: &impl Ssp, invoice: &str) -> String` | quote → mint/latch → SSP pays → returns the **preimage** (proof of payment). Auto-routes: exact coin when one can be minted, otherwise the non-exact in-ladder lane; an RGB invoice latches a colored coin instead |
| `pay_lightning_invoice_reclaimable` | `(ssp: &impl Ssp, invoice: &str) -> std::result::Result<String, (String, anyhow::Error)>` | same, but the error carries the latched coin's statechain id for `reclaim_lightning_payment` (empty string = nothing was latched) |
| `pay_lightning_invoice_inladder` | `(ssp: &impl Ssp, invoice: &str, parent_statechain_id: &str) -> String` | explicit non-exact pay from one laddered coin: in-ladder split, piece latched to the invoice hash + a self-owned change. On failure the split is rolled back and the whole parent is recovered (sdk66) |
| `create_lightning_invoice` | `(ssp: &impl Ssp, amount_sats: u64) -> ReceiveSwap` | receive sats: returns the BOLT11 to hand the payer; the coin lands via the background watcher (`TransferClaimed`) |
| `create_lightning_invoice_asset` | `(ssp: &SspService, asset_id: &str, asset_amount: u64) -> ReceiveSwap` | receive an RGB asset onto a colored coin (local SSP only) |
| `reclaim_lightning_payment` | `(coin_statechain_id: &str) -> ()` | recover a coin whose pay swap never settled. **Only call once you have positively confirmed non-payment** — a client timeout is not proof, and after the SE `batch_timeout` this succeeds even if the SSP did pay. On a laddered coin it restores the coin locally as exitable (the failed latch left an orphan co-signed state, so off-chain re-transfer stays blocked until a `refresh`) |

**SSP side** — `SspService::new(wallet, RlnClient::new(api_url), fee_sats)`; `SspClient::new(base_url)`
speaks the deployed `mercury-ssp` HTTP API. The `Ssp` trait is `info` / `quote_pay` / `execute_pay` /
`create_receive`; `settle_receive` is deliberately not on it (it is never a user operation).

| Method | Signature | Notes |
|---|---|---|
| `SspService::quote_pay` | `(invoice: &str) -> PayQuote` | what the user must latch over, and to which address |
| `SspService::execute_pay` | `(invoice: &str, batch_id: &str) -> String` | pre-payment gate (latch hash matches the invoice, every latched coin is a pending transfer addressed to the SSP, value ≥ invoice + fee, ladder census on a laddered coin) → pay → unlock by preimage → claim |
| `SspService::create_receive` | `(amount_sats: u64, receiver_address: &str) -> ReceiveSwap` | exact coin when one can be minted, else a non-exact in-ladder piece latched under an SE-minted preimage |
| `SspService::create_receive_asset` | `(asset_id: &str, asset_amount: u64, receiver_address: &str) -> ReceiveSwap` | colored receive swap + RGB HODL invoice |
| `SspService::settle_receive` | `(&ReceiveSwap) -> ()` | wait for the HTLC to be **held** (`Claimable`), release the coin, then retrieve the preimage and claim the HODL invoice |
| `SspService::cancel_receive` | `(&ReceiveSwap) -> ()` | cancel the HODL invoice and reclaim the un-released coin |

**Raw latch primitives** (used directly by an LSP integration that drives its own Lightning node):

| Method | Signature | Notes |
|---|---|---|
| `start_lightning_swap` | `(counterparty: &str, coin: Option<String>) -> LightningSwap` | latch transfer locked on a fresh SE-held preimage |
| `get_swap_payment_hash` | `(batch_id: &str) -> Option<String>` | counterparty-side verification |
| `settle_lightning_swap` | `(&LightningSwap) -> String` | unlock + preimage (hex) |
| `latch_tokens` | `(asset_id: &str, receiver_address: &str, token_amount: u64, payment_hash: &str) -> (batch_id, piece_statechain_id)` | colored transfer latched on an **external** payment hash (RGB pay) |
| `latch_tokens_se_preimage` | `(asset_id: &str, receiver_address: &str, token_amount: u64) -> (batch_id, piece_statechain_id, payment_hash)` | colored transfer latched on an **SE-held** preimage (RGB receive) |

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

### Exit & watchtower

| Method | Signature | Notes |
|---|---|---|
| `withdraw` | `(to: &str, coins: Option<Vec<String>>, fee_rate: Option<f64>) -> Vec<String>` | cooperative, SE-co-signed, no timelock wait; sub-coin branches auto-materialize first. Refuses RGB carriers. A received child has no confirmed outpoint to spend, so it is routed to `unilateral_exit` and marked `WITHDRAWING` |
| `unilateral_exit` | `(coins: Option<Vec<String>>, to: Option<String>) -> Vec<ExitStatus>` | no SE needed. **Laddered coin**: walks the tier chain (`T`, then each extension/state as its relative CSV matures) — idempotent and incremental, so call once per block until `complete`; `wait_blocks` is the remaining maturity of the next tier (sdk50). **Received child**: walks `T → X_m → SP → ext_child → state_child`, whose final state already pays this wallet's key. **Un-laddered coin**: broadcasts the exit branch, then the latest backup once its `nLockTime` is reached. Refuses carriers and any coin that is no longer `CONFIRMED` (exiting a spent parent would void the sub-coins its split funded). `to` is accepted but unused — every path pays the coin's own pre-signed payee (a seed-derived address of this wallet) |
| `defend_ladders` | `() -> Vec<String>` | owner-run **ladder** watchtower pass. A no-op while `F` is unspent — an idle ladder has nothing to defend. If someone **triggers** the coin (a contested exit), this races the owner's tiers; the adopted current state carries the strictly-lowest CSV, so it matures first and pays the owner. Idempotent; call once per block. Emits `LadderDefended` (sdk51) |
| `auto_exit_due` | `(margin_blocks: u32) -> Vec<String>` | watchtower pass for the **un-laddered** lane: force-exit plain off-chain sub-coins and **materialize** received token carriers (branch only — a backup sweep would destroy the allocation) within `margin_blocks` of their exit-race deadline. Run by the background watcher each poll when `auto_exit` (default on, margin `auto_exit_margin_blocks` = 288) |
| `export_watch_bundle` | `() -> String` | **keyless** watch bundle (JSON `WatchBundle`): per off-chain coin, the exit branch + deadline (+ the backup for plain coins; carriers are branch-only, so no tower can sweep them). No mnemonic, no key shares, no RGB seed — safe to hand to untrusted watchtowers. Re-export after any transfer/claim/split (sdk45) |
| `watchtower::watch_pass` | `(bundle: &WatchBundle, electrum: &Client, margin_blocks: u32) -> (Vec<String>, Vec<String>)` | **free function, sync**: one keyless watch iteration (acted ids, errors) from a bundle + electrum only — no wallet, DB, SE or keys. Idempotent, so several independent towers can run it; a genuine rejection (the root already spent by a competing tx) is reported, never swallowed |
| `estimate_exit_cost` | `(statechain_id: &str) -> ExitCostEstimate` | tx count, vbytes, `wait_blocks` (when the exit **completes**) and `exit_deadline_block` (the un-laddered safety deadline; `None` for a flat on-chain coin) |
| `get_withdrawal_fee_quote` | `(statechain_ids: Option<Vec<String>>) -> WithdrawalFeeQuote` | cooperative-withdrawal fee quote |

## Events (`WalletEvent`)

| Event | Payload |
|---|---|
| `DepositConfirmed` | `{address, amount_sats}` |
| `TransferClaimed` | `{statechain_ids}` |
| `TokenTransferClaimed` | `{asset_id, amount, statechain_id}` |
| `BalanceUpdate` | `{balance}` |
| `LadderEstablished` | `{statechain_id}` — `claim()` established the TES-R ladder on a fresh confirmed root coin; its tiers are pre-signed and un-broadcast, and it is now transferable by state replacement |
| `LadderDefended` | `{statechain_id, tiers_broadcast}` — the coin was found **triggered** (someone spent `F`) and `defend_ladders` broadcast tier tx(s) this pass; emitted per pass until the exit completes |
| `ExitBranchConflict` | `{statechain_id}` — a competing tx is spending the branch root; fee-bump/re-attempt (do **not** assume the coin exited) |
| `ExitDeadlineApproaching` | `{statechain_id, deadline_block, tip}` — an **un-laddered** off-chain sub-coin is near its exit-race deadline; `auto_exit_due` acts on it |
| `CoinRefreshed` | `{old_statechain_id, new_statechain_id, fee_sats}` — a coin was re-anchored; re-export the recovery/watch bundles |
| `TokenCarrierMaterialized` | `{statechain_id, deadline_block, tip}` — the watchtower settled a received token carrier on-chain (branch-only) before its clawback deadline |

## Errors (`SdkError`)

`TokenPaymentRequired{token_id, deposit_address, fee_sats}` · `InsufficientBalance{requested_sats,
available_sats}` · `NoExactAmount{requested_sats}` · `TokensNotConfigured` ·
`CoinBelowMaintenanceCost{statechain_id, amount_sats, fee_sats}` — the coin's value is at or below
its own re-anchor fee, so it cannot pay to move itself. Not lost: combine it with another coin. Such
coins are excluded from routine auto-refresh and reported in `TransferQuote::stuck_coins`.
Everything else surfaces as `anyhow::Error`.

## Config (`SdkConfig`)

Presets: `SdkConfig::regtest(name)`, `SdkConfig::mainnet(name, se_url, electrum_url)`. Fields:
`wallet_name`, `statechain_entity_url`, `electrum_url`, `electrum_type`, `network`, `database_file`,
`confirmation_target`, `rgb_proxy_url` + `rgb_data_dir` (both required for token support),
`deposit_token_id`, `poll_interval_secs`, plus the maintenance knobs:

| Field | Default | Meaning |
|---|---|---|
| `auto_refresh` | `true` | run the pre-spend re-anchor hook inside `transfer`/`transfer_many`, so the cost shows up as a payment fee instead of a balance shrinking in the background |
| `auto_refresh_margin_blocks` | `144` | backup-locktime headroom at or below which a coin is re-anchored |
| `background_auto_refresh` | `false` | also re-anchor from the background watcher (routine, unprompted). Off by default |
| `auto_exit` | `true` | run `auto_exit_due` from the background watcher |
| `auto_exit_margin_blocks` | `288` | deadline margin for that pass; must absorb the audit-[17] gap (`≥ k_max·interval + 144`) |

There is no `deposit_protocol_version` field and no `UTEXO_PROTOCOL_DEFAULT` env var — both are
deleted. Coin shape follows from what the coin is (see the table at the top), not from configuration.

## Types

`serde` serializable (what the language bindings marshal), except `RefreshResult`, the Lightning
types and `InLadderLatch`, which are `Clone + Debug` only.

- `Balance`, `TokenBalance`, `CoinInfo{statechain_id, amount_sats, status, utxo_txid, utxo_vout, off_chain}`
- `TransferResult{receiver_address, total_sats, coins: Vec<TransferredCoin>, used_split}`
- `TransferQuote{amount_sats, network_fee_sats, renewal_fee_sats, total_fee_sats, fundable, stuck_coins, note}`
- `ClaimResult{claimed_transfers, confirmed_deposits, token_results}` with
  `TokenClaimStatus{statechain_id, state, asset_id, amount, detail}` and
  `TokenClaimState::{Booked, Pending, Rejected}` — a claimed sats transfer and its token booking are
  separate steps, so read `token_results` rather than inferring "tokens received" from
  `claimed_transfers`
- `RefreshResult{old_statechain_id, new_statechain_id, old_amount_sats, new_amount_sats, fee_sats, refresh_txid, rebate_sats}`
- `ExitCostEstimate{statechain_id, branch_txs, branch_vbytes, backup_vbytes, total_vbytes, wait_blocks, exit_deadline_block}` (+ `fee_sats_at(rate)`), `ExitStatus{statechain_id, complete, wait_blocks}`, `WithdrawalFeeQuote`
- `WatchBundle{version, wallet_name, entries}` / `WatchEntry{statechain_id, token_carrier, deadline_block, branch_txs, backup_tx?, backup_locktime?}`
- `UtexoInvoice{version, address, amount, asset_id, memo, expiry_unix}`, `TokenTx`
- Lightning: `LightningSwap{batch_id, payment_hash, statechain_id}`, `PayQuote{amount_sats, fee_sats, payment_hash, ssp_address, asset_id, asset_amount}`, `ReceiveSwap{batch_id, statechain_id, invoice, payment_hash, asset_id, asset_amount}`, `SspInfo{ssp_address, fee_sats}`, `DecodedInvoice{amt_msat, payment_hash, asset_id, asset_amount}`, and `InLadderLatch::{None, External(&str), ClassicMinted}` — a call-site enum choosing how an in-ladder piece is latched (`None` = plain payment, `External` = non-exact LN pay, `ClassicMinted` = non-exact LN receive)
- `is_terminal(sig_budget: Option<i64>, finalized: i64) -> bool` — free function mirroring the SE's
  terminal predicate (SPEC §3.6); the authoritative value comes from `GET /statechain/spend_budget`
