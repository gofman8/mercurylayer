# Wallet SDK guide

Crate: `mercury-utexo-sdk` (`clients/libs/rust-sdk`). Everything below is on `UtexoWallet`.

## What a coin is (read this first)

There is **one protocol**. Every plain BTC deposit is **laddered** with TES-R — three tiers of
pre-signed, **un-broadcast** v3/TRUC transactions with P2A anchors:

```
F (on-chain funding)
└─ T   TRIGGER    no timelock, signed once at deposit
   └─ X_m EXTENSION  relative CSV E_m  — renewal replaces it horizontally
      └─ S_k STATE   relative CSV Δ_k  — decrements by δ on every transfer
```

Three consequences the API depends on:

- **An idle coin never ages.** BIP-68 relative timelocks only start counting once the parent
  confirms, and `T` has no timelock — so nothing matures until someone broadcasts `T`. There is no
  calendar deadline and **0 vB of idle rent**. Do not build "expiry" UI for a laddered coin.
- **A transfer co-signs a fresh state one δ *lower*** than the one it replaces, so the new owner's
  state always matures first; the superseded state is disclosed and counted by the receiver's
  census. Nothing goes on-chain.
- **Renewal and rollover are off-chain and unbounded** — a coin can live off-chain forever
  (`sdk43`). `refresh` is the **re-anchor** primitive (one on-chain tx), not a deadline reset.

**Not every coin is laddered, by design.** Two shapes coexist:

| Shape | Which coins | Transfer mechanism | Exit |
|---|---|---|---|
| **Laddered** (TES-R) | every plain BTC deposit | co-sign a lower-CSV state | walk `T → X_m → S_k`, waiting out each relative CSV |
| **Un-laddered** | RGB **carriers** (a plain tier spend would destroy the allocation — terminal-freeze) and **split sub-coins** whose funding is un-broadcast | signed-once backup-chain handover, decrementing `nLockTime` | broadcast the exit branch, then the backup once its absolute locktime matures |

The un-laddered lane is **current and load-bearing for RGB assets** (`sdk52`, `sdk39`) — not
deprecated. Wherever behaviour differs, this guide says which shape it applies to.

Background: [`../PROTOCOL.md`](../PROTOCOL.md), [`../CHILDREN.md`](../CHILDREN.md),
[`../LIGHTNING.md`](../LIGHTNING.md), and the conceptual [`../learn/core-concepts.md`](../learn/core-concepts.md).

## Create / restore

```rust
// New wallet (mnemonic generated)
let (wallet, mnemonic) = UtexoWallet::initialize(SdkConfig::regtest("alice"), None).await?;

// Re-open a wallet whose database_file still exists (same mnemonic)
let (wallet, _) = UtexoWallet::initialize(SdkConfig::regtest("alice"), Some(&mnemonic)).await?;
```

> ⚠️ **The mnemonic ALONE is not a sufficient backup.** It restores the key hierarchy, but off-chain
> statechain funds are only safe if you can *exit* them, and the exit material lives ONLY on your
> disk — the SE cannot re-serve it after a claim: the pre-signed TES-R tier chain (`tesr-*` for a
> coin, `ctesr-*` for a received split child), the un-laddered coins' backup chains and exit
> branches, the terminal-ancestor lists, and (for token wallets) the entire RGB stash under a
> *separate* `rgb.mnemonic` inside `rgb_data_dir`. **Losing `wallet.db` or `rgb_data_dir` is total
> loss of every off-chain coin and token, even with the mnemonic.**

### Full backup / restore

```rust
// Back up EVERYTHING (wallet record + every backup row: tesr-* ladders, ctesr-* child bundles,
// branch-*, parents-* + the RGB seed). Re-export after every transfer/claim/split.
// Store securely — it contains the wallet seed.
let bundle_json = wallet.export_recovery_bundle().await?;
std::fs::write("alice-recovery.json", &bundle_json)?;
// For token wallets, ALSO copy the whole rgb_data_dir (the RGB stash is not embedded in the bundle).

// Restore into a fresh database_file:
let cfg = SdkConfig::regtest("alice"); // point database_file at a fresh path
let (wallet, _) = UtexoWallet::import_recovery_bundle(cfg, &bundle_json).await?;
// Then restore rgb_data_dir contents for token balances.
```

