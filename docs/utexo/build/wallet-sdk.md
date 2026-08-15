# Wallet SDK guide

Crate `mercury-utexo-sdk` (`clients/libs/rust-sdk`). Every method below is on `UtexoWallet` unless
the snippet says otherwise. Free functions and types are re-exported from the crate root
(`clients/libs/rust-sdk/src/lib.rs`).

Normative behaviour lives in [`../spec/SPEC.md`](../spec/SPEC.md),
[`../spec/PROTOCOL.md`](../spec/PROTOCOL.md),
[`../spec/CHILDREN.md`](../spec/CHILDREN.md), [`../spec/LIGHTNING.md`](../spec/LIGHTNING.md),
[`../spec/TRUST-MODEL.md`](../spec/TRUST-MODEL.md) and
[`../spec/PARTIAL-PAYMENT-ECONOMICS.md`](../spec/PARTIAL-PAYMENT-ECONOMICS.md). Where this guide and
a spec document disagree, the spec is right.

## What a coin is (read this first)

There is **one protocol**. Every plain BTC deposit is **laddered** at claim — three tiers of
pre-signed, **un-broadcast** v3/TRUC transactions with P2A anchors:

```
F (on-chain funding, 2-of-2 owner+SE)
└─ T   TRIGGER    no timelock, signed once at claim
   └─ X_m EXTENSION  relative CSV E_m  — renewal replaces it horizontally, off-chain
      └─ S_k STATE   relative CSV Δ_k  — decrements by δ on every transfer
```

Four consequences the API depends on:

- **An idle coin never ages on the CSV side.** BIP-68 relative timelocks only start counting once
  the parent confirms, and `T` has no timelock, so nothing matures until someone broadcasts `T`.
  0 vB of idle rent.
