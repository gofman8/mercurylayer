# API reference — `mercury-utexo-sdk`

Crate: `mercury-utexo-sdk` (`clients/libs/rust-sdk`). Every method below is `async` on `UtexoWallet`
unless marked **sync**; signatures elide `&self` and the `anyhow::Result<…>` wrapper. The two
exceptions — `pay_lightning_invoice_reclaimable`, whose error half carries a coin id, and
`resume_split_conveyance`, whose success half is an outcome report — are spelled out in full.

Normative behaviour lives in [`../spec/`](../spec/README.md): [SPEC.md](../spec/SPEC.md) for the
requirement set, [PROTOCOL.md](../spec/PROTOCOL.md) for the tiers and the in-ladder split,
[CHILDREN.md](../spec/CHILDREN.md) for received children, [LIGHTNING.md](../spec/LIGHTNING.md) for
the HODL-invoice latch, [TRUST-MODEL.md](../spec/TRUST-MODEL.md) for what each party can do to you,
and [PARTIAL-PAYMENT-ECONOMICS.md](../spec/PARTIAL-PAYMENT-ECONOMICS.md) for the measured cost of a
payment. Where this document and the spec disagree, the spec is right.

## Coin shapes — read this first

There is **one protocol**. `claim()` ladders every fresh confirmed **root** coin with TES-R, always;
there is no `deposit_protocol_version` field and no `UTEXO_PROTOCOL_DEFAULT` environment variable.
Coin shape follows from what the coin *is*, and a handful of methods behave differently on each
shape. Three of the four are LADDERED shapes and a wallet can hold all of them at once; the fourth
is the absence of a ladder, and it is a state to repair rather than a lane to pick.

| | **LADDERED ROOT** | **RECEIVED CHILD** | **SPINE TIP** | **NO LADDER** |
|---|---|---|---|---|
| Which coins | every plain BTC deposit, laddered at `claim()` (`tesr-` row) — and, wherever `colored_ladder` is on, every RGB **carrier** too, laddered *coloured* | a piece adopted from someone's in-ladder split (`ctesr-` row) | this wallet's own change leg after an in-ladder payment (`spinetip-` row) | a coin `claim()` could not ladder, with a `LadderSkipReason` recorded for it; and off-chain sub-coins carved by the legacy coloured split/combine lane, whose funding output is un-broadcast (`branch-` rows) |
| Exit material | `F → T` (TRIGGER, no timelock) `→ X_m` (EXTENSION, relative CSV) `→ S_k` (STATE, relative CSV) — v3/TRUC with a P2A anchor, pre-signed and **un-broadcast** | the root walk plus every intermediate segment, then `ext_child → state_child` | the root walk plus every intermediate segment, then ONE cap over `SP_i.out[K]` | one signed-once backup tx with an absolute `nLockTime`, plus — for a sub-coin, whose funding is un-broadcast — the exit branch down from the on-chain root |
| Ageing | **none.** BIP-68 relative locks start counting only once the parent confirms and `T` carries no timelock, so nothing matures until someone broadcasts `T`. An idle coin never ages: 0 vB of idle rent. Its one absolute clock is `min(L_k)` over the flat backup chain, held by prior owners | inherits the parent's `min(L_k)`; the walk must be **started** `head_start` blocks before it | same as a child | absolute locktime plus a deposit-anchored exit-race deadline (`ExitCostEstimate::exit_deadline_block`) |
| Transfer whole | co-sign a fresh state one δ **lower**, disclosing the replaced one for the receiver's census | `tesr::child_retransfer` — same rule, one level down | **refused** by `transfer`: there is no spine-tip conveyance builder, and a flat conveyance would hand the receiver an outpoint that will never exist on chain | key handover of the backup chain, and only where `transfer_sender::assert_flat_conveyance_is_legitimate` can PROVE the coin is legitimately flat — a terminalized carrier, a pre-migration-0009 no-aggregate coin, or a wallet that has provably never been through the ladder pass. Every other coin is refused |
| Non-exact payment | `in_ladder_pay` / `in_ladder_pay_many` — a state tier `SP` over `X_m.out[0]`, a descendant of `T`, never a rival for `F` | `child_in_ladder_pay` / `child_in_ladder_pay_many` | `spine_batch_pay` / `spine_batch_pay_many` — `SP_{i+1}` over `SP_i.out[K]` | **none.** `parent_shape` refuses instead of returning a shape: the plain off-chain split that served this row spent the coin's funding output `F` directly, which is what a prior owner's retained un-timelocked trigger also spends [B1], and it is DELETED |
| Maintenance | off-chain renewal and rollover, unbounded (`sdk43`); `refresh` is the on-chain **re-anchor**, not a deadline reset (`sdk30`) | none needed; protected by `auto_exit_due` | as child | `auto_exit_due` / a keyless watch bundle |
| Unilateral exit | `unilateral_exit` walks the chain tier by tier; idempotent, call once per block until `complete` (`sdk50`) | walks `T → X_m → SP → ext_child → state_child` (`sdk59`, `sdk60`) | walks the prefix, then its cap | broadcast the exit branch, then the backup once its `nLockTime` matures |

**The fourth column shrank by construction, and that is the point.** `split_coin`, the plain
off-chain split, and `ParentShape::Unladdered` with it, are gone; `ensure_exact_coin` no longer
mints. So nothing in this SDK *produces* a plain un-laddered sub-coin any more, and the [B1] hazard
that shape carried — a prior owner's retained, un-timelocked trigger spends the same `F` the split
spends, voiding it, with no way for the receiver to detect the exposure — is closed by construction
rather than by a refusal inside one function. What did **not** change is the fact underneath it: a
split sub-coin's funding output is un-broadcast and cannot root a trigger. Every in-ladder split
child and every spine-tip change leg still has un-broadcast funding — that is what buys the 0 vB of
idle rent — and the `branch-` rows that are a sub-coin's only way down are still read on every exit.