`import_recovery_bundle` refuses to import into a database that already holds a wallet of that name.

## Configuration

`SdkConfig` fields: `wallet_name`, `statechain_entity_url`, `electrum_url` + `electrum_type`,
`network`, sqlite `database_file`, `confirmation_target`, `rgb_proxy_url` + `rgb_data_dir` (token
support; both `None` disables token calls), `poll_interval_secs`, optional `deposit_token_id`,
plus the maintenance knobs:

| Field | Default | What it does |
|---|---|---|
| `auto_refresh` | `true` | Before a spend, re-anchor any coin whose stored backup locktime is within `auto_refresh_margin_blocks` of the tip, so a handover never fails on an aged coin. That floor is a hard constraint only on the **un-laddered** shape (its `nLockTime` budget really does run out); a laddered coin never ages, so for it the pass is precautionary rather than necessary. Set `false` to manage re-anchoring yourself. |
| `auto_refresh_margin_blocks` | `144` | Headroom at which the hook above fires. Must exceed the SE `interval` so a whole-coin handover still validates. |
| `background_auto_refresh` | `false` | Whether the background watcher *also* re-anchors idle coins with no user action. Off, so a running wallet never silently shrinks a balance; renewal is folded into `transfer` and quoted as a payment fee. |
| `auto_exit` | `true` | Run the `auto_exit_due` watchtower pass from the background watcher (protects **un-laddered** off-chain sub-coins and received RGB carriers). |
| `auto_exit_margin_blocks` | `288` | Deadline margin for that pass. |

Presets: `SdkConfig::regtest(name)`, `SdkConfig::mainnet(name, se_url, electrum_url)`.

## Addresses & identity

```rust
let address = wallet.get_utexo_address().await?;       // stable bech32m ml1…/tml1…
let id_pub  = wallet.get_identity_public_key().await?; // stable across coins (m/1000h/0h/0h)
let sig     = wallet.sign_message_with_identity_key(b"hello").await?;
```

## Balance & inventory

```rust
let b = wallet.get_balance().await?;
// b.available_sats  — spendable now
// b.pending_sats    — deposits below confirmation target
// b.in_transfer_sats— outgoing, unclaimed by the receiver
// b.tokens          — Vec<TokenBalance{asset_id, ticker, balance, …}>

let coins = wallet.list_coins().await?;  // CoinInfo{statechain_id, amount_sats, status, off_chain, …}
```

`CoinInfo::off_chain` is true for a coin whose funding tx is un-broadcast (a split sub-coin or an
in-ladder child).

## Deposit

```rust
let addr = wallet.get_deposit_address(100_000).await?;  // fund with exactly 100k sats
```

The background watcher confirms and activates it (`DepositConfirmed` event). **`claim()` ladders
every fresh confirmed root coin unconditionally** — `T`, `X_m` and `S_0` are built and co-signed at
that point and a `LadderEstablished` event fires. Nothing is broadcast; the coin now sits off-chain
indefinitely. (`deposit_protocol_version` and the `UTEXO_PROTOCOL_DEFAULT` env no longer exist —
there is no lane to choose.)

Deposit slots consume SE deposit tokens: the SDK fetches them; if the SE charges, you get
`SdkError::TokenPaymentRequired { token_id, deposit_address, fee_sats }` — pay it and retry, or
pre-pay and pool with `wallet.add_prepaid_token(&token_id)`. Split/combine/refresh slots are
**derived** (free) against the parent coin, so only onboarding costs a token.

## Transfer

```rust
let r = wallet.transfer(&bob_address, 15_000).await?;
// r.total_sats, r.coins (per-coin), r.used_split == true when the amount was not an exact subset
```

`transfer` picks one of three routes automatically:

1. **Exact subset** — a subset of confirmed coins sums to the amount: each is handed over whole
   (laddered coins co-sign a fresh lower-CSV state; un-laddered coins do a backup-chain handover).
2. **Non-exact, laddered parent → in-ladder split** (`in_ladder_pay`): a STATE tier `SP` spends
   `X_m.out[0]` — a **descendant of the trigger**, never a rival for `F` — and pays two children,
   the PIECE to the recipient and the CHANGE back to you. The piece's child bundle is conveyed
   through the mailbox; the recipient's `claim()` adopts it with the standard SE key handover.
   Nothing is broadcast (`sdk58`, `sdk59`).