- **A coin still has one absolute calendar.** Alongside the tiers, the coin retains a **flat backup
  chain** over `F` with *absolute* locktimes `L_k = L_0 − k·interval`, and prior owners hold rungs of
  it. `min(L_k)` is the coin's **epoch expiry**: once that height passes, an ancestor's matured rung
  can spend `F` and take the coin. Mining consumes it, and each whole-coin hop consumes `interval`
  more. This is why `deadline_safety_due` runs unconditionally in `start_background()` — see
  [Maintenance](#maintenance-and-watchtowers). Do not build UI that says a laddered coin cannot
  expire.
- **A transfer co-signs a fresh state one δ *lower*** than the one it replaces, so the new owner's
  state matures first; the superseded state is disclosed and counted by the receiver's census.
  Nothing goes on-chain.
- **Renewal and rollover are off-chain and unbounded.** `refresh` is the on-chain **re-anchor**
  primitive — one transaction that moves the coin to a fresh outpoint and mints a fresh flat chain.

**Not every coin is laddered, by design.** Four shapes coexist in the API:

| Shape | Which coins | Non-exact payment route | Exit |
|---|---|---|---|
| **Laddered root** | every plain BTC deposit | `in_ladder_pay` — a state tier `SP` over `X_m.out[0]` | walk `T → X_m → S_k`, waiting out each relative CSV |
| **Received child** | a piece someone paid you | `child_in_ladder_pay` — `CSP` at the child's own level | walk `T → X_m → SP → ext_child → state_child` |
| **Spine tip** | your own change leg from an in-ladder payment | `spine_batch_pay` — the next batch over `SP_i.out[K]` | walk down to the tip's single cap tier |
| **Un-laddered** | RGB **carriers** (a plain tier spend destroys the allocation) and plain split sub-coins whose funding is un-broadcast | `split_coin` — a plain off-chain split | broadcast the exit branch, then the backup once its absolute locktime matures — **except a carrier**, which `unilateral_exit` refuses (see [Tokens](#tokens)) |

`ParentShape` (`clients/libs/rust-sdk/src/transfer.rs`) is the single resolution of that question,
and it selects the route AND the value floor together — so `quote_transfer` and `transfer` cannot
answer differently. It is an internal type: an app never names a shape, it calls `transfer` and
reads the outcome.

## Create / restore

```rust
use mercury_utexo_sdk::{SdkConfig, UtexoWallet};

// New wallet (mnemonic generated)
let (wallet, mnemonic) = UtexoWallet::initialize(SdkConfig::regtest("alice"), None).await?;

// Re-open a wallet whose database_file still exists (a differing mnemonic argument is an error)
let (wallet, _) = UtexoWallet::initialize(SdkConfig::regtest("alice"), Some(&mnemonic)).await?;
```

> ⚠️ **The mnemonic ALONE is not a sufficient backup.** It restores the key hierarchy, but off-chain
> funds are only safe if you can *exit* them, and the exit material lives ONLY on your disk — the SE
> cannot re-serve it after a claim. That is the pre-signed tier chain (`tesr-*` for a root coin,
> `ctesr-*` for a received child, `spinetip-*` for a change tip), the un-laddered coins' backup
> chains and exit branches (`branch-*`), the terminal-ancestor lists (`parents-*`), and — for token
> wallets — the entire RGB stash under a *separate* `rgb.mnemonic` inside `rgb_data_dir`. **Losing
> `wallet.db` or `rgb_data_dir` is total loss of every off-chain coin and token, even with the
> mnemonic.**

```rust
// Back up EVERYTHING: wallet record + every backup row + the RGB engine seed.
// Re-export after every transfer/claim/split. Store securely — it contains the wallet seed.
let bundle_json = wallet.export_recovery_bundle().await?;
std::fs::write("alice-recovery.json", &bundle_json)?;
// For token wallets, ALSO copy the whole rgb_data_dir (the stash is not embedded in the bundle).

// Restore into a fresh database_file:
let cfg = SdkConfig::regtest("alice"); // point database_file at a fresh path
let (wallet, _) = UtexoWallet::import_recovery_bundle(cfg, &bundle_json).await?;
```

`import_recovery_bundle` refuses to import into a database that already holds a wallet of that name.

## Configuration

`SdkConfig` (`clients/libs/rust-sdk/src/config.rs`). Connection and storage:
`wallet_name`, `statechain_entity_url`, `electrum_url` + `electrum_type`, `network`, sqlite
`database_file`, `confirmation_target`, `poll_interval_secs`, `rgb_proxy_url` + `rgb_data_dir`
(**both** must be `Some` for any token call to work), optional `deposit_token_id`.

Behaviour knobs and their shipped defaults:

| Field | `regtest` | `mainnet` | What it does |
|---|---|---|---|
| `auto_refresh` | `true` | `true` | Run the pre-spend re-anchor hook inside `transfer` / `transfer_many`, so a coin near its flat-chain floor is refreshed before it is selected. |
| `auto_refresh_margin_blocks` | `144` | `144` | Headroom at which that hook — and the background `deadline_safety_due` pass — fires. Must exceed the SE `interval` so a whole-coin handover still validates. |
| `background_auto_refresh` | `false` | `false` | **Read by nothing at runtime.** `maintenance_plan` returns `MaintenancePass::DeadlineSafety` unconditionally, so deadline defence is never gated on an economics flag. Kept as a field; do not build behaviour on it. |
| `auto_exit` | `true` | `true` | Run the `auto_exit_due` watchtower pass from the background watcher. |
| `auto_exit_margin_blocks` | `860` | `2_120` | **Derived, not chosen** — `auto_exit_margin_blocks_for(AUDIT_17_K_MAX, interval, AUTO_EXIT_MODELLED_DEPTH)` = `k_max·interval + tesr_exit_txs(d)·144`. The two networks differ because the SE `interval` does (10 vs 100). |
| `fee_bump` | `None` | `None` | `Some(FeeBumpConfig{..})` gives `unilateral_exit` / `defend_ladders` a fee float and a Core RPC endpoint so a tier stuck under the relay floor can be escalated to a 1P1C package. `None` means the wallet **cannot** bump and says so rather than retrying at the committed rate forever. The key funds fees only — never a coin key. |
| `colored_ladder` | `false` | `false` | Off. See [Tokens](#tokens): with it off, an RGB carrier is never laddered and its transfers take the flat RGB-aware split lane. |
| `attestation_identity` | `None` | `None` | The enclave identity a laddering claim verifies sig-count attestations against. Resolution is **compiled-in pin → this field → `UTEXO_ATTESTATION_IDENTITY` → refuse**. With no pin available, `claim()` records `LadderSkipReason::AttestationIdentityUnpinned` and establishes no ladder — the correct direction to fail. |

Presets: `SdkConfig::regtest(name)` (SE at `127.0.0.1:8000`, RGB enabled) and
`SdkConfig::mainnet(name, statechain_entity_url, electrum_url)` (RGB fields `None`). The per-tx fee
cap `max_fee_rate` lives on the underlying `ClientConfig`, reachable via `wallet.client_config()`.

## Addresses & identity

```rust
let address = wallet.get_utexo_address().await?;        // stable bech32m ml1…/tml1…, reusable
let id_pub  = wallet.get_identity_public_key().await?;  // stable across coins
let sig     = wallet.sign_message_with_identity_key(b"hello").await?;

// Verification is an associated function — no wallet needed.
let ok = UtexoWallet::validate_message_with_identity_key(b"hello", &sig, &id_pub)?;
```

## Balance & inventory

```rust
let b = wallet.get_balance().await?;
// b.available_sats   — spendable now (confirmed coins)
// b.pending_sats     — detected, below the confirmation target
// b.in_transfer_sats — outgoing, unclaimed by the receiver
// b.tokens           — Vec<TokenBalance { asset_id, ticker, name, precision, balance, total }>

let coins = wallet.list_coins().await?;
// CoinInfo { statechain_id, amount_sats, status, utxo_txid, utxo_vout, off_chain }
```

`get_balance` **fails closed** on a token wallet whose RGB state cannot be read: counting a carrier's
sats as spendable BTC would invite an allocation-destroying spend, so it returns `Err` rather than a
plausible number. `CoinInfo::off_chain` is true when the coin's funding transaction is un-broadcast
(a plain split sub-coin, an in-ladder child, a spine tip).

Two queries answer *"can this coin be conveyed on the ladder lane?"* from persisted state, which is
the authority — the `LadderSkipped` event is transition-only and an app that starts late never sees
it:

```rust
let reason: Option<LadderSkipReason> = wallet.ladder_skip_reason(&statechain_id).await;
let raw:    Option<String>           = wallet.ladder_skip_reason_raw(&statechain_id).await;
// (statechain_id, reason_string, permits_flat_conveyance) for every flat-only coin
let flat: Vec<(String, String, bool)> = wallet.flat_only_coins().await?;
```

`LadderSkipReason::permits_flat_conveyance()` is a **prediction**, not the decision — the authority
is `mercuryrustlib::transfer_sender::assert_flat_conveyance_is_legitimate`, which re-proves the
licence from live evidence at conveyance time. `false` is reliable; `true` means "provided the
evidence is still there".

## Deposit

```rust
let addr = wallet.get_deposit_address(100_000).await?;  // fund with exactly 100k sats
```

The background watcher detects and confirms it (`DepositConfirmed`). At `claim()` the coin is
laddered — `T`, `X_0` and `S_0` are built and co-signed, and `LadderEstablished` fires. Nothing is
broadcast; the coin sits off-chain. Laddering is attempted for every fresh confirmed root coin
(`sdk71`); when it cannot be done the reason is recorded and surfaced as `LadderSkipped` +
`ladder_skip_reason`, and some reasons are permanent for the coin (`RgbCarrier`,
`FundingNotOnChain`, `DuplicateDeposit`, `AttestationIdentityUnpinned`) while others clear on a later
pass (`CoordinatorUnavailable`, `FundingUnresolvable`, `RgbStateUnavailable`).

Each fresh **on-chain onboarding** slot consumes an SE deposit token. The SDK fetches one; if the SE
charges you get `SdkError::TokenPaymentRequired { token_id, deposit_address, fee_sats }` — pay it and
retry, or pre-pay and pool:

```rust
wallet.add_prepaid_token(&token_id).await;   // returns (), pooled for the next onboarding slot
```

Slots minted by SE-co-signed flows over an existing statechain — split outputs, combine outputs,
refresh re-anchors — draw **free derived tokens** vouched by the parent coin instead. A coin may
vouch at most `wallet::DERIVED_SLOTS_PER_STATECHAIN` = **64** derived slots over its lifetime, which
is what bounds a batch to `wallet::MAX_BATCH_RECIPIENTS` = **63** recipients.

## Transfer

```rust
let r = wallet.transfer(&bob_address, 15_000).await?;
// r.receiver_address, r.total_sats, r.coins: Vec<TransferredCoin>, r.used_split
```

`transfer` runs the pre-spend auto-refresh hook, then plans with the same `plan_payment` call
`quote_transfer` uses, then dispatches:

1. **Exact subset** — a subset of confirmed coins sums to the amount and each is handed over whole.
   A laddered coin co-signs a fresh lower-CSV state; a received child goes through
   `mercuryrustlib::tesr::child_retransfer`; an un-laddered coin does a backup-chain handover via
   `transfer_sender::execute`.
2. **Non-exact, laddered root** → `in_ladder_pay`. A STATE tier `SP` spends `X_m.out[0]` — a
   **descendant of the trigger**, never a rival for `F` — and pays the payee's PIECE plus the
   sender's CHANGE. The piece's child bundle is conveyed through the mailbox with the key-handover
   material; the recipient's `claim()` adopts it (`sdk58`, `sdk59`).
3. **Non-exact, received child** → `child_in_ladder_pay`. The child is terminalized and becomes an
   intermediate segment in each grandchild's ancestor chain (depth 2 — `sdk17`, `sdk76`).
4. **Non-exact, spine tip** → `spine_batch_pay`, i.e. the next batch `SP_{i+1}` over the tip's own
   funding outpoint `SP_i.out[K]`. This is the arm that keeps a wallet spendable: after the first
   partial payment the sender holds a tip, so every subsequent payment out of that coin comes
   through here.
5. **Non-exact, un-laddered** → `split_coin`, the plain off-chain split.

**Handing a spine tip over whole is refused by name.** A tip's funding output is un-broadcast and
there is no builder for a spine-tip conveyance, so `transfer` errors rather than falling through to
the flat lane —
which would hand the recipient a backup chain over an outpoint that will never exist on chain. The
coin is untouched and still unilaterally exitable by this wallet.

### The in-ladder admission floor

**Two legs, two floors.** One internal function, `split_output_floors(backup_rate, shape)`, returns
`SplitFloors { piece, change, lane }` for both the quote and the executor, and the numbers are
**rate evaluations, not constants** — quoting one without its rate is quoting a rate. An app does
not call it; it reads `quote_transfer` or the refusal text below.

- A payee's **piece** is always a two-tier child (`establish_child` hangs an extension *and* a state
  off `SP.out[j]`), so it must clear `mercurylib::tesr::min_child_value(rate, dust)` =
  `2·(committed_fee(rate) + P2A) + dust` — **1 560 sat** at the shipped `committed_fee_rate` of
  3.0 sat/vB.
- The sender's **change** clears whatever shape that lane's builder actually gives it, reported by
  `mercuryrustlib::tesr::change_leg_role(lane)`. Three lanes — `PlainRoot`, `SpineBatch` and
  `Colored` — build a one-cap **spine tip**, so their change floor is
  `mercurylib::tesr::min_spine_tip_value` = `committed_fee(rate) + P2A + dust` = **945 sat** at the
  same rate. Only `PlainChild` builds a two-tier change, and it floors it at `min_child_value`.
- The un-laddered floor `min_split_output(backup_rate)` = dust + the sub-coin's own 112-vB backup fee
  applies to every leg as well, and the **larger binds**.

One shared number could only reach the tip's floor by lowering the piece's floor with it, which
admits a piece that cannot fund its second rung — discovered inside `establish_child`, *after* the
parent has been terminalized, stranding it to unilateral-exit-only. So the SDK refuses up front, per
leg, naming which leg fell short:

```
in-ladder split refused — the piece falls short. The payee's piece (900) must be >= 1560 sat
(it funds its own extension + state rungs) and the change (…) must be >= 945 sat (…); both must
then clear the 330-sat dust floor. The split total is …
```

### Quoting

```rust
let q = wallet.quote_transfer(15_000).await?;
// q.amount_sats
// q.network_fee_sats + q.renewal_fee_sats == q.total_fee_sats  (one fee, shown like a payment fee)
// q.fundable
// q.stuck_coins             — worth less than their own re-anchor fee; combine to rescue
// q.no_exit_material_coins  — no TES-R bundle AND no flat backup rows; combining does NOT rescue
// q.note                    — a human-readable explanation of the verdict
```

`renewal_fee_sats` is `0` unless a coin this send would use is actually due for an on-chain
re-anchor. Both quote reads (the carrier set and the backup fee rate) **propagate** their errors: a
defaulted-empty carrier set or a defaulted fee rate would produce a lower floor than the executor
uses, which is the quote disagreeing with the executor.

`stuck_coins` and `no_exit_material_coins` are different problems. A stuck coin has a fee problem;
combining it with another coin rescues it. A no-exit-material coin is missing the material itself,
and `transfer` names it in the refusal rather than reporting a bare "insufficient balance".

### Multi-recipient

```rust
let results = wallet
    .transfer_many(&[(bob_addr, 10_000), (carol_addr, 25_000)])
    .await?;   // one TransferResult per recipient, in order
```

`transfer_many` dispatches on the same `ParentShape`: `in_ladder_pay_many` for a laddered root,
`child_in_ladder_pay_many` for a received child, `spine_batch_pay_many` for a spine tip, the plain
`N+1`-output split for an un-laddered coin (`sdk69`). It refuses a list longer than
`MAX_BATCH_RECIPIENTS` locally, by name, with `SdkError::BatchTooManyRecipients` — before any SE
call, because the coordinator's own refusal is a bare 400 that arrives after the caller has already
committed to a recipient list.

### Received children are first-class

A child you received is fully yours: its claim completed the SE key handover, so you co-own
`A_child` (invariant across the rotation, which is what keeps the pre-signed exit chain valid) and
the sender is permanently locked out. Pay it onward off-chain with no on-chain footprint — whole
(`transfer` → `child_retransfer`) or in part (`transfer` → `child_in_ladder_pay`). Each hop
co-signs one lower-CSV state and discloses one superseded state for the receiver's census (`sdk60`
alice→bob→carol with the funding outpoint unspent throughout; `sdk17` a partial second hop).

One limit: a child cannot be **cooperatively** withdrawn to an arbitrary L1 address — its funding
`SP.out[j]` is un-broadcast, so there is no confirmed outpoint to spend. `withdraw` routes a child —
and a spine tip, for the same reason — to the unilateral exit instead, and marks the coin
`WITHDRAWING` for the multi-block walk.

### Explicit split / in-ladder pay

```rust
use mercury_utexo_sdk::transfer::InLadderLatch;

// Un-laddered parent only: one SE-co-signed, un-broadcast tx into two single-use sub-coins.
let (piece_id, change_id) = wallet.split_coin(&statechain_id, 15_000).await?;
let exact_id = wallet.ensure_exact_coin(30_000).await?;   // mints one via split_coin when needed

// Laddered root: the split IS the payment (piece conveyed, change kept as a spine tip).
let (piece_sid, change_sid, latch) = wallet
    .in_ladder_pay(&parent_sid, &bob_address, 15_000, InLadderLatch::None)
    .await?;   // latch: Option<(String, String)>, None for InLadderLatch::None

// Received child, and a spine tip:
let (piece_sid, change_sid) = wallet.child_in_ladder_pay(&child_sid, &bob_address, 15_000).await?;
let (piece_sid, change_sid) = wallet.spine_batch_pay(&tip_sid, &bob_address, 15_000).await?;
```

`split_coin` **hard-refuses a laddered coin**: a prior owner may hold a no-timelock trigger over the
same funding outpoint and could void a plain split, destroying the payee's sub-coin. It also refuses
an RGB carrier. `ensure_exact_coin` only ever picks un-laddered parents with proven exit material,
for the same reason, and errors with *"no splittable coin large enough … coins carrying a TES-R exit
ladder cannot be split"* when the wallet holds only laddered coins. Prefer `transfer`, which routes
automatically. `InLadderLatch` variants other than `None` are the Lightning lanes.

### Cancelling an opened transfer

The coordinator's pending-transfer lock is what stops a sender from co-signing a rival state while a
recipient holds claimable material, so cancellation is not a power the sender simply has. If the
mailbox message was never posted, the sender alone may withdraw it; once posted, the **recorded
recipient must co-sign**. There is no force flag. The lock is also the *only* server-side hold on
that path and it expires on a wall clock — which is the receiver's reason to claim promptly, and the
sender's reason not to treat "the recipient will not consent" as permanent.

```rust
use mercury_utexo_sdk::{CancelNeedsRecipientConsent, CancelOutcome};

match wallet.cancel_transfer(&statechain_id).await {
    Ok(CancelOutcome::Cancelled | CancelOutcome::AlreadyCancelled) => { /* coin is CONFIRMED again */ }
    Err(e) => match e.downcast_ref::<CancelNeedsRecipientConsent>() {
        // .recipient_auth_pub_key is Option<String> — the key the coordinator RECORDED as this
        // transfer's recipient, and the only thing that makes cross-wallet cancellation actionable.
        // None means the coordinator declined to name it, so there is nothing to route a request to.
        Some(needs) => { /* route a consent request to needs.recipient_auth_pub_key */ }
        // CancelRefused carries { statechain_id, code, message, decision } — every variant of it
        // means the lock is STILL HELD, which is why it is a type and not a string.
        None => return Err(e),
    },
}
```

Recipient side — preview before signing, because consenting to a cancellation is consenting to give
a payment back:

```rust
let req = wallet.preview_cancel_consent(&statechain_id).await?;  // read-only, no lock
// req.statechain_id(), req.amount() (branch-validated), req.recipient_auth_pub_key(), req.is_coloured()
let token = wallet.cancel_consent(&req).await?;   // single-use, bound to the material previewed

// Or let the recipient enumerate its own mailbox instead of trusting a named request:
let all = wallet.preview_all_cancellable_consents().await?;
```

Sender finishes with the whole opaque token — do not split it:

```rust
wallet.cancel_transfer_with_consent(&statechain_id, &recipient_auth_pub_key, &token).await?;
```

Covered by `sdk85`.

### Recovering an interrupted split

An in-ladder split terminalizes the parent and *then* conveys the pieces serially, so a crash has a
nameable middle. `recover_in_ladder_splits` classifies each interrupted operation and never conveys
anything by itself:

```rust
for rec in wallet.recover_in_ladder_splits().await? {
    // rec.op_id, rec.lane, rec.terminalized_statechain_id, rec.outcome
    // InLadderSplitOutcome::Replayed { change_statechain_id, unconveyed_pieces }
    //   — the material survived; the pieces are real coins of this wallet that were never handed over
    // InLadderSplitOutcome::Retryable        — the crash landed before terminalization; just pay again
    // InLadderSplitOutcome::CooperativePathLost
    //   — the SE consumed the budget but the SP co-signature was never recorded; unilateral exit only
}

// PendingConveyance { op_id, lane, terminalized_statechain_id, outstanding, stranded }
let pending = wallet.pending_conveyances().await?;
let outcome = wallet.resume_split_conveyance(&op_id).await?;   // tesr::ConveyanceOutcome
wallet.convey_recovered_piece(&op_id, &piece_statechain_id, &recipient_address).await?;
```

Covered by `sdk81`. Structural RGB spends have their own healer,
`wallet.recover_structural_spends()`; both coloured send entry points — `transfer_tokens` and
`batch_transfer_tokens` — run it before selecting a carrier, so an app rarely calls it directly
(`sdk73`).

## Receive (auto-claim + events)

```rust
use mercury_utexo_sdk::WalletEvent;

let mut events = wallet.subscribe();
let handle = wallet.start_background();          // poll loop; abort() to stop
while let Ok(ev) = events.recv().await {
    match ev {
        WalletEvent::DepositConfirmed { address, amount_sats } => {}
        WalletEvent::TransferClaimed { statechain_ids } => {}
        WalletEvent::TransferCancelled { statechain_ids } => {}   // the payment will never arrive
        WalletEvent::TokenTransferClaimed { asset_id, amount, statechain_id } => {}
        WalletEvent::BalanceUpdate { balance } => {}
        WalletEvent::LadderEstablished { statechain_id } => {}
        WalletEvent::LadderSkipped { statechain_id, reason } => {}      // transition-only
        WalletEvent::LadderDefended { statechain_id, tiers_broadcast } => {}
        WalletEvent::ExitDeadlineApproaching { statechain_id, deadline_block, tip } => {}
        WalletEvent::LeafExitForced { statechain_id, deadline_block, tip } => {}
        WalletEvent::TokenCarrierMaterialized { statechain_id, deadline_block, tip } => {}
        WalletEvent::ExitBranchConflict { statechain_id } => {}         // someone raced your exit
        WalletEvent::CoinRefreshed { old_statechain_id, new_statechain_id, fee_sats } => {}
        WalletEvent::ColoredExitTipRegistered { statechain_id, outpoint } => {}
        WalletEvent::ColoredExitTipUnregistered { statechain_id, detail } => {}
        WalletEvent::WatchtowerBlind { pass, detail } => { /* 🔴 ALERT — see below */ }
    }
}
// One-shot instead of the background loop:
let r = wallet.claim().await?;
// r.claimed_transfers, r.confirmed_deposits, r.token_results, r.cancelled_transfers
```

`claim()` is what adopts an incoming state or child bundle: it verifies the bundle (the adopted state
must carry the strictly-lowest CSV), runs the census over the disclosed superseded states, and
completes the key handover. A cancelled incoming transfer is **reported**, not raised — it lands in
`ClaimResult::cancelled_transfers` and as `TransferCancelled`, so one withdrawn payment cannot
discard the deposits and receipts of the same pass. Each `TokenClaimStatus` carries a
`TokenClaimState`: `Booked`, `Pending` (transient RGB error, retried; the coin is quarantined from
plain-BTC spends) or `Rejected` (permanently invalid consignment — the coin is ordinary spendable
BTC).

⚠️ **Claim promptly, and keep the background loop running.** A conveyed-but-unclaimed coin is held
server-side by the coordinator's open-transfer window — a **wall clock**, not ownership. While it is
open, `/sign/first` on that coin answers `409 Conflict`; once the row ages past it, a payer who
bypasses their own client gets a session. That is the first move of a claw-back, not a completed one
(the payer must still win a broadcast race against the state you hold at a lower CSV), but the only
reliable answer is to claim. Read
[`../spec/TRUST-MODEL.md`](../spec/TRUST-MODEL.md) before designing a flow that parks an unclaimed
coin; a Lightning latch batch is the deliberate exception and stays open until its receiver claims.

**`WatchtowerBlind` is an alert, not a log line.** While it persists, nothing is racing a clawback or
a hostile trigger on this wallet's behalf. The same state is retained and readable on demand:

```rust
if wallet.is_watchtower_blind().await {
    for f in wallet.watchtower_faults().await {
        // f.pass (WatchtowerPass::AutoExit | DefendLadders), f.detail,
        // f.consecutive_failures, f.since_unix, f.last_unix
    }
}
```

## Utexo invoices

```rust
let inv = wallet.create_sats_invoice(15_000, Some("coffee".into()), None).await?;      // utexoinv1…
let inv = wallet.create_tokens_invoice(&asset_id, 250, None, None).await?;
let r   = payer.fulfill_utexo_invoice(&inv).await?;   // decodes, checks expiry, transfers
```

`decode_utexo_invoice` / `encode_utexo_invoice` are free functions on the crate root, over
`UtexoInvoice`. A decoder refuses a format version it does not support by name
(`SdkError::UnsupportedVersion`) rather than mis-parsing it. Covered by `sdk11`.

## Tokens

RGB assets ride on **carriers**. `SdkConfig::colored_ladder` ships **`false`**, and on that default:

- a carrier is never laddered — a plain tier spend carries no state transition and would destroy the
  allocation, so `claim()` records `LadderSkipReason::RgbCarrier` and leaves it on the flat
  signed-once backup shape (`sdk52`, `sdk39`);
- `transfer_tokens` / `batch_transfer_tokens` take the RGB-aware **flat** split lane
  (`create_colored_split_tx` / `create_colored_combine_tx`), not the coloured in-ladder split;
- a carrier has **no unilateral exit**. `unilateral_exit` refuses it; the protection is
  `materialise_carrier`, which settles the allocation on chain and is not an exit.

The SDK enforces the carrier boundary everywhere: a carrier is excluded from plain-BTC coin
selection, from Lightning swap selection, from the `withdraw` and `unilateral_exit` defaults (and
hard-errors if named explicitly), and from the cooperative `refresh` / `auto_refresh` route —
`withdraw` and `refresh` are RGB-unaware and refuse a carrier on **every** configuration, coloured
ladder or not. `unilateral_exit` is the one that opens, and only for a carrier whose ladder is
coloured.

```rust
let tokens = wallet.get_token_balances().await?;                 // Vec<TokenBalance>
let ledger = wallet.ledger_token_balances().await?;              // HashMap<asset_id, amount>
let allocs = wallet.list_token_allocations(&asset_id).await?;    // Vec<(outpoint, amount)>
let txs    = wallet.query_token_transactions(&asset_id).await?;  // Vec<TokenTx>
let l1     = wallet.get_token_l1_address().await?;               // == get_token_funding_address()

wallet.transfer_tokens(&asset_id, &bob_address, 250).await?;
// If no single carrier covers the amount, this combines several carriers of the asset into ONE
// SE-co-signed coloured tx (piece + change); every combined carrier is made terminal first.

// Multi-recipient: ONE SE-co-signed tx carves one piece per recipient plus this wallet's change;
// each piece ships its own consignment. One TransferResult per recipient, in order.
let results = wallet
    .batch_transfer_tokens(&asset_id, &[(bob_address.clone(), 250), (carol_address, 100)])
    .await?;

wallet.burn_tokens(&asset_id, 50).await?;
```

Any token call on a wallet without both `rgb_proxy_url` and `rgb_data_dir` fails with
`SdkError::TokensNotConfigured`; `get_token_balances` returns an empty vec instead of erroring, and
`ledger_token_balances` reads the wallet's own ledger and needs no RGB engine. Issuance and minting —
`issue_token`, `issue_token_sized`, `issue_inflatable_token`, `mint_tokens` and friends — are covered
in [`issuer-sdk.md`](issuer-sdk.md).

Turning `colored_ladder` on switches carriers onto the coloured ladder (`sdk74`, `sdk75`, `sdk77`)
and structurally closes the legacy split lane, with a narrow migration hatch for carriers that can
never be coloured (`sdk78`). What gates the flip is **measured economics, not safety** — read
[`../spec/PARTIAL-PAYMENT-ECONOMICS.md`](../spec/PARTIAL-PAYMENT-ECONOMICS.md) before flipping it,
and take the cost figures from there rather than from this guide. On that lane the carrier's first
partial payment splits the root and leaves a coloured spine tip; every later one is a coloured spine
batch over that tip, so the lane is repeatable rather than one-shot. The coloured helpers
(`colored_ladder_health`, `colored_exit_proof`, `renew_colored_ladder`, `probe_colored_tip`,
`colored_reanchor`, `transfer_colored_carrier`, `transfer_colored_child`) are only meaningful on that
configuration.

## Lightning

Lightning works **both directions** through an SSP (a statechain wallet plus an RLN node), latched to
a HODL invoice. `ssp` is `&impl Ssp` — the same call works against an in-process `SspService` or a
remote `SspClient` over HTTP (`sdk21`).

```rust
// PAY (Utexo -> Lightning). No payment -> the latch expires and the coin is still yours.
let preimage = wallet.pay_lightning_invoice(&ssp, bolt11).await?;   // proof of payment

// RECEIVE (Lightning -> Utexo). Hand swap.invoice to the payer; the coin arrives via claim().
let swap = wallet.create_lightning_invoice(&ssp, 25_000).await?;
// swap.batch_id, swap.invoice, swap.payment_hash, swap.statechain_id, swap.asset_id, swap.asset_amount
```

`pay_lightning_invoice` mints an exact coin when it can; when the wallet holds only laddered coins of
the wrong size it auto-routes to the non-exact in-ladder lane — the piece is split off in-ladder,
latched to the invoice hash, and censused by the SSP before it pays (`sdk63` pay, `sdk64` receive,
`sdk65` non-exact pay, `sdk67` non-exact receive). To pick the parent yourself:

```rust
let preimage = wallet.pay_lightning_invoice_inladder(&ssp, bolt11, &parent_sid).await?;
```

The LN-latched piece is the **one** coin that stays terminalized (`set_spend_budget(piece, 0)` before
conveyance): it is deliberately left unclaimed past the pending-transfer lock's window, so it is
frozen rather than left re-transferable. The parent and the change slice are untouched.

RGB over Lightning uses `create_lightning_invoice_asset` (which takes a concrete `&SspService`). The
coloured latch legs themselves are wallet methods, `latch_tokens` (a known payment hash) and
`latch_tokens_se_preimage` (the SE mints the preimage); `SspService` drives them (`sdk23`).

### Failure handling

```rust
match wallet.pay_lightning_invoice_reclaimable(&ssp, bolt11).await {
    Ok(preimage) => { /* paid */ }
    Err((coin_id, e)) if !coin_id.is_empty() => {
        // ONLY after positively confirming the SSP did NOT pay, and after the SE batch_timeout:
        wallet.reclaim_lightning_payment(&coin_id).await?;
    }
    Err((_, e)) => return Err(e),   // failed before anything was latched
}
```

A client-side timeout, or an error *after* the SSP revealed the preimage, are **not** proof of
non-payment. Two shape-specific outcomes:

- **Non-exact (in-ladder) pay failure** rolls the un-broadcast split back: the parent is restored as
  exitable and the conveyed piece + optimistic change bookings are dropped, so the whole parent value
  is recovered (`sdk66`, `sdk68`).
- **Exact laddered pay failure** leaves an orphan co-signed `S'`, so the coin is restored locally as
  exitable (value intact via `unilateral_exit`) but **off-chain re-transfer stays bricked until you
  `refresh()`** it. Un-laddered coins reclaim cleanly off-chain via a self-transfer.

Adversarial coverage: `sdk19`, `sdk20`, `sdk24`, `sdk25`, `sdk37`.

### Low-level latch legs

If you are building the counterparty side (an LSP) rather than using an SSP:

```rust
let swap = wallet.start_lightning_swap(&lsp_address, None).await?;
// LSP verifies get_swap_payment_hash(&swap.batch_id), then pays the invoice for swap.payment_hash
let preimage = wallet.settle_lightning_swap(&swap).await?;   // unlocks the coin for the LSP
```

## Exit

```rust
// Cooperative (immediate, 1 tx per coin, SE co-signed, no timelock wait):
let txids = wallet.withdraw("bc1p…", None /* all coins */, None /* fee rate */).await?;
let q = wallet.get_withdrawal_fee_quote(None).await?;   // n_coins, est_vbytes, fee_rate_sat_vb, fee_sats

// Unilateral (no SE cooperation needed):
let statuses = wallet.unilateral_exit(None, None).await?;   // Vec<ExitStatus>
```

`unilateral_exit` is **incremental and idempotent**, because an exit is a walk down the pre-signed
chain. Each call advances as far as maturity allows and returns
`ExitStatus { statechain_id, complete, wait_blocks }`; call it once per block until `complete`
(`sdk50`).

- **Laddered root** — broadcast `T` (spending `F`), then each `X_m`/`S_k` as its relative CSV
  matures.
- **Received child** — the same walk over `T → X_m → SP → ext_child → state_child`, keyless (every
  tier is already signed and the final state pays your own key).
- **Un-laddered coin** — broadcast the locktime-free exit branch, then the latest backup once its
  absolute `nLockTime` matures.

Guards: a coin that is not `CONFIRMED` is refused (exiting a parent already consumed by a split would
invalidate the sub-coins it funded), and an RGB carrier is refused unless its ladder is **coloured**
— an RGB-unaware sweep destroys the allocation. The un-colourable class gets its own refusal naming
`materialise_carrier`, not a silently different outcome.

```rust
// Sever from F on demand. Every prior owner of this coin retains rungs of its flat backup chain
// over the SAME funding output; broadcasting the already-co-signed, un-timelocked T spends F and
// kills all of them at once. It needs no counterparty — which is the point, because the party being
// defended against is the party a cooperative re-anchor would have to ask. It ENDS the exposure
// rather than capping it, and it costs the coin's off-chain life. Mechanically unilateral_exit on
// one coin, named for what it is for; see ../spec/TRUST-MODEL.md.
let statuses = wallet.sever_from_f(&statechain_id).await?;

// Settle an un-colourable carrier's ALLOCATION on chain. NOT an exit: the sats stay on the live
// 2-of-2. Returns true if a branch was broadcast, false if the carrier verifiably had none.
let acted = wallet.materialise_carrier(&statechain_id).await?;

// Answer a griefer's confirmed trigger: spend at zero CSV wait, killing every retained tier.
let txid = wallet.detrigger_to_owner(&statechain_id, None).await?;   // sdk89
```

Cost and readiness:

```rust
let est = wallet.estimate_exit_cost(&statechain_id).await?;
// est.branch_txs, est.branch_vbytes, est.backup_vbytes, est.total_vbytes
// est.fee_sats_at(rate)
// est.wait_blocks          — when the exit COMPLETES
// est.exit_deadline_block  — the SAFETY deadline for an off-chain sub-coin: the earliest height an
//                            ancestor could broadcast a stale backup. `None` when the coin carries
//                            no exit branch at all — a coin funded on chain, a laddered root
//                            included — which is NOT a claim that it has no calendar (the retained
//                            flat chain does).
// est.exit_deadline_blind  — Some(reason) means "I could not tell", never "nothing is due"
```

⚠️ **`exit_deadline_block == None` alone is not "safe".** Branch on
`est.deadline_is_unknown()`: a `None` produced by an unreachable SE or an unreadable chain lookup is
indistinguishable from the `None` a genuinely deadline-free coin produces, and reading the first as
the second is how a watchtower concludes "nothing is due" from a total inability to tell.

The whole-coin walk is `3 + 2d` sequential transactions at split depth `d`
(`config::tesr_exit_txs`), `293·d + 375` vB (`config::tesr_exit_vbytes`), and
`config::tesr_exit_wait_blocks(params, d)` blocks of relative timelocks plus one confirmation per
tier. A spine-shaped level costs one rung per level instead of two — `4 + d`, via
`tesr_exit_txs_for(ExitShape::Spine, d)`. Use the shape-aware form when publishing a cost, and the
conservative bare `tesr_exit_txs` for any safety margin: over-counting makes a tower act early,
which is the safe direction.

## Maintenance and watchtowers

`start_background()` runs four things per poll: `claim()`, then every pass in
`maintenance_plan(&config)`, then the ladder defence (once per new block), then `auto_exit_due` when
`auto_exit` is on.

```rust
// 1. DEADLINE SAFETY — unconditional. maintenance_plan returns DeadlineSafety for every config.
//    Route 1 is the cooperative re-anchor (auto_refresh_due); whatever the counterparty declines to
//    co-sign is then SEVERED from F by broadcasting the already-co-signed trigger. A defence its
//    adversary can decline is not a defence.
let (re_anchored, severed) = wallet.deadline_safety_due(cfg.auto_refresh_margin_blocks).await?;
let refreshed = wallet.auto_refresh_due(cfg.auto_refresh_margin_blocks).await?;   // route 1 alone

// 2. LADDER DEFENCE — unconditional, idempotent, once per block. No-op while F is unspent (an idle
//    laddered coin never ages on the CSV side). If someone DID trigger the coin, this races your
//    tiers; your adopted state carries the strictly-lowest CSV so it matures first and the funds
//    land at your own key. Emits LadderDefended.
let acted = wallet.defend_ladders().await?;

// 3. DEADLINE TOWER — force-exit plain off-chain sub-coins and MATERIALIZE received RGB carriers
//    within margin of their exit-race deadline. Emits ExitDeadlineApproaching / LeafExitForced /
//    TokenCarrierMaterialized.
let acted = wallet.auto_exit_due(cfg.auto_exit_margin_blocks).await?;
```

All three **fail closed and loud**. An empty success result means "nothing was due", never "I could
not tell": a chain-tip, wallet-record or RGB-carrier enumeration failure emits `WatchtowerBlind`,
retains a `WatchtowerFault`, and returns `Err`. `defend_ladders` additionally reports a coin whose
`F` was spent by a transaction that is **not** its own trigger as a permanent LOSS, kept distinct
from blindness so a later success on some other coin cannot erase it — no pass recovers that coin
(`sdk72`).

Note the interaction with `colored_ladder`: `deadline_safety_due`'s unilateral fallback runs through
`unilateral_exit`, which refuses a carrier without a coloured ladder — on the default configuration
that is every carrier. Those coins are reported three ways (event, stdout line, `Err`) rather than
folded into a clean pass. The flat carrier lane has no remedy here; the SDK just stops claiming one.

### Delegated watching, without custody

Everything a tower broadcasts is already fully signed and pays only the owner:

```rust
use mercury_utexo_sdk::{watch_pass, WatchBundle, WatchState};

let bundle_json = wallet.export_watch_bundle().await?;   // NO mnemonic, NO key shares, NO RGB seed
let bundle: WatchBundle = serde_json::from_str(&bundle_json)?;
match watch_pass(&bundle, &electrum_client, margin_blocks) {
    WatchState::Idle => {}                                  // the tip WAS read and nothing is due
    WatchState::Acted { ids, failures, blind } => {}        // engaged; `blind` is per-entry
    WatchState::Blind { reason } => { /* 🔴 alert */ }      // saw nothing, defended nothing
    WatchState::Void { spender, detail } => { /* 🔴 the coin is gone — stop retrying */ }
}
```

Run it on a cron anywhere, from any number of machines — no wallet database, no SE, no keys.
Broadcasts are idempotent and every tower broadcasts the SAME transactions, so a second tower can
never conflict with the first. Carriers are exported **without** their backup transaction, so a tower
structurally cannot do the token-destroying sweep (`sdk45`, `sdk51`, `sdk34`, `sdk79`, `sdk80`).
`export_watch_bundle` fails closed for a token wallet whose carriers cannot be enumerated, and
refuses to export an entry whose deadline is blind.

Never use emptiness to decide whether a pass worked — use `WatchState::is_blind()`. Re-export the
watch bundle after any transfer/claim/split, like the recovery bundle.

If a tower needs to fee-bump a tier stuck under the relay floor it needs `SdkConfig::fee_bump`; a
keyless tower has no move at all. `wallet.fee_float_solvency()` reports whether the configured float
still covers the reserve.

## Refresh (re-anchor)

`refresh` moves a coin to a **fresh funding outpoint** with ONE SE-co-signed on-chain transaction:
the current outpoint is spent into a fresh deposit aggregate (new `statechain_id`, same owner) and
`claim()` mints a brand-new ladder on it. Because the old outpoint is now spent, every previous
owner's pre-signed material against it is permanently dead.

It does **not** reset the exit — a laddered coin's exit is the CSV tier chain, which never matures
while idle. It **does** reset the coin's flat calendar, by minting a fresh chain at
`tip + initlock`, which is what makes it the answer for a coin that has spent most of its hop budget.
Reach for it to:

- reset a coin approaching its flat-chain deadline (this is what `deadline_safety_due` does for you);
- **un-brick** a coin whose exact-lane Lightning pay failed (its orphan `S'` blocks off-chain
  re-transfer until re-anchored);
- reset an un-laddered coin whose decrementing-`nLockTime` backup chain is nearing its floor (a
  receiver rejects a backup at or below the tip — `MercuryError::LocktimeTooLow`);
- consolidate an off-chain coin back onto a confirmed outpoint.

Refresh is **cooperative** (it needs the SE); if the SE is gone, exit unilaterally instead. The coin
must be `CONFIRMED`, carry no RGB allocation, and be large enough to cover the fee above the dust
floor (`sdk30`).

```rust
// User pays: the on-chain fee comes from the coin, so the refreshed coin is amount − fee.
// fee_rate (sat/vB) is capped at the client's max_fee_rate; None uses the SE-quoted rate.
let r = wallet.refresh(&statechain_id, None).await?;
// r.old_statechain_id (now spent), r.new_statechain_id, r.old_amount_sats,
// r.new_amount_sats == old − fee, r.fee_sats, r.refresh_txid, r.rebate_sats

// Operator pays: the same on-chain re-anchor, then a funded `sponsor` wallet reimburses the fee
// OFF-CHAIN so the user's total ends >= whole. r.rebate_sats is max(fee + dust, min_child_value) —
// the rebate must itself be an off-chain-payable piece, which on a laddered sponsor means clearing
// the 1 560-sat in-ladder piece floor at the shipped 3.0 sat/vB rate.
let r = wallet.refresh_sponsored(&statechain_id, &sponsor, None).await?;

// The sponsor's own half, if you are building the operator side:
let t = sponsor.rebate_refresh_fee(&user_utexo_address, fee_sats).await?;
```

The fresh coin confirms asynchronously (watcher / `claim()`), like a deposit. `sdk38` measures a
broke sponsor: the loss is bounded.

## History

```rust
let activities = wallet.get_activities().await?;   // deposits, sends, receives
let transfers  = wallet.get_transfers().await?;
let one        = wallet.get_transfer(&utxo).await?;   // Option<Activity>
```

## Errors worth handling

`SdkError` (`clients/libs/rust-sdk/src/types.rs`); everything else arrives as `anyhow::Error`.

| Variant | Meaning | Action |
|---|---|---|
| `TokenPaymentRequired { token_id, deposit_address, fee_sats }` | the SE charges for onboarding slots | pay `fee_sats` to `deposit_address`, retry |
| `InsufficientBalance { requested_sats, available_sats }` | amount > spendable | top up / lower the amount |
| `NoExactAmount { requested_sats }` | no exact subset and split was disabled for the call | allow the split, or pick a different amount |
| `TokensNotConfigured` | a token call without RGB config | set both `rgb_proxy_url` and `rgb_data_dir` |
| `CoinBelowMaintenanceCost { statechain_id, amount_sats, fee_sats }` | the coin is worth less than its own re-anchor fee | not lost — combine it with another coin so the aggregate covers the fee |
| `BatchTooManyRecipients { recipients, slots, cap, max_recipients }` | `K + 1 > 64` derived slots | split into batches of at most 63 |
| `UnsupportedVersion { kind, found, supported }` | a self-describing payload declares a format version this build cannot interpret | upgrade; never mis-parse it |

Untyped refusals worth recognising:

- the per-leg in-ladder floor message (*"the piece falls short"* / *"the change falls short"*) — the
  leg named was too small to fund the rungs its lane's builder gives it;
- *"coin … is a SPINE TIP"* — a whole-coin handover of a change tip, which has no conveyance builder;
- `LocktimeTooLow` on an un-laddered handover — that coin needs a `refresh` first;
- *"no exit material on any lane"* — the slot has neither a TES-R bundle nor flat backup rows;
  combining does not rescue it.

`CancelRefused` and `CancelNeedsRecipientConsent` are re-exported from the crate root so a
cancellation refusal can be branched on rather than string-matched.

## Design, not built

Two items appear in the specification and have no running enforcement. Do not build product on
either:

- **The discharge round** ([`../spec/SPEC.md`](../spec/SPEC.md) §5.4) is a design with nothing
  plant-and-run. Its SE enforcement point is empty, so the SE would presently co-sign a collapse that
  pays out nobody.
- **De-trigger restoration.** `detrigger_to_owner` is wired and proven (`sdk89`), but there is no
  fresh `F′` and no rebuilt `T′/X′_0/S′_0` — returning off-chain after a de-trigger is a fresh
  deposit.

## Testing

Live end-to-end cases run from `clients/tests/rust` under `SDK_E2E=<n>`; the numbers reach 91 and are
not contiguous. `SDK_E2E=22` is `chaos22`, the concurrent fuzzer (N users acting in parallel against
a spec-invariant oracle). See [`testing-guide.md`](testing-guide.md) for the run environment and
per-suite invocation.