`SdkConfig::colored_ladder` **reads the pinned attestation identity rather than stating a bool**
(`SdkConfig::regtest`, `SdkConfig::mainnet` in `clients/libs/rust-sdk/src/config.rs`, against
`mercurylib::tesr::TesrParams::attestation_identity_const`). It is therefore **on** for regtest,
which has a compiled-in pin, and **off** for mainnet — not as a product judgement, but because no
mainnet enclave is provisioned, so there is no identity to pin and `claim()` could not establish a
ladder of any colour there. Turning it on without a pin would ship a wallet whose token lane refuses
permanently behind a message promising that a later `claim()` will fix it; pin a mainnet identity in
`attestation_identity_const` and this flips itself, with no other change. Where it is on, a carrier
is laddered like any other coin and every coloured exit path applies to it. Every call under
[Coloured-lane diagnostics](#coloured-lane-diagnostics) **except `probe_carrier_funding`** needs a
coloured ladder to exist; `probe_carrier_funding` is the sibling written for a carrier that has none,
which is what a carrier is on a network still waiting for its enclave.

## `UtexoWallet`

### Lifecycle & identity

| Method | Signature | Notes |
|---|---|---|
| `initialize` | `(SdkConfig, Option<&str> mnemonic) -> (UtexoWallet, String)` | create or restore; a differing mnemonic for an existing wallet name is an error. The returned mnemonic restores keys only — see `export_recovery_bundle` |
| `subscribe` | **sync** `() -> broadcast::Receiver<WalletEvent>` | multi-consumer event stream |
| `start_background` | **sync** `() -> JoinHandle<()>` | every `poll_interval_secs`: `claim()`, then the `maintenance_plan` passes, then `defend_ladders` (gated to one pass per new block), then `auto_exit_due` when `auto_exit`. `abort()` to stop |
| `get_identity_public_key` | `() -> String` | 33-byte compressed hex, derived at `m/1000h/0h/0h` |
| `get_utexo_address` | `() -> String` | stable bech32m statechain address (`ml1…`/`tml1…`); reuse is supported |
| `export_recovery_bundle` | `() -> String` | the ONLY complete backup: wallet record + every backup row (ladders, child bundles, `branch-*`, `parents-*`) + the RGB engine seed. Plain JSON containing the wallet seed — store it securely, and for token wallets also copy `rgb_data_dir` |
| `import_recovery_bundle` | `(SdkConfig, bundle_json: &str) -> (UtexoWallet, String)` | restore from an exported bundle |
| `sign_message_with_identity_key` | `(message: &[u8]) -> String` | BIP-340 Schnorr over `sha256(message)`, 64-byte hex |
| `validate_message_with_identity_key` | **sync, static** `(message: &[u8], signature_hex: &str, public_key_hex: &str) -> bool` | verifier for the above |
| `client_config` / `wallet_name` | **sync** `() -> &ClientConfig` / `() -> &str` | accessors used by integrations that drive `mercuryrustlib` directly |

### Balance & history

| Method | Signature | Notes |
|---|---|---|
| `get_balance` | `() -> Balance` | `{available_sats, pending_sats, in_transfer_sats, tokens}`. Fails **closed** on both halves: an unreadable carrier set or unreadable token balances is an `Err`, never a quiet zero |
| `get_token_balances` | `() -> Vec<TokenBalance>` | empty when RGB is not configured. `balance`/`total` take the `max` of the engine's chain-anchored figure and this wallet's own ledger, because every allocation here is deliberately un-broadcast |
| `ledger_token_balances` | `() -> HashMap<String, u64>` | the off-chain half alone: root carriers (`tesr-`), adopted children (`ctesr-`) and spine tips (`spinetip-`), each from the consignment the receiver validated at claim |
| `list_token_allocations` | `(asset_id: &str) -> Vec<(String, u64)>` | per-outpoint bindings — answers "is the allocation still on the coin I think it is?", which an aggregate balance cannot |
| `get_activities` | `() -> Vec<Activity>` | deposits / sends / receives |
| `get_transfers` | `() -> Vec<Activity>` | sends / receives only |
| `get_transfer` | `(utxo: &str) -> Option<Activity>` | single activity by `txid:vout` or `txid` |
| `list_coins` | `() -> Vec<CoinInfo>` | inventory with `status` and `off_chain` (true when the coin has a stored exit branch, i.e. its funding is un-broadcast) |
| `ladder_skip_reason` | `(statechain_id: &str) -> Option<LadderSkipReason>` | why this coin was left flat-only, read back from the persisted record. `WalletEvent::LadderSkipped` fires only on a transition, so this is the authority for an app that started later |
| `ladder_skip_reason_raw` | `(statechain_id: &str) -> Option<String>` | the exact persisted spelling — preferred when a forward value written by a newer client must not be silently dropped |
| `flat_only_coins` | `() -> Vec<(String, String, bool)>` | `(statechain_id, raw_reason, may_still_be_transferred)` for every flat-only coin. `false` in the third slot means `transfer` will refuse the coin; its value is unaffected either way (still withdrawable and exitable) |

### Deposit

| Method | Signature | Notes |
|---|---|---|
| `get_deposit_address` | `(amount_sats: u64) -> String` | fresh single-use address; fund with the exact amount |
| `add_prepaid_token` | `(token_id: &str)` | pool a pre-paid SE deposit token. Only fresh ON-CHAIN onboarding slots draw on this pool; split/combine/re-anchor slots use free SE-minted derived tokens |
| `claim` | `() -> ClaimResult` | one watcher pass: confirm deposits, claim incoming transfers, book incoming consignments, and **establish the TES-R ladder** on every fresh confirmed root coin (emits `LadderEstablished`) |

`claim()`'s laddering step is unconditional but records a `LadderSkipReason` and emits
`LadderSkipped` where it declines — an RGB carrier, a coin whose funding `F` is not on chain, a
coordinator that cannot be reached, an unpinned attestation identity. It fails **closed**: it never
ladders on a guess, and the next `claim()` retries the transient reasons. A cancelled incoming
transfer is *reported* (`ClaimResult::cancelled_transfers` + `WalletEvent::TransferCancelled`),
never raised, so one withdrawn payment cannot discard the deposits and receipts of the same pass.

### Send

| Method | Signature | Notes |
|---|---|---|
| `transfer` | `(receiver_address: &str, amount_sats: u64) -> TransferResult` | the one call you normally need; picks the route (below). `used_split` reports whether a split was needed |
| `quote_transfer` | `(amount_sats: u64) -> TransferQuote` | all-in preview over the SAME coin set and planner `transfer` uses, so `fundable: true` followed by a refusal is not expressible |
| `transfer_many` | `(recipients: &[(String, u64)]) -> Vec<TransferResult>` | ONE off-chain split of ONE parent → N pieces + change, routed on that parent's shape exactly like `transfer` — every `ManyRoute` is in-ladder and each one returns, so there is no plain-split tail for a parent to fall into (`sdk69`). Every piece AND the change must clear the route's floor. The carve is one transaction; the N hand-overs are not, so a partial failure leaves legs in `pending_conveyances` |
| `in_ladder_pay` | `(parent_statechain_id: &str, recipient_address: &str, piece_sats: u64, latch: InLadderLatch) -> (piece_sid, change_sid, Option<(batch_id, payment_hash)>)` | explicit in-ladder split of a **laddered root**: `SP` spends `X_m.out[0]` and pays a piece child (conveyed with the standard key handover) plus a change leg kept by this wallet. `latch` is `InLadderLatch::None` for a plain payment (`sdk59`) |
| `in_ladder_pay_many` | `(parent_statechain_id: &str, recipients: &[(String, u64)]) -> (Vec<String>, String)` | N recipients under one `SP`; returns `(piece_sids in recipient order, change_sid)`. Value is conserved by the builder, so the change is derived, not stated |
| `child_in_ladder_pay` | `(child_statechain_id: &str, recipient_address: &str, piece_sats: u64) -> (piece_sid, change_sid)` | the same one level down: split a **received child** into two grandchildren |
| `child_in_ladder_pay_many` | `(child_statechain_id: &str, recipients: &[(String, u64)]) -> (Vec<String>, String)` | one `CSP` over `ext_child.out[0]` carving N grandchildren plus change; this is also the lane `transfer` takes for a plain payment out of a received child (`sdk80`) |
| `spine_batch_pay` | `(tip_statechain_id: &str, recipient_address: &str, piece_sats: u64) -> (piece_sid, next_tip_sid)` | payment *N+1* out of a coin: `SP_{i+1}` spends the tip's own funding outpoint `SP_i.out[K]`. Deliberately has no `latch` parameter |
| `spine_batch_pay_many` | `(tip_statechain_id: &str, recipients: &[(String, u64)]) -> (Vec<String>, String)` | the N-recipient spine batch; the change becomes the next tip |
| `ensure_exact_coin` | `(sats: u64) -> String` | **finds** a CONFIRMED non-carrier coin of exactly `sats`, and errors when the wallet holds none. It no longer mints one: the off-chain plain split it used to mint with is DELETED, because it spent the coin's funding output `F` directly and a prior owner's retained no-timelock trigger spends the same `F` [B1]. Not a regression — REQ-42 requires the one-call Lightning pay to fall back to the NON-EXACT in-ladder lane exactly here, and that lane carves its piece as a DESCENDANT of the trigger rather than a rival for `F` |
| `transfer_tokens` | `(asset_id: &str, receiver_address: &str, token_amount: u64) -> TransferResult` | coloured split, or a multi-carrier combine when one carrier is insufficient (`sdk31`); consignment travels in the mailbox message |
| `batch_transfer_tokens` | `(asset_id: &str, transfers: &[(String, u64)]) -> Vec<TransferResult>` | one coloured tx to many recipients, each piece with its own consignment envelope |

**How `transfer` routes.** It runs the pre-spend auto-refresh hook (when `auto_refresh` is on),
then plans over confirmed, non-carrier coins that hold exit material on some lane:

- exact subset of whole coins → plain key handover per coin;
- a **received child** sent whole → `tesr::child_retransfer`, co-signing a fresh lower-CSV state
  over `ext_child.out[0]` and disclosing the replaced state (`sdk60`);
- a **spine tip** sent whole → refused by name (`tesr::load_spine_tip`, checked in the hand-over
  loop), because conveying it on the flat lane would hand the recipient a backup chain over an
  outpoint that is not on chain and never will be. The refusal costs the coin nothing: it stays
  unilaterally exitable and its cap already pays this wallet's own key;
- non-exact out of a **laddered root** → `in_ladder_pay`;
- non-exact out of a **received child** → `child_in_ladder_pay`;
- non-exact out of a **spine tip** → `spine_batch_pay`;
- non-exact out of a coin with **no ladder of any kind** → refused by name, naming `claim()` and
  `ladder_skip_reason` as the remedy. Every remaining route is in-ladder, so `ManyRoute` has no
  plain-split tail and the dispatch is exhaustive on the shape.

The planner prefers this wallet's own **inventory** — a laddered root or a spine tip — over a
received child, hard rather than as a tiebreak (`select::Candidate::is_inventory`, sorted ahead of
amount): splitting a received piece would push that payee's leaf a level deeper and mint a crumb that
sorts earlier next time. The answer comes from the coin's own `tesr-`/`ctesr-`/`spinetip-` record,
never from its amount, so a counterparty cannot choose which of the recipient's coins gets split next
by choosing what to send them.

**Admission floor for split payments.** A child funds its own two tiers (extension + state) and must
still clear dust, so a piece must be at least `mercurylib::tesr::min_child_value` =
`2·(committed_fee + P2A) + dust` = **1 560 sat** at the shipped 3.0 sat/vB committed rate. A change
leg on the spine and coloured lanes is one rung, `min_spine_tip_value` = **945 sat**. Both take the
fee rate as an argument — the figures above are evaluations at the shipped rate, not constants. The
plain backup-fee floor also applies and the larger binds. The floor is resolved **per leg**, not once
per split: `transfer`, `transfer_many` and `quote_transfer` all read it from the same internal
`split_output_floors` helper in `clients/libs/rust-sdk/src/transfer.rs`, so a quote cannot admit an
amount the executor refuses. The guard runs **before** the parent is terminalized, so a rejected
payment leaves the parent fully spendable (`sdk58`).

**Batch size.** A K-recipient batch needs `K + 1` fresh statechain slots, each costing one derived
token vouched by the coin being split, and a coin may only ever vouch
`DERIVED_SLOTS_PER_STATECHAIN` = 64 of them over its lifetime. So `K ≤ 63`
(`MAX_BATCH_RECIPIENTS`), refused locally by name as `SdkError::BatchTooManyRecipients` before any
SE call. Because every spine level is a fresh statechain, that is a bound *per level*, not a budget
the wallet spends down.

**Received children are first-class.** The claim completes the standard SE key handover, so the
receiver co-owns `A_child` (invariant across the rotation, which is what keeps the pre-signed exit
chain valid) and the sender is permanently locked out. A child can be paid onward off-chain — whole
or split — one co-signature and one disclosed superseded state per hop, counted by the receiver's
census (`sdk60`: alice → bob → carol with the funding outpoint unspent throughout).

**The window on the sender's side.** A payer who bypasses the client and POSTs `/sign/first`
directly gets HTTP 409 while the coordinator's one-hour transfer window is open, and HTTP 200 with a
`server_pubnonce` once the row is older than an hour (`sdk91`). That window is the only SERVER-side
gate on that path; an honest client is stopped earlier by two independent LOCAL gates (`sdk90`).

### Interrupted payments

The in-ladder lanes write a journal record before the parent is terminalized, so co-signed material
the SE will never re-issue survives a crash. None of these calls re-sends a payment by itself — a
crashed process is not evidence that the user still wants it made.

| Method | Signature | Notes |
|---|---|---|
| `recover_in_ladder_splits` | `() -> Vec<InLadderSplitRecovery>` | run at startup: replays every split that stopped after the parent was terminalized. Per record the outcome is `Replayed { change_statechain_id, unconveyed_pieces }`, `Retryable` (nothing was consumed — just pay again), or `CooperativePathLost` (the budget was consumed but the `SP` co-signature never recorded; the parent's value is recoverable only by unilateral exit). Idempotent |
| `pending_conveyances` | `() -> Vec<PendingConveyance>` | every piece this wallet carved and never handed over, across ALL journal records including closed ones. `stranded` legs are listed separately because they cannot be resumed to the same address |
| `resume_split_conveyance` | `(op_id: &str) -> Result<mercuryrustlib::tesr::ConveyanceOutcome>` | finish a batch from the journal alone — every recipient address was written before the parent was terminalized, so this takes no other arguments. Returns the outcome rather than erroring when legs remain |
| `convey_recovered_piece` | `(op_id: &str, piece_statechain_id: &str, recipient_address: &str) -> ()` | the explicit "yes, still send it" for one replayed piece. Rebuilds that leg's bundle from the journal; nothing new is signed |
| `recover_structural_spends` | `() -> Vec<StructuralSpendRecovery>` | the coloured-lane equivalent for interrupted colour split/combine spends. `transfer_tokens` and `batch_transfer_tokens` call it themselves before selecting a carrier, so an interrupted spend is always healed before a new one touches the same coin. Errors propagate; an unresolved entry stays open and its carriers stay excluded from selection |

A single-call payment that carves successfully but cannot hand every piece over **fails**, with the
`op_id` and the resume call in the message: the split is complete and durable, so the correct
response is to retry the hand-over, not to re-pay.

### Cancelling a payment

The coordinator's pending-transfer lock stops a sender from co-signing a rival state while a
recipient holds claimable material, so withdrawing a conveyed payment is not a power the sender
simply has. If the mailbox message was never posted, the sender alone may withdraw it; once posted,
the recorded recipient must co-sign. There is no force flag (`sdk85`).

| Method | Signature | Notes |
|---|---|---|
| `cancel_transfer` | `(statechain_id: &str) -> CancelOutcome` | sender side. Supplies the recipient co-signature automatically only when this wallet holds the recipient key. Otherwise it returns `CancelNeedsRecipientConsent`, which carries the recipient auth key |
| `preview_cancel_consent` | `(statechain_id: &str) -> CancelConsentRequest` | recipient side, read-only: the amount, coin and colour, all established locally by decrypting the mailbox message. Nothing in it is asserted by the party asking |
| `preview_all_cancellable_consents` | `() -> Vec<CancelConsentRequest>` | every transfer this wallet could consent to cancelling — the recipient enumerates its own mailbox instead of trusting a description |
| `cancel_consent` | `(approved: &CancelConsentRequest) -> String` | recipient side: a single-use consent token, bound to the conveyed material currently in this wallet's hands. Takes the previewed OBJECT, never a coin id plus a key, so it cannot be made to sign something that was never shown |
| `cancel_transfer_with_consent` | `(statechain_id: &str, recipient_auth_pub_key: &str, consent_token: &str) -> CancelOutcome` | sender side with the token obtained out of band. Pass the whole opaque string; a token stripped back is refused |

`CancelOutcome`, `CancelConsentRequest`, `CancelNeedsRecipientConsent`, `CancelRefused`,
`ConsentBlocked`, `ConsentToken` and `ConsentUnavailable` are re-exported from the crate root, so an
app can branch on a refusal instead of string-matching it.

### Maintenance and re-anchoring

`refresh` is the **re-anchor** primitive: ONE SE-co-signed on-chain tx moves the coin's value to a
fresh funding outpoint, minting a new statechain id and a new ladder. It is not a deadline reset — a
laddered coin has no calendar deadline to reset. Reach for it to put a stale-shaped coin back on a
fresh root, to invalidate every previous owner's backup by spending the old outpoint, or to unbrick
a coin after a failed latch. Renewal and rollover of a live ladder are off-chain and cost nothing
(`sdk43`); re-anchoring is the only step that touches L1 (`sdk30`). Refresh is **cooperative**; if
the SE is gone, exit unilaterally.

| Method | Signature | Notes |
|---|---|---|
| `refresh` | `(statechain_id: &str, fee_rate: Option<f64>) -> RefreshResult` | user-pays: the fee comes from the coin (1-in-1-out, `BACKUP_TX_VBYTES` = 112 vB), so the fresh coin is `amount − fee`. Errors on a non-`CONFIRMED` coin, an RGB carrier, or `CoinBelowMaintenanceCost`. `fee_rate` is capped at the client's `max_fee_rate`; `None` uses the SE-quoted rate |
| `refresh_sponsored` | `(statechain_id: &str, sponsor: &UtexoWallet, fee_rate: Option<f64>) -> RefreshResult` | the same re-anchor, then an off-chain rebate from `sponsor`. The rebate is `max(fee_sats + DUST_LIMIT, min_child_value)` — the smallest off-chain-payable amount — so `rebate_sats ≥ fee_sats` and the user ends at least whole; the operator absorbs the difference |
| `rebate_refresh_fee` | `(to_utexo_address: &str, fee_sats: u64) -> TransferResult` | sponsor side; a thin wrapper over `transfer` |
| `auto_refresh_due` | `(margin_blocks: u32) -> Vec<RefreshResult>` | re-anchor confirmed non-carrier coins whose signed-once backup `nLockTime` is within `margin_blocks` of the tip. Carriers are skipped (a plain re-anchor destroys the allocation); coins below their own fee are skipped, not failed. Emits `CoinRefreshed` per coin. `Ok(vec![])` means "nothing was due" — an unreadable carrier set is an `Err` |
| `deadline_safety_due` | `(margin_blocks: u32) -> (Vec<RefreshResult>, Vec<String>)` | the unconditional half of maintenance, and what the background loop actually schedules. Two remedies in order: the cooperative re-anchor above, then **sever from `F`** for whatever the counterparty declined to co-sign. Returns `(re_anchored, severed)`; a coin it could not defend is reported by event, by log line AND by `Err` — never as a clean pass |
| `sever_from_f` | `(statechain_id: &str) -> Vec<ExitStatus>` | broadcast the already-co-signed trigger. `T` carries `lock_time 0` and no relative timelock and spends `F` directly, so it beats every retained rung by being valid first; from the moment it confirms, every historical key share for this coin authorises a spend of an output that no longer exists. Costs the coin its off-chain life |
| `detrigger_to_owner` | `(statechain_id: &str, to_address: Option<String>) -> String` | answer a griefer's confirmed `T` with a fresh spend of `T`'s payload at zero CSV wait, paying an address you name (this wallet's own backup address by default). Returns the de-trigger txid. This is an **EXIT**, not a re-anchor: there is no fresh `F′` and no rebuilt `T′/X′_0/S′_0`, so getting back off-chain is a fresh deposit (`sdk89`) |
| `colored_reanchor` | `(statechain_id: &str) -> String` | the coloured variant, where the de-trigger carries a valid RGB transition so the allocation lands on its payload output and the coin CAN be re-laddered from the new outpoint. Refuses a plain ladder |
| `maintenance_plan` | **sync, free fn** `(&SdkConfig) -> Vec<MaintenancePass>` | the passes a background tick runs, as a value. `MaintenancePass::DeadlineSafety` is unconditional — not gated on `auto_refresh`, `background_auto_refresh`, or anything else |

### Tokens (issuer)

Where `colored_ladder` is on, an issued carrier is laddered coloured at `claim()` like any other
coin. Where it is off — a network with no enclave to pin — the carrier stays flat and rides the
legacy RGB-aware split lane end to end: never laddered, never plain-exited. A carrier is never
plain-split on either lane, and now structurally so: the plain off-chain split is deleted.

| Method | Signature | Notes |
|---|---|---|
| `get_token_funding_address` | `() -> String` | fund the RGB engine before issuing |
| `get_token_l1_address` | `() -> String` | alias of `get_token_funding_address` |
| `issue_token` | `(ticker: &str, name: &str, precision: u8, supply: u64) -> String` | RGB NIA onto a fresh statechain coin of `TOKEN_CARRIER_SATS`; returns the `rgb:…` asset id |
| `issue_token_sized` | `(ticker, name, precision, supply, carrier_sats: u64) -> String` | the same with the carrier's sats chosen by the caller. Reach for `issue_token` to issue a token; this is the knob for reproducing and migrating what is already in circulation. A carrier funded below the coloured root floor can never be laddered, and the migration hatch serves exactly those coins |
| `issue_inflatable_token` | `(ticker, name, precision, supply, inflation_amounts: Vec<u64>) -> String` | IFA issuance with reserved inflation rights |
| `issue_inflatable_token_sized` | `(ticker, name, precision, supply, inflation_amounts, carrier_sats) -> String` | the IFA sibling of `issue_token_sized` |
| `mint_tokens` | `(asset_id: &str, inflation_amounts: Vec<u64>) -> (String, u64)` | realize reserved inflation rights as new supply; **broadcasts on chain** and waits for the minted allocation to settle. Returns `(inflate_txid, minted_total)` |
| `mint_tokens_sized` | `(asset_id, inflation_amounts, carrier_sats) -> (String, u64)` | as above with the carrier size chosen |
| `burn_tokens` | `(asset_id: &str, amount: u64) -> String` | burn the engine-held (free) balance, on chain. Statechain-bound supply must be exited into the engine first. Returns the burn txid |
| `query_token_transactions` | `(asset_id: &str) -> Vec<TokenTx>` | RGB-engine transfer history |
| `validate_pending_token` | `(consignment_env: &str, branch_txs: &[String], funding_txid: &str, funding_vout: u32) -> (String, u64)` | validate an un-claimed transfer's consignment WITHOUT booking it, returning `(contract_id, amount cryptographically assigned to the witness outpoint)`. The pre-payment gate an SSP runs before paying a Lightning invoice: the envelope's advisory amount is attacker-controlled, so only this consignment-derived figure is trustworthy |
| `validate_pending_token_ex` | `(consignment_env, branch_txs, child_witness_txids: &[String], funding_txid, funding_vout) -> (String, u64)` | as above for a coloured CHILD, whose witnesses are its own txid chain rather than an exit branch. A non-empty `child_witness_txids` REPLACES the branch |

### Coloured-lane diagnostics

Every one of these probes the RGB **stock** through the fork's off-chain resolver with the coin's own
txid list; none of them reads `get_asset_balance` or `list_unspents`, both of which report a full
settled spendable balance over a stock at zero and would never fire an alarm.

All but `probe_carrier_funding` need the coin to hold a coloured ladder, so they are reachable
wherever `colored_ladder` is on — which is wherever an attestation identity is pinned.
`probe_carrier_funding` is the sibling for a carrier that has none: a carrier on a network still
waiting for its enclave, and the class the migration hatch serves.

| Method | Signature | Notes |
|---|---|---|
| `colored_ladder_health` | `(statechain_id: &str) -> (String, u64, Vec<String>, Option<String>)` | `(contract_id, amount assigned to the final state, tier txids, detail)`. `Err` for a plain or absent ladder, and `Err` when the consignment does not validate — a coloured ladder that cannot be validated off-chain is not "probably fine" |
| `colored_child_health` | `(child_statechain_id: &str) -> (String, u64, Vec<String>, Option<String>)` | the same question for an adopted child, over `colored_child_txids()` |
| `colored_tip_health` | `(statechain_id: &str) -> (String, u64, Vec<String>, Option<String>)` | the same for the sender's own change tip, whose witness list contributes ONE txid for the cap |
| `colored_exit_proof` | `(statechain_id: &str) -> (String, u64, Option<String>)` | validates the leaf consignment with an **empty** off-chain witness set, so `Valid` is reachable only once every tier that ever carried the allocation is genuinely mined. Fails before the exit walk, succeeds after |
| `colored_child_exit_proof` | `(child_statechain_id: &str) -> (String, u64, Option<String>)` | the child-lane sibling, over `T, X_m, SP, ext_child, state_child` |
| `probe_colored_tip` | `(statechain_id: &str, amount: u64) -> ()` | read-only stock probe at a root ladder's final-state payload output. Runs `color_psbt`, never `color_psbt_and_consume`, so nothing is consumed |
| `probe_colored_child_tip` | `(child_statechain_id: &str, amount: u64) -> ()` | the same at a child's `child_state` |
| `probe_colored_spine_tip` | `(statechain_id: &str, amount: u64) -> ()` | the same at a spine tip's cap — the shape neither of the other two can read without concluding something positive and wrong |
| `probe_carrier_funding` | `(statechain_id: &str, asset_id: &str, amount: u64) -> ()` | the same at a FLAT carrier's confirmed funding output, for coins that have no ladder to have a tip. The prevout is read from the chain, not from the coin record |
| `renew_colored_ladder` | `(statechain_id: &str) -> u32` | renew off-chain; returns the new renewal counter `m`. The new extension rivals the old one over the trigger's payload output, and the seal rung folds in both the counter and the strictly lower CSV so the two transitions cannot collapse |
| `renew_colored_ladder_with` | `(statechain_id: &str, csv_e: u16, csv_d: u16) -> u32` | as above with hand-picked CSVs; the new extension CSV must still be strictly lower |
| `transfer_colored_carrier` | `(statechain_id: &str, receiver_address: &str) -> ()` | convey a whole coloured carrier, sats and allocation together. The consignment is validated against the ladder BEFORE any SE co-sign, so a seal collision is a refusal here rather than an unvalidatable consignment at the receiver |
| `transfer_colored_child` | `(child_statechain_id: &str, receiver_address: &str) -> ()` | re-transfer an adopted coloured child whole. A plain re-transfer over `ext_child`'s sealed payload output would burn the allocation, which is what `tesr::refuse_uncolored_over_colored_child` refuses and this is the route it points at |

### Lightning

Lightning works in **both directions on the ladder** through an SSP (a statechain wallet plus an RLN
node) using a **HODL-invoice latch**. PAY: the user latches a coin to the invoice's payment hash and
hands it over; the SSP censuses it, pays the BOLT11, and the LN preimage is simultaneously the
user's proof of payment and the SSP's key to unlock the coin. RECEIVE: the SSP latches a coin to an
SE-held preimage and issues a HODL invoice on that hash; it can only retrieve the preimage — and so
claim the HTLC — after releasing the coin. Exact amounts use a whole coin (`sdk63` pay, `sdk64`
receive); non-exact amounts use the in-ladder split with the piece child latched (`sdk65` pay,
`sdk67` receive). Failures roll back (`sdk66`, `sdk68`). The same calls work against a remote SSP
over HTTP (`sdk21`). See [LIGHTNING.md](../spec/LIGHTNING.md).

**User side** — `ssp` is any `&impl Ssp` (in-process `SspService` or remote `SspClient`):

| Method | Signature | Notes |
|---|---|---|
| `pay_lightning_invoice` | `(ssp: &impl Ssp, invoice: &str) -> String` | quote → latch → SSP pays → returns the **preimage**. Auto-routes: an exact coin when the wallet already holds one, otherwise the non-exact in-ladder lane (REQ-42 — and since `ensure_exact_coin` no longer mints, that fallback is now the ordinary path, not the exception); an RGB invoice latches a coloured coin instead |
| `pay_lightning_invoice_reclaimable` | `(ssp: &impl Ssp, invoice: &str) -> std::result::Result<String, (String, anyhow::Error)>` | the same, but the error carries the latched coin's statechain id for `reclaim_lightning_payment`. An empty string means nothing was latched |
| `pay_lightning_invoice_inladder` | `(ssp: &impl Ssp, invoice: &str, parent_statechain_id: &str) -> String` | explicit non-exact pay from one laddered coin. On failure the split is rolled back and the whole parent is recovered (`sdk66`). RGB invoices are refused on this lane |
| `create_lightning_invoice` | `(ssp: &impl Ssp, amount_sats: u64) -> ReceiveSwap` | receive sats: returns the BOLT11 to hand the payer; the coin lands via the background watcher (`TransferClaimed`) |
| `create_lightning_invoice_asset` | `(ssp: &SspService, asset_id: &str, asset_amount: u64) -> ReceiveSwap` | receive an RGB asset onto a coloured coin (local SSP only) |
| `reclaim_lightning_payment` | `(coin_statechain_id: &str) -> ()` | recover a coin whose pay swap never settled. **Only call once you have positively confirmed non-payment** — a client timeout is not proof, and after the SE `batch_timeout` this succeeds even if the SSP did pay. On a laddered coin it restores the coin locally as exitable; the failed latch left an orphan co-signed state, so off-chain re-transfer stays blocked until a `refresh` |

**SSP side** — `SspService::new(wallet, RlnClient::new(api_url), fee_sats)`; `SspClient::new(base_url)`
speaks the deployed `mercury-ssp` HTTP API. The `Ssp` trait is `info` / `quote_pay` / `execute_pay` /
`create_receive`; `settle_receive` is deliberately not on it (it is never a user operation).

| Method | Signature | Notes |
|---|---|---|
| `SspService::quote_pay` | `(invoice: &str) -> PayQuote` | what the user must latch over, and to which address. Zero-amount invoices are refused |
| `SspService::execute_pay` | `(invoice: &str, batch_id: &str) -> String` | pre-payment gate (latch hash matches the invoice, every latched coin is a pending transfer addressed to the SSP, census-bound value ≥ invoice + fee) → pay → unlock by preimage → claim (`sdk37`) |
| `SspService::create_receive` | `(amount_sats: u64, receiver_address: &str) -> ReceiveSwap` | an exact coin when the SSP already holds one, else a non-exact in-ladder piece latched under an SE-minted preimage |
| `SspService::create_receive_asset` | `(asset_id: &str, asset_amount: u64, receiver_address: &str) -> ReceiveSwap` | coloured receive swap plus an RGB HODL invoice |
| `SspService::settle_receive` | `(&ReceiveSwap) -> ()` | wait for the HTLC to be held, release the coin, then retrieve the preimage and claim the HODL invoice. Requires `ReceiveSwap::statechain_id` to be `Some`, which it is only for a locally-created swap |
| `SspService::cancel_receive` | `(&ReceiveSwap) -> ()` | cancel the HODL invoice and reclaim the un-released coin |

`RlnClient` wraps the Lightning node directly: `decode`, `decode_invoice`, `ln_invoice`,
`ln_invoice_asset`, `send_payment`, `payment`, `invoice_status`, `claim_hodl`, `cancel_hodl`,
`create_utxos`, `refresh`, `issue_asset`, `asset_balance`, `open_asset_channel`.

**Raw latch primitives** (for an LSP integration driving its own Lightning node):

| Method | Signature | Notes |
|---|---|---|
| `start_lightning_swap` | `(counterparty_address: &str, statechain_id: Option<String>) -> LightningSwap` | latch transfer locked on a fresh SE-held preimage; never auto-selects a token carrier |
| `get_swap_payment_hash` | `(batch_id: &str) -> Option<String>` | counterparty-side verification |
| `settle_lightning_swap` | `(&LightningSwap) -> String` | unlock and return the preimage (hex) |
| `latch_tokens` | `(asset_id: &str, receiver_address: &str, token_amount: u64, payment_hash: &str) -> (batch_id, piece_statechain_id)` | coloured transfer latched on an **external** payment hash (RGB pay) |
| `latch_tokens_se_preimage` | `(asset_id: &str, receiver_address: &str, token_amount: u64) -> (batch_id, piece_statechain_id, payment_hash)` | coloured transfer latched on an **SE-held** preimage (RGB receive) |

### Invoices

A self-describing payment request: the recipient's utexo address plus the requested amount, an
optional asset (sats when absent), memo and expiry. A payer fulfills it in one call.

| Method | Signature | Notes |
|---|---|---|
| `create_sats_invoice` | `(amount: u64, memo: Option<String>, expiry_unix: Option<u64>) -> String` | sats request payable to this wallet; returns a `utexoinv1…` string |
| `create_tokens_invoice` | `(asset_id: &str, amount: u64, memo: Option<String>, expiry_unix: Option<u64>) -> String` | token request payable to this wallet |
| `fulfill_utexo_invoice` | `(invoice: &str) -> TransferResult` | decode, check expiry, then `transfer` or `transfer_tokens` to the embedded address |

Free functions, re-exported from the crate root: `encode_utexo_invoice(&UtexoInvoice) -> String`
encodes as `utexoinv1<hex(json)>`; `decode_utexo_invoice(&str) -> UtexoInvoice` parses one back. The
decoder probes the version field FIRST and refuses an unknown one as
`SdkError::UnsupportedVersion` rather than mis-parsing a layout it does not understand.

### Exit & watchtower

| Method | Signature | Notes |
|---|---|---|
| `withdraw` | `(to_address: &str, statechain_ids: Option<Vec<String>>, fee_rate: Option<f64>) -> Vec<String>` | cooperative, SE-co-signed, no timelock wait; sub-coin branches are materialized first. Refuses RGB carriers, hard-erroring when one is named explicitly. A received child or a spine tip has no confirmed outpoint to spend, so it is routed to `unilateral_exit` and marked `WITHDRAWING` |
| `unilateral_exit` | `(statechain_ids: Option<Vec<String>>, to_address: Option<String>) -> Vec<ExitStatus>` | no SE needed. Walks the tier chain as each relative CSV matures — idempotent and incremental, so call once per block until `complete`; `wait_blocks` is the remaining maturity of the next tier (`sdk50`). Refuses a coin that is not `CONFIRMED`, and refuses a carrier unless its ladder is coloured. `to_address` is accepted but unused — every path pays the coin's own pre-signed payee, a seed-derived address of this wallet |
| `materialise_carrier` | `(statechain_id: &str) -> bool` | settle an un-colourable carrier's ALLOCATION on chain by broadcasting its stored exit branch. **Not an exit**: the sats stay on the live 2-of-2 outpoint and still need the SE. Refuses any carrier that has, or could still be given, a coloured ladder. Returns whether a branch was broadcast; the settlement is verified against the chain before returning (`sdk78`) |
| `defend_ladders` | `() -> Vec<String>` | owner-run ladder watchtower pass. A no-op while `F` is unspent — an idle ladder has nothing to defend. If someone triggers the coin, this races the owner's tiers; the adopted current state carries the strictly-lowest CSV, so it matures first and pays the owner. Idempotent; call once per block. Emits `LadderDefended` (`sdk51`) |
| `auto_exit_due` | `(margin_blocks: u32) -> Vec<String>` | near-deadline pass over every off-chain shape: a coin whose exit material is a `branch-` chain — a leaf or sub-coin with no ladder — is force-exited (`LeafExitForced`); the gate is a VERIFIED non-empty branch read, so a coin with no branch is skipped for having no deadline rather than for an unreadable one; received token carriers are **materialized** branch-only, because a backup sweep would destroy the allocation (`TokenCarrierMaterialized`); adopted children (`ctesr-`) and this wallet's own spine tips (`spinetip-`) are walked out, each with a `head_start` of its own `Σ csv` subtracted from the deadline, because a chain of relative timelocks must be STARTED early enough to finish. A leaf whose split journal shows it terminalized is skipped rather than driven, so the pass can never rival a grandchild already handed to a payee. Run by the background watcher each poll when `auto_exit` |
| `export_watch_bundle` | `() -> String` | **keyless** watch bundle (JSON `WatchBundle`): per off-chain coin, the exit branch and deadline, the backup for plain coins, and a `WatchTrigger` where the race is event-based rather than height-based. No mnemonic, no key shares, no RGB seed — safe to hand to untrusted watchtowers. Fails closed rather than silently omitting an entry it cannot build. Re-export after any transfer, claim, split or re-anchor (`sdk45`) |
| `estimate_exit_cost` | `(statechain_id: &str) -> ExitCostEstimate` | tx count, vbytes, `wait_blocks` (when the exit **completes**) and `exit_deadline_block` (the safety deadline). Read `exit_deadline_blind` with it — see the note below |
| `get_withdrawal_fee_quote` | `(statechain_ids: Option<Vec<String>>) -> WithdrawalFeeQuote` | cooperative-withdrawal fee quote at the current electrum-estimated rate, ~111 vB per coin |
| `watchtower_faults` | `() -> Vec<WatchtowerFault>` | every deadline-critical pass that is currently BLIND, with `consecutive_failures`, `since_unix`, `last_unix`. Poll it next to `get_balance` and alert on a non-empty result: while it is non-empty, nothing is racing a clawback or a hostile trigger on this wallet's behalf |
| `is_watchtower_blind` | `() -> bool` | convenience over the above |
| `fee_float_solvency` | `() -> Option<mercuryrustlib::tower_float::Solvency>` | can the configured fee float cover every coin this wallet defends, in BOTH units? A float with plenty of sats in ONE utxo funds exactly one simultaneous rescue, because a v3 fee child may have only one unconfirmed ancestor. `Ok(None)` when no `fee_bump` is configured — a keyless wallet is out of scope, not underfunded |

**"No deadline" and "I could not compute a deadline" are different answers.**
`exit_deadline_block == None` with `exit_deadline_blind == None` means the coin is flat and
genuinely unraceable. `exit_deadline_blind == Some(reason)` means the coin HAS a branch, a deadline
therefore exists, and it could not be computed. Anything deadline-critical must branch on
`ExitCostEstimate::deadline_is_unknown()`, not on the `Option`. `auto_exit_due` routes a blind coin
into `WalletEvent::WatchtowerBlind` plus a retained `WatchtowerFault` and returns `Err`; it
deliberately does NOT force-materialize on suspicion, because a single backend blip would otherwise
dump every off-chain coin in the wallet on chain.

**Keyless towers.** Three free functions, all **sync**, all needing only an electrum connection:

| Function | Signature | Notes |
|---|---|---|
| `watchtower::watch_pass` | `(bundle: &WatchBundle, electrum: &electrum_client::Client, margin_blocks: u32) -> WatchState` | one keyless iteration from a bundle. An entry is due when EITHER predicate fires: the height one, or — where a `WatchTrigger` is present — the event one (the watched outpoint has been spent). Idempotent, so several independent towers can run it |
| `mercuryrustlib::tesr::watch_pass` | `(electrum, bundle: &TesrBundle) -> WatchState` | the laddered tower, same vocabulary |
| `mercuryrustlib::tesr::exit_pass` / `exit_child_pass` | `(electrum, bundle) -> Result<ExitProgress>` | one tier-walking step; `next_exit_tier` / `next_child_exit_tier` return `Result<Option<u16>>`, so an unreadable backend is an `Err` and never a silent "nothing to do" |

`WatchState` is the shared vocabulary and it has four states, not two:

- `Idle` — the tip was read, **every** entry was evaluated on both predicates, and none was due. A
  positive observation.
- `Acted { ids, failures, blind }` — the pass was engaged: it broadcast something, tried and was
  rejected, or could not evaluate an entry. `blind` names entries that were not watched at all this
  pass and must be alerted on, not averaged away by the entries that were.
- `Blind { reason }` — the chain backend could not be read, so no deadline was evaluated. **A tower
  that could not see is not an idle tower.**
- `Void { spender, detail }` — `F` was spent by something that is not this bundle's trigger, so every
  tier below `T` is permanently unconfirmable. "I saw, and this coin is gone", not "retry".

## Events (`WalletEvent`)

| Event | Payload |
|---|---|
| `DepositConfirmed` | `{address, amount_sats}` |
| `TransferClaimed` | `{statechain_ids}` |
| `TransferCancelled` | `{statechain_ids}` — an expected incoming transfer was withdrawn by its sender; no coin will appear. Its own event because the alternative is indistinguishable from silence |
| `TokenTransferClaimed` | `{asset_id, amount, statechain_id}` |
| `BalanceUpdate` | `{balance}` |
| `LadderEstablished` | `{statechain_id}` — `claim()` established the TES-R ladder on a fresh confirmed root coin |
| `LadderSkipped` | `{statechain_id, reason}` — a confirmed coin was left flat-only. Emitted only when the recorded reason CHANGES, so read it back with `ladder_skip_reason` / `flat_only_coins` rather than relying on having been subscribed |
| `LadderDefended` | `{statechain_id, tiers_broadcast}` — the coin was found triggered and `defend_ladders` broadcast tier tx(s) this pass; emitted per pass until the exit completes |
| `ExitBranchConflict` | `{statechain_id}` — a competing tx is spending the branch root; fee-bump or re-attempt, and do **not** assume the coin exited |
| `ExitDeadlineApproaching` | `{statechain_id, deadline_block, tip}` |
| `LeafExitForced` | `{statechain_id, deadline_block, tip}` — a plain (uncoloured) leaf was driven to L1 to beat its deadline. Distinct from the carrier event on purpose: `near_deadline_exit_event` chooses between the two on the coin's COLOUR, and emitting the token event for a plain coin would mis-report it to any integrator watching the stream |
| `TokenCarrierMaterialized` | `{statechain_id, deadline_block, tip}` — a received token carrier was settled on chain, branch-only, before its clawback deadline |
| `CoinRefreshed` | `{old_statechain_id, new_statechain_id, fee_sats}` — re-export the recovery and watch bundles |
| `WatchtowerBlind` | `{pass, detail}` — a deadline-critical pass could not SEE, so it did not act. Emitted on EVERY failing pass, so a late subscriber still learns the wallet is blind. `pass` is `WatchtowerPass::{AutoExit, DefendLadders}`, whose `as_str()` gives the stable wire spelling |
| `ColoredExitTipRegistered` | `{statechain_id, outpoint}` — a completed coloured exit's payload output was registered with the RGB engine |
| `ColoredExitTipUnregistered` | `{statechain_id, detail}` — the coloured exit landed but the engine could not be told where. The coin is safe; every UTXO-driven rgb-lib view is stale until the pass is re-run |

## Errors (`SdkError`)

| Variant | Meaning |
|---|---|
| `TokenPaymentRequired{token_id, deposit_address, fee_sats}` | pay for a deposit token, then retry |
| `InsufficientBalance{requested_sats, available_sats}` | |
| `NoExactAmount{requested_sats}` | no exact subset and split is disabled for this call |
| `TokensNotConfigured` | set `rgb_proxy_url` + `rgb_data_dir` |
| `CoinBelowMaintenanceCost{statechain_id, amount_sats, fee_sats}` | the coin's value is at or below its own re-anchor fee, so it cannot pay to move itself. Not lost — combine it with another coin. Such coins are excluded from routine auto-refresh and reported in `TransferQuote::stuck_coins` |
| `BatchTooManyRecipients{recipients, slots, cap, max_recipients}` | a batch needs `K + 1` slots and a coin may only ever vouch `cap` = 64 derived slots, so `K ≤ 63`. Refused locally before any SE call |
| `UnsupportedVersion{kind, found, supported}` | a self-describing decoder refused a declared format version it cannot interpret — distinguishable from malformed bytes |

Everything else surfaces as `anyhow::Error`.

## Config (`SdkConfig`)

Presets: `SdkConfig::regtest(name)`, `SdkConfig::mainnet(name, se_url, electrum_url)`. Fields:
`wallet_name`, `statechain_entity_url`, `electrum_url`, `electrum_type`, `network`, `database_file`,
`confirmation_target`, `rgb_proxy_url` + `rgb_data_dir` (both required for token support),
`deposit_token_id`, `poll_interval_secs`, plus:

| Field | Default | Meaning |
|---|---|---|
| `auto_refresh` | `true` | run the pre-spend re-anchor hook inside `transfer`/`transfer_many`, so the cost appears as a payment fee instead of a balance shrinking in the background |
| `auto_refresh_margin_blocks` | `144` | backup-locktime headroom at or below which a coin is re-anchored. Must exceed the SE `interval` so a whole-coin handover still validates |
| `background_auto_refresh` | `false` | also run the ROUTINE re-anchor from the background watcher. Off by default. It does not gate deadline safety, which is unconditional |
| `auto_exit` | `true` | run `auto_exit_due` from the background watcher |
| `auto_exit_margin_blocks` | **derived** — 860 regtest, 2 120 mainnet | `auto_exit_margin_blocks_for(AUDIT_17_K_MAX, interval, AUTO_EXIT_MODELLED_DEPTH)` = `k_max·interval + tesr_exit_txs(d)·144`: the ancestor-locktime gap plus ONE confirmation window per SEQUENTIAL transaction of the exit walk. The regtest SE `interval` is 10 and the mainnet one is 100, so a single shared literal would be wrong on one of them |
| `fee_bump` | `None` | `FeeBumpConfig{core_rpc_url, core_rpc_user, core_rpc_password, funding_secret_key_hex, target_fee_rate, reserve_bumps_per_coin}`. `None` means the wallet **cannot** bump, which is the honest default: a tier refused for fee reasons is reported as a stated limit rather than retried forever at the same committed rate. Set it and `unilateral_exit` / `defend_ladders` escalate a refused tier to a 1P1C package. The key funds FEES ONLY and is never a coin key |
| `colored_ladder` | **derived** — `true` regtest, `false` mainnet | build a COLOURED TES-R ladder over an RGB carrier at `claim()` instead of leaving it on the flat lane. Not a stated bool: both constructors READ `TesrParams::attestation_identity_const` for the network, because the coloured lane cannot establish a ladder without a pinned attestation identity and true-without-a-pin ships a wallet whose token lane refuses forever. Mainnet is `false` only because no mainnet enclave is provisioned yet; pinning one flips it. Cost figures for the lane it switches on: [PARTIAL-PAYMENT-ECONOMICS.md](../spec/PARTIAL-PAYMENT-ECONOMICS.md) |
| `attestation_identity` | `None` | the enclave attestation identity this wallet verifies sig-count attestations against. Resolution is **compiled-in pin → this field → REFUSE**: a value that disagrees with a compiled-in pin is an error, not an override, and `None` with no compiled-in pin makes every laddering claim refuse (`LadderSkipReason::AttestationIdentityUnpinned`). Falls back to the `UTEXO_ATTESTATION_IDENTITY` environment variable. Read the value from the enclave's `GET /attestation_identity` |

## Types

`serde`-serializable (what the language bindings marshal) except `RefreshResult`, `WalletEvent`,
`WatchtowerFault`, `WatchtowerPass`, `LadderSkipReason`, `WatchState`, the Lightning types, the
recovery/conveyance report types and `InLadderLatch`, which are `Clone + Debug` only. The three
watch-bundle types (`WatchBundle`, `WatchEntry`, `WatchTrigger`) ARE serde — that is the wire format
a keyless tower consumes.

- `Balance{available_sats, pending_sats, in_transfer_sats, tokens}`, `TokenBalance`,
  `CoinInfo{statechain_id, amount_sats, status, utxo_txid, utxo_vout, off_chain}`
- `TransferResult{receiver_address, total_sats, coins: Vec<TransferredCoin>, used_split}`
- `TransferQuote{amount_sats, network_fee_sats, renewal_fee_sats, total_fee_sats, fundable,
  stuck_coins, no_exit_material_coins, note}` — `no_exit_material_coins` is distinct from
  `stuck_coins`: those have a fee problem combining rescues, these are missing the exit material
  itself and combining does not help. Their value is not counted in `fundable`
- `ClaimResult{claimed_transfers, confirmed_deposits, token_results, cancelled_transfers}` with
  `TokenClaimStatus{statechain_id, state, asset_id, amount, detail}` and
  `TokenClaimState::{Booked, Pending, Rejected}` — a claimed sats transfer and its token booking are
  separate steps, so read `token_results` rather than inferring "tokens received" from
  `claimed_transfers`
- `RefreshResult{old_statechain_id, new_statechain_id, old_amount_sats, new_amount_sats, fee_sats,
  refresh_txid, rebate_sats}`
- `ExitCostEstimate{statechain_id, branch_txs, branch_vbytes, backup_vbytes, total_vbytes,
  wait_blocks, exit_deadline_block, exit_deadline_blind}` with `fee_sats_at(rate)` and
  `deadline_is_unknown()`; `ExitStatus{statechain_id, complete, wait_blocks}`;
  `WithdrawalFeeQuote{n_coins, est_vbytes, fee_rate_sat_vb, fee_sats}`
- `WatchBundle{version, wallet_name, entries}` /
  `WatchEntry{statechain_id, token_carrier, deadline_block, branch_txs, backup_tx?,
  backup_locktime?, trigger?}` / `WatchTrigger{watch_txid, watch_vout, csv_blocks, push_txs}`
- `WatchtowerFault{pass, detail, consecutive_failures, since_unix, last_unix}`,
  `WatchtowerPass::{AutoExit, DefendLadders}`, `LadderSkipReason` (thirteen variants, with
  `as_str()`, `from_str()` and `permits_flat_conveyance()` — the last a PREDICTION an app can warn
  on, never a substitute for the conveyance-time classifier)
- `InLadderSplitRecovery{op_id, lane, terminalized_statechain_id, outcome}` with
  `InLadderSplitOutcome::{Replayed{change_statechain_id, unconveyed_pieces}, Retryable,
  CooperativePathLost}`; `PendingConveyance{op_id, lane, terminalized_statechain_id, outstanding,
  stranded}`; `StructuralSpendRecovery`, `StructuralSpendRecord`, `StructuralStage`, `BatchPiece`
- `UtexoInvoice{version, address, amount, asset_id, memo, expiry_unix}`,
  `TokenTx{kind, status, amount, txid}`
- Lightning: `LightningSwap{batch_id, payment_hash, statechain_id}`,
  `PayQuote{amount_sats, fee_sats, payment_hash, ssp_address, asset_id, asset_amount}`,
  `ReceiveSwap{batch_id, statechain_id, invoice, payment_hash, asset_id, asset_amount}`,
  `SspInfo{ssp_address, fee_sats}`, `DecodedInvoice{amt_msat, payment_hash, asset_id, asset_amount}`,
  `AssetBalance`, and `InLadderLatch::{None, External(&str), ClassicMinted}` — a call-site enum
  choosing how an in-ladder piece is latched (`None` = plain payment, `External` = non-exact LN pay,
  `ClassicMinted` = non-exact LN receive)

### Free functions and models

| Symbol | Notes |
|---|---|
| `types::is_terminal(sig_budget: Option<i64>, finalized: i64) -> bool` | mirrors the SE's terminal predicate; the authoritative value comes from `GET /statechain/spend_budget` |
| `select::{Candidate, Plan, exact_subset, plan, plan_with_floor}` | the coin-selection primitives behind `transfer`. `Plan` is `Exact(Vec<usize>)`, `WithSplit{whole, split, split_amount}` or `Insufficient{available}` |
| `config::tesr_exit_txs(d) -> u32` | transactions a unilateral exit must confirm IN SEQUENCE at split depth `d`: `3 + 2d`. **Every safety margin must use this one** — over-counting makes a watchtower act earlier, which is the safe direction |
| `config::tesr_exit_txs_for(ExitShape, d) -> u32` | the shape-aware count: `TwoTier` → `3 + 2d`, `Spine` → `4 + d`. Publish economics with this; size margins with the bare name |
| `config::tesr_exit_vbytes(d) -> u64` | the signed vsize of that walk, from the measured `TIER_VBYTES` = 125 and `P2TR_OUT_VBYTES` = 43: `293·d + 375` vB uncoloured |
| `config::tesr_exit_wait_blocks(&TesrParams, d) -> u32` | exit latency in blocks: the walk's relative timelocks plus one confirmation per tier |
| `config::tesr_exit_csv_total(&TesrParams, d) -> u32` | those relative timelocks alone, with no confirmation budget |
| `config::auto_exit_margin_blocks_for(k_max, interval, d) -> u32` | the derivation behind `auto_exit_margin_blocks`: `k_max·interval + tesr_exit_txs(d)·BLOCKS_PER_DAY` |
| `config::{SE_INTERVAL_DEPLOYED, SE_INTERVAL_DEFAULT}` | the regtest (10) and mainnet (100) SE `interval`, read at compile time from `TesrParams::flat_ladder_params_const` rather than transcribed, so a margin can never be sized against a ladder nobody runs |
| `config::{BLOCKS_PER_DAY, AUDIT_17_K_MAX, AUTO_EXIT_MODELLED_DEPTH}` | the remaining terms, and they are chosen rather than derived: 144, 14 and 1. `AUDIT_17_K_MAX` is the **assumption** in the margin — it bounds the pre-split hops the deposit-anchored deadline over-estimates by, and nothing conveys the true hop count to a receiver |

## What a payment costs

From [PARTIAL-PAYMENT-ECONOMICS.md](../spec/PARTIAL-PAYMENT-ECONOMICS.md), against ~154 vB for an
ordinary on-chain payment:

| Per payment on the leaf lane | Block space |
|---|---|
| spent onward off-chain | **0 vB** |
| swept and settled | **~105 vB** — 1.47× better, and the cap without the discharge round |
| shipped default | **418 vB** |
| walked out unilaterally | **250 – 2 719 vB** |

The discharge round that would make the swept row the ordinary outcome (SPEC.md §5.4) is **DESIGN,
NOT BUILT**: its SE enforcement point is empty, so nothing in this SDK reaches it and no method
below assumes it.