3. **Non-exact, received child parent → child-level in-ladder split** (`child_in_ladder_pay`): the
   child's state is replaced by a split state paying two grandchildren, and the child becomes an
   intermediate segment in each grandchild's ancestors chain (depth 2 — `sdk17`).

### The in-ladder admission floor

Both the piece and the change must be at least `min_child_value` — **1306 sat at the default
2 sat/vB** — because each child funds its **own** extension + state tier before its final output can
clear the 330-sat dust limit. (The un-laddered `min_split_output` floor — dust + the sub-coin's own
backup fee — also applies; the larger binds.) The SDK refuses up front rather than terminalizing the
parent and failing afterwards:

```
in-ladder split needs both piece (900) and change (…) >= 1306 sat
(each child funds its own extension + state tier at 2 sat/vB, then must clear the 330-sat dust floor)
```

### Quoting

```rust
let q = wallet.quote_transfer(15_000).await?;
// q.network_fee_sats + q.renewal_fee_sats == q.total_fee_sats  (one fee, shown like a payment fee)
// q.fundable, q.stuck_coins (coins worth less than their own re-anchor fee — combine to rescue)
```

`renewal_fee_sats` is `0` unless a coin this send would use is actually due for an on-chain
re-anchor.

### Multi-recipient

```rust
// One off-chain split carves one piece per recipient plus your change; each piece is handed over.
let results = wallet.transfer_many(&[(bob_addr, 10_000), (carol_addr, 25_000)]).await?;
```

### Received split children are first-class

A child you received is fully yours: its claim completed the SE key handover, so you co-own
`A_child` (invariant across the rotation, which is what keeps the pre-signed exit chain valid) and
the sender is permanently locked out. Pay it onward off-chain with no on-chain footprint —

- **whole** → `transfer` routes it to `child_retransfer`,
- **in part** → `transfer` routes it to `child_in_ladder_pay`.

Each hop costs one co-signature and discloses one superseded state for the receiver's census
(`sdk60` alice→bob→carol with the funding outpoint unspent throughout; `sdk17` a partial second
hop). One limit: a child cannot be **cooperatively** withdrawn to an arbitrary L1 address — its
funding `SP.out[j]` is un-broadcast, so there is no confirmed outpoint to spend. `withdraw` routes a
child to the unilateral exit instead (see below).

### Explicit split / in-ladder pay

```rust
// Un-laddered parent only: one SE-co-signed, un-broadcast tx into two single-use sub-coins.
let (piece_id, change_id) = wallet.split_coin(&statechain_id, 15_000).await?;
let exact_id = wallet.ensure_exact_coin(30_000).await?;  // mints one via split_coin when needed

// Laddered parent: the split IS the payment (piece conveyed, change kept).
use mercury_utexo_sdk::transfer::InLadderLatch;
let (piece_sid, change_sid, _latch) = wallet
    .in_ladder_pay(&parent_sid, &bob_address, 15_000, InLadderLatch::None)
    .await?;
```

`split_coin` **hard-refuses a laddered coin**: a prior owner may hold a no-timelock trigger over the
same funding outpoint and could void a plain split, destroying the payee's sub-coin. `ensure_exact_coin`
only ever picks un-laddered parents for the same reason and errors with *"no splittable coin large
enough … coins carrying a TES-R exit ladder cannot be split"* when the wallet holds only laddered coins.
Use `transfer` (which routes automatically) or `in_ladder_pay`. The `InLadderLatch` variants other
than `None` are the Lightning lanes — see below.

## Receive (auto-claim + events)

```rust
let mut events = wallet.subscribe();
let handle = wallet.start_background();          // poll loop; abort() to stop
while let Ok(ev) = events.recv().await {
    match ev {
        WalletEvent::TransferClaimed { statechain_ids } => { /* sats arrived */ }
        WalletEvent::TokenTransferClaimed { asset_id, amount, .. } => { /* tokens arrived */ }
        WalletEvent::DepositConfirmed { amount_sats, .. } => { /* deposit live */ }
        WalletEvent::LadderEstablished { statechain_id } => { /* TES-R ladder built at claim */ }
        WalletEvent::BalanceUpdate { balance } => { /* refresh UI */ }
        WalletEvent::LadderDefended { statechain_id, tiers_broadcast } => { /* contested exit */ }
        WalletEvent::ExitDeadlineApproaching { statechain_id, .. } => { /* un-laddered sub-coin */ }
        WalletEvent::TokenCarrierMaterialized { statechain_id, .. } => { /* carrier settled on L1 */ }
        WalletEvent::ExitBranchConflict { statechain_id } => { /* someone raced your exit */ }
        WalletEvent::CoinRefreshed { new_statechain_id, fee_sats, .. } => { /* re-anchored */ }
    }
}
// One-shot instead of background: wallet.claim().await?
```

`claim()` is what adopts an incoming state or child bundle: it verifies the bundle (the adopted
state must carry the strictly-lowest CSV), runs the census over the disclosed superseded states, and
completes the key handover.

## Utexo invoices

```rust
let inv = wallet.create_sats_invoice(15_000, Some("coffee".into()), None).await?;   // utexoinv1…
let inv = wallet.create_tokens_invoice(&asset_id, 250, None, None).await?;
let r   = payer.fulfill_utexo_invoice(&inv).await?;   // decodes, checks expiry, transfers
```

## Tokens

RGB carriers are **never laddered** — a plain tier spend would destroy the allocation. They keep the
signed-once backup shape and transfer by backup-chain handover (`sdk52`). The SDK enforces this: a
carrier is excluded from plain-BTC coin selection, from `withdraw`/`unilateral_exit` defaults (and
hard-errors if named explicitly), from Lightning swap selection, and from `auto_refresh`.

```rust
// hold + send (issuance: see the issuer guide, issuer-sdk.md)
let tokens = wallet.get_token_balances().await?;
wallet.transfer_tokens(&asset_id, &bob_address, 250).await?;
// If no single carrier covers the amount, this combines several carriers of the asset into ONE
// SE-co-signed colored tx (piece + change); every combined carrier is made terminal first.

// Multi-recipient colored split: ONE SE-co-signed tx carves one piece per recipient
// (its exact amount) plus this wallet's change; each piece ships its own consignment.
// Returns one TransferResult per recipient, in order.
let results = wallet
    .batch_transfer_tokens(&asset_id, &[(bob_address.clone(), 250), (carol_address, 100)])
    .await?;
```

## Lightning

Lightning works **both directions** through an SSP (Utexo Service Provider: a statechain wallet plus
an RLN node), latched to a HODL invoice. `ssp` is `&impl Ssp` — the same call works against an
in-process `SspService` or a remote `SspClient` over HTTP (`sdk21`).

```rust
// PAY (Utexo -> Lightning). Trustless both ways: no payment -> the latch expires, the coin is yours.
let preimage = wallet.pay_lightning_invoice(&ssp, bolt11).await?;  // proof of payment

// RECEIVE (Lightning -> Utexo). Hand the invoice to the payer; the coin arrives via claim().
let swap = wallet.create_lightning_invoice(&ssp, 25_000).await?;   // swap.invoice, swap.payment_hash
```

`pay_lightning_invoice` mints an exact coin when it can; when the wallet holds only laddered coins
of the wrong size it **auto-routes to the non-exact in-ladder lane** — the piece is split off
in-ladder, latched to the invoice hash, and censused by the SSP before it pays (`sdk63` pay,
`sdk64` receive, `sdk65` non-exact pay, `sdk67` non-exact receive). Call
`pay_lightning_invoice_inladder(&ssp, bolt11, &parent_sid)` to pick the parent yourself.

The LN-latched piece is the **one** case that stays terminalized — it sits unclaimed past the
pending-transfer lock's window, so it is deliberately frozen rather than left re-transferable.

### Failure handling

```rust
// Same as pay_lightning_invoice, but the error carries the latched coin id so you can reclaim it.
match wallet.pay_lightning_invoice_reclaimable(&ssp, bolt11).await {
    Ok(preimage) => { /* paid */ }
    Err((coin_id, e)) if !coin_id.is_empty() => {
        // ONLY after positively confirming the SSP did NOT pay, and after the SE batch_timeout:
        wallet.reclaim_lightning_payment(&coin_id).await?;
    }
    Err((_, e)) => return Err(e), // failed before anything was latched
}
```

Read the safety note on `reclaim_lightning_payment` before automating it: a client-side timeout, or
an error *after* the SSP revealed the preimage, are **not** proof of non-payment. Two shape-specific
outcomes:

- **Non-exact (in-ladder) pay failure** rolls the un-broadcast split back: the parent is restored as
  exitable and the conveyed piece + optimistic change bookings are dropped, so the whole parent
  value is recovered (`sdk66`, `sdk68`).
- **Exact laddered pay failure** leaves an orphan co-signed `S'`, so the coin is restored locally as
  exitable (value intact via unilateral exit) but **off-chain re-transfer stays bricked until you
  `refresh()`** it. Un-laddered coins reclaim cleanly off-chain via a self-transfer.

Adversarial coverage: `sdk19`, `sdk20`, `sdk24`, `sdk25`; RGB-over-Lightning: `sdk23`.

### Low-level latch legs

If you are building the counterparty side (an LSP) rather than using an SSP:

```rust
let swap = wallet.start_lightning_swap(&lsp_address, None).await?;
// LSP verifies get_swap_payment_hash(&swap.batch_id), then pays the invoice for swap.payment_hash
let preimage = wallet.settle_lightning_swap(&swap).await?;  // unlocks the coin for the LSP
```

## Exit

```rust
// Cooperative (immediate, 1 tx per coin, SE co-signed, no timelock wait):
wallet.withdraw("bc1p…", None /* all coins */, None /* fee rate */).await?;
let q = wallet.get_withdrawal_fee_quote(None).await?;   // n_coins, est_vbytes, fee_sats

// Unilateral (no SE cooperation needed):
let statuses = wallet.unilateral_exit(None, None).await?;
```

`unilateral_exit` is **incremental and idempotent**, because a laddered exit is a walk down the
pre-signed chain:

- **Laddered coin** — broadcast `T` (spending `F`), then each `X_m`/`S_k` as its relative CSV
  matures. Each call advances as far as maturity allows and returns
  `ExitStatus { complete, wait_blocks }`; call it once per block until `complete` (`sdk50`).
- **Received child** — the same walk over the full chain `T → X_m → SP → ext_child → state_child`,
  keyless (every tier is already signed and the final state pays your own key).
- **Un-laddered coin** — broadcast the locktime-free exit branch, then the latest backup once its
  absolute `nLockTime` matures; `wait_blocks` reports the remaining blocks.

Guards: a coin that is not `CONFIRMED` is refused (exiting a parent already consumed by a split
would invalidate the sub-coins it funded), and an RGB carrier is refused outright — an RGB-unaware
sweep destroys the allocation (materialize it instead; see below).

```rust
let est = wallet.estimate_exit_cost(&statechain_id).await?;
// est.total_vbytes, est.fee_sats_at(rate)
// est.wait_blocks         — when the exit COMPLETES
// est.exit_deadline_block — the SAFETY deadline for an UN-LADDERED off-chain sub-coin: the earliest
//                           height an ancestor could broadcast a stale backup. None for a coin with
//                           no off-chain ancestor. Treat it as an upper bound and act with margin.
```

## Watchtowers

Two independent passes, both default-on inside `start_background()`:

```rust
// Laddered coins: no-op while the coin is idle (F unspent — nothing has been triggered, nothing
// ages). If someone DID trigger it (a contested exit), this races your tiers; your adopted state
// carries the strictly-lowest CSV so it matures first and the funds land at your own key.
// Idempotent — call once per block. Emits LadderDefended.
let acted = wallet.defend_ladders().await?;

// Un-laddered off-chain sub-coins + received RGB carriers: force-exit / MATERIALIZE anything within
// margin_blocks of its exit-race deadline, before an ancestor's stale backup can be broadcast.
let acted = wallet.auto_exit_due(288).await?;
```

Watching can be **delegated without custody** — everything a tower broadcasts is already fully
signed and pays only the owner:

```rust
let bundle_json = wallet.export_watch_bundle().await?;  // NO mnemonic, NO key shares, NO RGB seed
// Run mercury_utexo_sdk::watch_pass(&bundle, &electrum, margin_blocks) on a cron anywhere, from
// any number of machines — no wallet database, no SE, no keys. Broadcasts are idempotent and every
// tower broadcasts the SAME transactions, so a second tower can never conflict with the first.
// Carriers are exported WITHOUT their backup tx, so a tower structurally cannot do the
// token-destroying sweep (sdk45, sdk51, sdk34).
//
// It returns a `WatchState`: `Idle` (the tip WAS read and nothing is due), `Acted { ids, failures }`
// or `Blind { reason }`. Alert on `Blind` — a tower that cannot see the chain is watching nothing,
// and before this it was indistinguishable from a quiet one.
```

Re-export the watch bundle after any transfer/claim/split, like the recovery bundle.

## Refresh (re-anchor)

`refresh` moves a coin to a **fresh funding outpoint** with ONE SE-co-signed on-chain transaction:
the coin's current outpoint is spent into a fresh deposit aggregate (new `statechain_id`, same
owner) and `claim()` mints a brand-new ladder on it. Because the old outpoint is now spent, every
previous owner's pre-signed material against it is permanently dead.

It is **not** a deadline reset — a laddered coin has no deadline to reset (`sdk43`: renewal and
rollover are off-chain and unbounded). Reach for it when you actually need a new on-chain root:

- to **un-brick** a coin whose exact-lane Lightning pay failed (its orphan `S'` blocks off-chain
  re-transfer until re-anchored);
- to reset an **un-laddered** coin whose decrementing-`nLockTime` backup chain is nearing its floor
  (a receiver rejects a backup at or below the tip — `MercuryError::LocktimeTooLow`);
- to consolidate an off-chain coin back onto a confirmed outpoint.

Refresh is **cooperative** (it needs the SE); if the SE is gone, exit unilaterally instead. The coin
must be `CONFIRMED`, carry no RGB allocation, and be large enough to cover the fee above the dust
floor (`sdk30`).

```rust
// User pays: the on-chain fee comes from the coin, so the refreshed coin is amount − fee.
// fee_rate (sat/vB) is capped at max_fee_rate; None uses the SE-quoted rate.
let r = wallet.refresh(&statechain_id, None).await?;
// r.old_statechain_id (now spent) / r.new_statechain_id / r.new_amount_sats == amount − fee

// Operator pays: same on-chain re-anchor, then a funded `sponsor` wallet reimburses the fee
// OFF-CHAIN (instant, free) so the user's total balance ends ≥ whole. r.rebate_sats reports the
// amount rebated: max(fee + dust, min_child_value) — the rebate must itself be an off-chain-payable
// piece, and on a laddered sponsor coin that means clearing the 1306-sat in-ladder floor.
let r = wallet.refresh_sponsored(&statechain_id, &sponsor, None).await?;
```

The fresh coin confirms asynchronously (watcher/`claim()`), like a deposit.

## History

```rust
let activities = wallet.get_activities().await?;  // deposits, sends, receives
let transfers  = wallet.get_transfers().await?;
let one        = wallet.get_transfer(&utxo).await?;
```

## Errors worth handling

| Error | Meaning | Action |
|---|---|---|
| `TokenPaymentRequired` | SE charges for deposit slots | pay `fee_sats` to `deposit_address`, retry |
| `InsufficientBalance` | amount > spendable | top up / lower amount |
| `NoExactAmount` | no exact subset and split was disabled for the call | allow the split, or pick a different amount |
| `TokensNotConfigured` | token call without RGB config | set `rgb_proxy_url` + `rgb_data_dir` |
| `CoinBelowMaintenanceCost` | the coin is worth less than its own re-anchor fee | not lost — combine it with another coin so the aggregate covers the fee |

Non-typed errors worth recognising: the in-ladder floor message (`… >= 1306 sat`) means the piece or
change was too small to fund its own tiers; `LocktimeTooLow` on an un-laddered handover means that
coin needs a `refresh` first.

## Testing

Live SDK end-to-end dispatch is `SDK_E2E=1..68` (with gaps where tests were retired) plus
`chaos22`, run from `clients/tests/rust`. See [`testing-guide.md`](testing-guide.md) for the run
environment and per-suite invocation.
