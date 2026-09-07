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

There is **one protocol** and **one exit material**: a coin's TES-R ladder, established at the
**first mempool sighting** of its funding transaction — by `coin_status::check_deposit` under
`LadderAtSight::Plain` for a caller with no RGB engine, or by `claim()`'s establish pass under
`LadderAtSight::Defer`, which ladders every un-laddered `IN_MEMPOOL` / `UNCONFIRMED` / `CONFIRMED`
coin in the same pass, plain or **coloured** for a carrier whose allocation is booked. There is no
`deposit_protocol_version` field, no `UTEXO_PROTOCOL_DEFAULT` environment variable, and **no flat
backup**: no coin carries an absolute-locktime backup transaction, at deposit or at any hop, and
`coin.locktime` is `None` for life. Coin shape follows from what the coin *is*, and a handful of
methods behave differently on each shape. Three of the four are LADDERED shapes and a wallet can
hold all of them at once; the fourth is the absence of a ladder, which is the absence of exit
material — a state to repair, never a lane to pick.

| | **LADDERED ROOT** | **RECEIVED CHILD** | **SPINE TIP** | **NO LADDER** |
|---|---|---|---|---|
| Which coins | every BTC deposit, laddered at first sight of `F` (`tesr-` row) — and, wherever `colored_ladder` is on, every RGB **carrier** too, laddered *coloured* in `claim()`'s establish pass | a piece adopted from someone's in-ladder split (`ctesr-` row) | this wallet's own change leg after an in-ladder payment (`spinetip-` row) | a coin whose ladder could not be established, with a `LadderSkipReason` recorded for it: a carrier that cannot be coloured (`RgbCarrier`), a plain ladder found over a carrier (`PlainLadderOverCarrier` — the one variant that is *wrongly* laddered rather than un-laddered, and it has **no remedy**: `colored_reanchor` refuses a plain ladder by name and a plain `refresh` would destroy the allocation, so the allocation is stranded), an unreadable ladder row, an SDK deposit on a network with no pinned attestation identity (`AttestationIdentityUnpinned` — the establish pass gates on the attested `get_statechain_info`; `check_deposit` under `LadderAtSight::Plain` does not, so THAT lane ladders unpinned), … Under `LadderAtSight::Plain` a deposit whose ladder cannot be built at first sight is not even booked — it stays `INITIALISED` and is retried; under `Defer` the deposit IS booked and the skip is recorded against it |
| Exit material | `F → T` (TRIGGER, no timelock) `→ X_m` (EXTENSION, relative CSV) `→ S_k` (STATE, relative CSV) — v3/TRUC with a P2A anchor, pre-signed and **un-broadcast** | the root walk plus every intermediate segment, then `ext_child → state_child` | the root walk plus every intermediate segment, then ONE cap over `SP_i.out[K]` | **none.** The only rows this SDK still reads for such a coin are the `branch-` rows of the retired coloured split/combine lane, and only `materialise_carrier` reads them |
| Ageing | **none.** BIP-68 relative locks start counting only once the parent confirms and `T` carries no timelock, so nothing matures until someone broadcasts `T`. An idle coin never ages: 0 vB of idle rent, and no absolute clock of any kind (INV-27, unconditional) | none — its exposure is the parent's trigger being broadcast, an event, not a height | same as a child | none — nothing on it can mature, because nothing on it is signed |
| Transfer whole | co-sign a fresh state one δ **lower**, disclosing the replaced one for the receiver's census; the message conveys **no** `backup_transactions` (an empty vector is REQUIRED by the receiver) | `tesr::child_retransfer` — same rule, one level down | **refused** by `transfer`: there is no spine-tip conveyance builder | **refused by name** in `transfer_sender::execute_ex`, before any SE co-sign ("a coin's only exit material is its TES-R ladder … there is no un-laddered lane"). The flat conveyance lane's licence classifier is *deleted* — `assert_flat_conveyance_is_legitimate` no longer exists and `PermanentLicence` survives only in comments — and `is_legitimate_flat_reason` answers `false` for every reason. Duplicate deposits cannot be conveyed either, whatever `force_send` says |
| Non-exact payment | `in_ladder_pay` / `in_ladder_pay_many` — a state tier `SP` over `X_m.out[0]`, a descendant of `T`, never a rival for `F` | `child_in_ladder_pay` / `child_in_ladder_pay_many` | `spine_batch_pay` / `spine_batch_pay_many` — `SP_{i+1}` over `SP_i.out[K]` | **none.** `parent_shape` refuses instead of returning a shape: the plain off-chain split that served this row spent the coin's funding output `F` directly, which is what a prior owner's retained un-timelocked trigger also spends [B1], and it is DELETED |
| Maintenance | off-chain renewal and rollover, unbounded (`sdk43`); `refresh` is the on-chain **re-anchor**, not a deadline reset (`sdk30`) — a coin's off-chain life is bounded by renewals and rollover only | none needed | none needed | `claim()` retries the transient reasons every pass |
| Unilateral exit | `unilateral_exit` walks the chain tier by tier; idempotent, call once per block until `complete` (`sdk50`) | walks `T → X_m → SP → ext_child → state_child` (`sdk59`, `sdk60`) | walks the prefix, then its cap | **refused by name** — there is no flat exit fallback: "a coin's only exit material is its ladder, and there is no flat backup to fall back to" |

**The fourth column is empty by construction, and that is the point.** `split_coin`, the plain
off-chain split, and `ParentShape::Unladdered` with it, are gone; `ensure_exact_coin` no longer
mints; the off-chain branch split/combine (`register_split_subcoins_n`,
`register_combine_subcoins`) refuse by name, and `refuse_legacy_colored_split_lane` refuses on
**both** settings of `colored_ladder`, so the migration hatch for un-colourable legacy carriers is
closed with the lane; and there is no flat backup for a no-ladder coin to fall back on. So nothing in this SDK *produces* a coin whose exit is an absolute-locktime spend of
`F`, and the [B1] hazard that shape carried — a prior owner's retained, matured spend of `F`
voiding whatever descends from it, with no way for the receiver to detect the exposure — is closed
by construction rather than by a refusal inside one function. The only spends of `F` in a past
owner's hands are the retained trigger copies (no timelock, so the current owner or their watcher
can always pre-empt them by broadcasting the same `T`) and the superseded states below it (which
lose the CSV race). What did **not** change is the fact underneath it: a split sub-coin's funding
output is un-broadcast and cannot root a trigger. Every in-ladder split child and every spine-tip
change leg still has un-broadcast funding — that is what buys the 0 vB of idle rent — and its exit
chain reaches back through `SP` to the parent's on-chain `F`.

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
| `list_coins` | `() -> Vec<CoinInfo>` | inventory with `status` and `off_chain` (true only when the coin still has a stored `branch-` row from the retired coloured split/combine lane; a laddered coin's `F` is on chain and the field is false) |
| `ladder_skip_reason` | `(statechain_id: &str) -> Option<LadderSkipReason>` | why this coin has no ladder — or, for `PlainLadderOverCarrier`, the wrong kind — read back from the persisted record. `WalletEvent::LadderSkipped` fires only on a transition, so this is the authority for an app that started later. A coin with a reason recorded has **no exit material**; the reason is diagnostic, never a licence |
| `ladder_skip_reason_raw` | `(statechain_id: &str) -> Option<String>` | the exact persisted spelling — preferred when a forward value written by a newer client must not be silently dropped |
| `flat_only_coins` | `() -> Vec<(String, String, bool)>` | `(statechain_id, raw_reason, may_still_be_transferred)` for every coin recorded without a ladder. The third slot is **always `false`** now — `transfer_sender::is_legitimate_flat_reason` answers `false` for every reason, because there is no flat lane to convey on. Such a coin's value is not lost: `claim()` retries the transient reasons, and a carrier is re-coloured once it can be |

### Deposit

| Method | Signature | Notes |
|---|---|---|
| `get_deposit_address` | `(amount_sats: u64) -> String` | fresh single-use address; fund with the exact amount. **Refuses an amount below the plain ladder floor** (`mercurylib::tesr::ladder_floor` = `3 · (committed_fee(rate) + P2A) + dust` = **2 175 sat** at the shipped 3.0 sat/vB), by name and before any address is issued: a coin's only exit material is its ladder, so a deposit that cannot fund three tiers could never become a usable coin. The refusal is here rather than at first sight because `tesr::establish` interleaves building and co-signing — a coin large enough for the trigger alone would burn an irreversible co-signature and then fail, leaving a signature count no bundle can account for. A token carrier's floor is higher (`colored_ladder_floor`, 2 562 sat) |
| `add_prepaid_token` | `(token_id: &str)` | pool a pre-paid SE deposit token. Only fresh ON-CHAIN onboarding slots draw on this pool; split/combine/re-anchor slots use free SE-minted derived tokens |
| `claim` | `() -> ClaimResult` | one watcher pass: run the deposit watcher under `LadderAtSight::Defer`, claim incoming transfers, book incoming consignments, and **establish the TES-R ladder** on every un-laddered `IN_MEMPOOL` / `UNCONFIRMED` / `CONFIRMED` root coin — plain, or **coloured** for a carrier whose allocation is booked (emits `LadderEstablished`). The deposit is seen and laddered in one pass, before it confirms; the enclave count after a deposit is **3** (`T`, `X_0`, `S_0`) |

`claim()`'s laddering step is unconditional but records a `LadderSkipReason` and emits
`LadderSkipped` where it declines — a carrier that cannot be coloured, a coin whose funding `F` is
not on chain, a coordinator that cannot be reached, an unpinned attestation identity, a plain
ladder found over a carrier. It fails **closed**: it never ladders on a guess, never builds a plain
ladder over a carrier, and the next `claim()` retries the transient reasons. The carrier guard that
enforces the second of those had gone **silently dead** and was repaired in the same change: it used
to read `rgb_consignment` off the coin's backup rows, and since no conveyance writes one any more,
that read answered "not a carrier" for every coin — a coloured child could have been given a PLAIN
ladder over its sealed output. It now reads the coin's own conveyed BUNDLE (`tesr::load_child` /
`load_spine_tip`, `is_colored()`), fail-closed on an unreadable row (`RgbStateUnavailable`), and
keeps the legacy row read after it for coins that predate the rule. A coin it declines has
**no exit material** — there is no flat backup underneath — which is why the reason is surfaced
every pass. A cancelled incoming transfer is *reported* (`ClaimResult::cancelled_transfers` +
`WalletEvent::TransferCancelled`), never raised, so one withdrawn payment cannot discard the
deposits and receipts of the same pass.

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
| `transfer_tokens` | `(asset_id: &str, receiver_address: &str, token_amount: u64) -> TransferResult` | the coloured in-ladder split; the consignment travels in the mailbox message. When one carrier is insufficient there is **no combine transaction** — `colored_multi_carrier_transfer` runs ONE in-ladder split per carrier and conveys one child per leg, sequentially and non-atomically (a failure at leg `k` leaves legs `0..k` conveyed and names them). The legacy multi-input combine behind it is unreachable: `refuse_legacy_colored_split_lane` refuses first, on both settings of `colored_ladder` (`sdk31`, re-derived onto this shape) |
| `batch_transfer_tokens` | `(asset_id: &str, transfers: &[(String, u64)]) -> Vec<TransferResult>` | **one recipient only, today.** A coloured carrier routes to `colored_in_ladder_pay`, whose engine calls `refuse_colored_multi_payee` and refuses `K > 1` **by name** — a shipped decision (D43), not a pending fix: the coloured lane conveys serially after the carrier is terminal and journals no `recipient_address`, so a failure at payee `j` strands the rest. Pay coloured recipients one carrier each. The legacy N-piece lane that did serve `K > 1` is retired (`register_split_subcoins_n` refuses) |

**How `transfer` routes.** It runs the pre-spend auto-refresh hook (when `auto_refresh` is on —
inert on a laddered wallet, see below), then plans over confirmed, non-carrier coins that hold a
ladder (`has_exit_material`: a `tesr-`, `ctesr-` or `spinetip-` row — there is no other exit
material):

- exact subset of whole coins → plain key handover per coin;
- a **received child** sent whole → `tesr::child_retransfer`, co-signing a fresh lower-CSV state
  over `ext_child.out[0]` and disclosing the replaced state (`sdk60`);
- a **spine tip** sent whole → refused by name, because there is no spine-tip conveyance builder.
  Checked **twice**: in the SDK hand-over loop, and again in `transfer_sender::execute_ex` itself, so
  a caller that drives `mercuryrustlib` directly cannot walk a tip into the (now non-existent) flat
  lane. The refusal costs the coin nothing: it
  stays unilaterally exitable and its cap already pays this wallet's own key;
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

**Admission floor for split payments — a payee's leg is floored at ONE SATOSHI (REQ-83).** A payee's
leg is no longer always a two-rung child: `mercurylib::tesr::LeafShape::for_value(value, rate, dust)`
selects the shape from the value, and `split_output_floors` admits at the cheapest band's floor,
`SplitLegRole::Tail.min_value(…)` = **1**. The bands at the shipped 3.0 sat/vB committed rate:
`Piece` (extension + state) at ≥ `min_child_value` = **1 560**; `ThinPiece` (one cap rung) at
≥ `min_spine_tip_value` = **945**; `Ladderless` stub (`SP.out[j]` pays the payee's own key, no rung,
no SE slot) at ≥ `DUST_LIMIT` = **330**; `Tail` (sub-dust, coin-backed, zero fee) at ≥ **1**. A leg
below its band's rung count leaves on its group's exit rather than unaided — `LeafShape::rungs()` and
`exits_unaided()` report which, and the SDK never refuses a payment for it.

The sender's **change** leg still carries a ladder, floored at
`max(min_split_output(backup_rate), change_leg_role(lane).min_value(rate, dust))` — one rung
(**945**) on `PlainRoot` / `SpineBatch` / `Colored`, two (**1 560**) on `PlainChild`. Every figure
here takes the fee rate as an argument: they are evaluations at the shipped rate, not constants.

The floor is resolved **per leg**, not once per split: `transfer`, `transfer_many` and
`quote_transfer` all read it from the same internal `split_output_floors` helper in
`clients/libs/rust-sdk/src/transfer.rs`, so a quote cannot admit an amount the executor's admission
guard refuses. The guard runs **before** the parent is terminalized, so a rejected payment leaves the
parent fully spendable (`sdk58`).

**Only the plain in-ladder ROOT lane actually builds the two lower bands.** `tesr::in_ladder_split`
routes a `Stub`-band leg through its `ladderless` argument (the SDK's `in_ladder_pay` sorts
recipients into `children` and `ladderless` before any voucher is spent, so a stub burns no derived
slot) and builds a `Tail` as a coin-backed leg. `tesr::spine_batch_split` refuses both lower bands
**by name** ("this lane carries no ladderless legs — pay it from the plain root lane"), and the
coloured lane refuses a ladderless leg (`verify_ladderless_leaf`) and a coloured tail
(`refuse_coloured_tail`). Nothing is co-signed by those refusals.

⚠️ **The child lane does neither, and that is a defect rather than a third behaviour.**
`tesr::child_in_ladder_split` hard-codes `SplitLegRole::Piece` for every grandchild — it never calls
`LeafShape::for_value` — while `split_output_floors(rate, ParentShape::Child)` still admits a payee's
leg at 1 sat. A sub-`min_child_value` payment out of a **received child** is therefore admitted, and
then built as a two-rung shape that value cannot fund, with the discovery landing inside
`establish_child`. Keep payments out of a received child at or above `min_child_value` = 1 560 sat
until that is reconciled. This is reported as a code defect; the documents do not claim a remedy.

**Batch size.** A K-recipient batch needs at most `K + 1` fresh statechain slots (a `Stub`-band
recipient takes a ladderless leg and no slot at all, so `in_ladder_pay` counts only the non-stub
recipients before drawing vouchers), each costing one derived
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
| `recover_structural_spends` | `() -> Vec<StructuralSpendRecovery>` | replays the journal of the **retired** coloured branch split/combine lanes (`lane = "colored_split"` / `"colored_combine"`); `transfer_tokens` and `batch_transfer_tokens` call it themselves before selecting a carrier, so a journal entry that predates the rule is settled before a new spend touches the same coin. Under the rule the replay's registration step (`register_split_subcoins_n`) refuses, so an open legacy entry stays open and its carriers stay excluded from selection. The live coloured in-ladder split has **no** structural-spend journal and no crash point — a stated KNOWN GAP: a crash between its co-sign and the conveyance leaves the piece child in this wallet, exitable, with the parent terminalized, recoverable by hand |

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
fresh funding outpoint, minting a new statechain id and a new ladder — established at first sight
of the new funding transaction, through the same deposit path as any other coin. It is not a
deadline reset — no coin has a calendar deadline to reset. Reach for it to put a stale-shaped coin
back on a fresh root, to kill every retained trigger copy and superseded state rooted at the old
outpoint by spending it, or to unbrick a coin after a failed latch. Renewal and rollover of a live
ladder are off-chain and cost nothing (`sdk43`); the on-chain cadence of a coin is the cooperative
re-anchor at the renewal/rollover cap, and nothing else (`sdk30`). Refresh is **cooperative**; if
the SE is gone, exit unilaterally. Renewal and rollover are library calls not yet invoked on the
transfer path — renewal is by hand today — and the coloured re-anchor is a manual call.

| Method | Signature | Notes |
|---|---|---|
| `refresh` | `(statechain_id: &str, fee_rate: Option<f64>) -> RefreshResult` | user-pays: the fee comes from the coin (1-in-1-out, `BACKUP_TX_VBYTES` = 112 vB), so the fresh coin is `amount − fee`. Errors on a non-`CONFIRMED` coin, an RGB carrier, or `CoinBelowMaintenanceCost`. `fee_rate` is capped at the client's `max_fee_rate`; `None` uses the SE-quoted rate |
| `refresh_sponsored` | `(statechain_id: &str, sponsor: &UtexoWallet, fee_rate: Option<f64>) -> RefreshResult` | the same re-anchor, then an off-chain rebate from `sponsor`. The rebate is `max(fee_sats + DUST_LIMIT, min_child_value)` = `max(fee + 330, 1 560)` at the shipped rate, so `rebate_sats ≥ fee_sats` and the user ends at least whole; the operator absorbs the difference. `min_child_value` here is this function's own literal, not the admission floor — since REQ-83 a payee's leg is admitted at 1 sat, and the rebate is deliberately sized to a fully-laddered two-rung child instead |
| `rebate_refresh_fee` | `(to_utexo_address: &str, fee_sats: u64) -> TransferResult` | sponsor side; a thin wrapper over `transfer` |
| `auto_refresh_due` | `(margin_blocks: u32) -> Vec<RefreshResult>` | **has no laddered subject.** It selects coins by `coin.locktime − tip ≤ margin_blocks`, and `coin.locktime` is `None` for life, so on a laddered wallet it re-anchors nothing and returns `Ok(vec![])`. Kept as the pass `transfer`'s pre-spend hook and the background loop call; an unreadable carrier set is still an `Err`, never a quiet empty pass |
| `deadline_safety_due` | `(margin_blocks: u32) -> (Vec<RefreshResult>, Vec<String>)` | the unconditional half of maintenance, still scheduled by `maintenance_plan` on every tick — and it, too, **has no laddered subject**: both of its remedies (the re-anchor above, then a sever from `F`) are applied to coins near a `locktime`, and no coin has one. On a laddered wallet it returns `(vec![], vec![])`. Nothing on a coin matures on its own, so there is no deadline for this pass to beat |
| `sever_from_f` | `(statechain_id: &str) -> Vec<ExitStatus>` | broadcast the already-co-signed trigger. `T` carries `lock_time 0` and no relative timelock and spends `F` directly, so it pre-empts every other spend of `F` — which, with no flat backup anywhere, means the retained copies of the same `T` in past owners' hands; from the moment it confirms, every historical key share for this coin authorises a spend of an output that no longer exists. Costs the coin its off-chain life. Mechanically `unilateral_exit` on one coin |
| `detrigger_to_owner` | `(statechain_id: &str, to_address: Option<String>) -> String` | answer a griefer's confirmed `T` with a fresh spend of `T`'s payload at zero CSV wait, paying an address you name (this wallet's own backup address by default). Returns the de-trigger txid. This is an **EXIT**, not a re-anchor: there is no fresh `F′` and no rebuilt `T′/X′_0/S′_0`, so getting back off-chain is a fresh deposit (`sdk89`) |
| `colored_reanchor` | `(statechain_id: &str) -> String` | the coloured variant, where the de-trigger carries a valid RGB transition so the allocation lands on its payload output and the coin CAN be re-laddered from the new outpoint. It **refuses a plain ladder by name** ("use `refresh`"), so it is *not* a remedy for `PlainLadderOverCarrier` — that state has none, and the SDK does not claim one. A **manual call**: nothing schedules it |
| `maintenance_plan` | **sync, free fn** `(&SdkConfig) -> Vec<MaintenancePass>` | the passes a background tick runs, as a value. `MaintenancePass::DeadlineSafety` is unconditional — not gated on `auto_refresh`, `background_auto_refresh`, or anything else |

### Tokens (issuer)

Where `colored_ladder` is on, an issued carrier is laddered coloured in `claim()`'s establish pass
like any other coin — an issuance books the allocation at broadcast, so the carrier is colourable
from its first mempool sighting. Where it is off — a network with no enclave to pin — or where the
carrier sits below the coloured floor, the carrier has **no exit material** until a later pass
colours it (`LadderSkipReason::RgbCarrier`): it is never plain-laddered, never plain-exited, and
it cannot be conveyed. The legacy RGB-aware branch split/combine lane it used to ride is retired —
`register_split_subcoins_n` and `register_combine_subcoins` refuse by name, so the migration hatch
no longer mints anything. A carrier is never plain-split, structurally: the plain off-chain split is
deleted.

| Method | Signature | Notes |
|---|---|---|
| `get_token_funding_address` | `() -> String` | fund the RGB engine before issuing |
| `get_token_l1_address` | `() -> String` | alias of `get_token_funding_address` |
| `issue_token` | `(ticker: &str, name: &str, precision: u8, supply: u64) -> String` | RGB NIA onto a fresh statechain coin of `TOKEN_CARRIER_SATS`; returns the `rgb:…` asset id |
| `issue_token_sized` | `(ticker, name, precision, supply, carrier_sats: u64) -> String` | the same with the carrier's sats chosen by the caller. Reach for `issue_token` to issue a token; this is the knob for reproducing what is already in circulation. A carrier funded below the coloured root floor (`tesr::colored_ladder_floor` = `3 · (colored_committed_fee(1, rate) + P2A) + dust` = **2 562 sat** at the shipped 3.0 sat/vB) cannot be laddered and therefore has **no exit material** (`RgbCarrier`); the migration hatch's split lane is retired on both settings of `colored_ladder`, so nothing serves such a coin until it is re-funded above the floor |
| `issue_inflatable_token` | `(ticker, name, precision, supply, inflation_amounts: Vec<u64>) -> String` | IFA issuance with reserved inflation rights |
| `issue_inflatable_token_sized` | `(ticker, name, precision, supply, inflation_amounts, carrier_sats) -> String` | the IFA sibling of `issue_token_sized` |
| `mint_tokens` | `(asset_id: &str, inflation_amounts: Vec<u64>) -> (String, u64)` | realize reserved inflation rights as new supply; **broadcasts on chain** and waits for the minted allocation to settle. Returns `(inflate_txid, minted_total)` |
| `mint_tokens_sized` | `(asset_id, inflation_amounts, carrier_sats) -> (String, u64)` | as above with the carrier size chosen |
| `burn_tokens` | `(asset_id: &str, amount: u64) -> String` | burn the engine-held (free) balance, on chain. Statechain-bound supply must be exited into the engine first. Returns the burn txid |
| `query_token_transactions` | `(asset_id: &str) -> Vec<TokenTx>` | RGB-engine transfer history |
| `validate_pending_token` | `(consignment_env: &str, branch_txs: &[String], funding_txid: &str, funding_vout: u32) -> (String, u64)` | validate an un-claimed transfer's consignment WITHOUT booking it, returning `(contract_id, amount cryptographically assigned to the witness outpoint)`. The pre-payment gate an SSP runs before paying a Lightning invoice: the envelope's advisory amount is attacker-controlled, so only this consignment-derived figure is trustworthy |
| `validate_pending_token_ex` | `(consignment_env, branch_txs, child_witness_txids: &[String], funding_txid, funding_vout) -> (String, u64)` | as above for a coloured CHILD, whose witnesses are its own txid chain rather than an exit branch. A non-empty `child_witness_txids` REPLACES the branch (`branch_witness_txids(branch_txs)` is used only when it is empty) |

**Where those arguments come from, and what changed.** A paying party reads them off
`mercuryrustlib::transfer_receiver::peek_pending_transfers`, whose `PendingTransferInfo` now derives
the RGB material **from the conveyed bundle** rather than from a backup row. It used to read
`rgb_consignment` off `transfer_msg.branch_txs`' rows; no conveyance writes one any more, so it
reported `rgb_consignment: None` for every laddered hop and an SSP refused every RGB invoice. It now
takes the coloured ROOT ladder's — or the coloured CHILD's — LEAF consignment, wrapped as the SDK's
`{"c","a","s"}` envelope, and adds three fields: `rgb_assignment_txid` / `rgb_assignment_vout` (the
receiver's own final-state payload output — the outpoint the claim path books) and
`child_witness_txids` (a root ladder's `ladder_txids()`, a child's `colored_child_txids()`). A bundle
that will not parse, or a plain one, yields nothing, and the paying party refuses on that.
`SspService::execute_pay` validates against this material and refuses only when NEITHER chain is
present. **Pending run:** `sdk37` is the flow that measures it.

### Coloured-lane diagnostics

Every one of these probes the RGB **stock** through the fork's off-chain resolver with the coin's own
txid list; none of them reads `get_asset_balance` or `list_unspents`, both of which report a full
settled spendable balance over a stock at zero and would never fire an alarm.

All but `probe_carrier_funding` need the coin to hold a coloured ladder, so they are reachable
wherever `colored_ladder` is on — which is wherever an attestation identity is pinned.
`probe_carrier_funding` is the sibling for a carrier that has none: a carrier on a network still
waiting for its enclave, or one below the coloured floor — a coin with no exit material until a
later pass colours it.

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
| `probe_carrier_funding` | `(statechain_id: &str, asset_id: &str, amount: u64) -> ()` | the same at the confirmed funding output of a carrier that has NO ladder (`RgbCarrier`), and therefore no tip to probe. The prevout is read from the chain, not from the coin record |
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
| `withdraw` | `(to_address: &str, statechain_ids: Option<Vec<String>>, fee_rate: Option<f64>) -> Vec<String>` | cooperative, SE-co-signed, no timelock wait. **It reads no backup rows at all** — it used to refuse "No backup transaction associated with this statechain ID", which after the rule would have been every coin — and the withdrawal transaction's locktime comes from the current tip alone (`calculate_block_height` with `is_withdrawal`; the builder's backup-count argument is passed as 0). Where several rows share one statechain id it prefers the **LIVE** row (`CONFIRMED` or `IN_TRANSFER`) rather than the lowest locktime, since no coin has one. Refuses RGB carriers, hard-erroring when one is named explicitly. A received child or a spine tip has no confirmed outpoint to spend, so it is routed to `unilateral_exit` and marked `WITHDRAWING`. (A legacy `branch-` sub-coin, if the wallet still holds one, has its branch materialized first) |
| `unilateral_exit` | `(statechain_ids: Option<Vec<String>>, to_address: Option<String>) -> Vec<ExitStatus>` | no SE needed. Walks the tier chain as each relative CSV matures — idempotent and incremental, so call once per block until `complete`; `wait_blocks` is the remaining maturity of the next tier (`sdk50`). Its liveness rule is `is_live_for_defence`: a coin is walkable while it is `IN_MEMPOOL`, `UNCONFIRMED` or `CONFIRMED` — a ladder is defended and exitable from the block its deposit is first seen in — and every other status is refused. Refuses a carrier unless its ladder is coloured, and refuses **by name** a coin with no ladder row: there is no flat exit fallback, because there is no flat backup. `to_address` is accepted but unused — every path pays the coin's own pre-signed payee, a seed-derived address of this wallet |
| `materialise_carrier` | `(statechain_id: &str) -> bool` | settle an un-colourable carrier's ALLOCATION on chain by broadcasting the `branch-` rows it still holds from the retired coloured split/combine lane. **Not an exit**: the sats stay on the live 2-of-2 outpoint and still need the SE. Refuses any carrier that has, or could still be given, a coloured ladder. A carrier with no such rows — every carrier minted since the lane was retired — has nothing to materialize and no SE-free move at all; returns whether a branch was broadcast, the settlement verified against the chain before returning. **UNPROVEN end to end:** `sdk78`, its only E2E, was DELETED with the retired lane, so nothing exercises this call today |
| `defend_ladders` | `() -> Vec<String>` | owner-run ladder watchtower pass over every live coin (`is_live_for_defence`: `IN_MEMPOOL` \| `UNCONFIRMED` \| `CONFIRMED`), so a ladder is defended from the block its deposit is first seen in. A no-op while `F` is unspent — an idle ladder has nothing to defend. If someone triggers the coin, this races the owner's tiers; the adopted current state carries the strictly-lowest CSV, so it matures first and pays the owner. Idempotent; call once per block. Emits `LadderDefended` (`sdk51`) |
| `auto_exit_due` | `(margin_blocks: u32) -> Vec<String>` | the height-keyed near-deadline pass, and under the rule it has a **legacy subject only**: a coin still carrying `branch-` rows from the retired coloured split/combine lane. Such a coin is force-exited (`ExitDeadlineApproaching`) or, for a received carrier, **materialized** branch-only (`TokenCarrierMaterialized`) when `tip + margin` reaches its deposit-anchored `exit_deadline_block`; the gate is a VERIFIED non-empty branch read, so a coin with no branch is skipped for having no deadline rather than for an unreadable one. **The leaf near-deadline loop is deleted**: an adopted child (`ctesr-`) or spine tip (`spinetip-`) has no height deadline — its parent is a laddered coin with no flat backup, so no ancestor holds a matured spend of `F` — and its only exposure is the parent's trigger being broadcast, an EVENT the per-block `defend_ladders` child and tip loops already answer. Run by the background watcher each poll when `auto_exit`; on a wallet with no legacy rows it finds nothing |
| `export_watch_bundle` | `() -> String` | **keyless** watch bundle (JSON `WatchBundle`) over every live coin (`is_live_for_defence`). **Every** entry it can now emit — root, adopted child, spine tip — is **event-driven**: `deadline_block: u32::MAX` (the height predicate permanently false), `backup_tx: None`, `backup_locktime: None`, and a `WatchTrigger` on `F` whose `push_txs` are the owner's own pre-signed tiers. **No entry carries a `backup_tx` or a finite deadline at all any more**: the height-driven arm is DELETED (it was unreachable — no coin has an `exit_deadline_block` — and it opened by demanding a backup row, which failed the whole export). A coin with no ladder, a legacy `branch-`-only coin included, is therefore **omitted** and reported by `flat_only_coins` instead. No mnemonic, no key shares, no RGB seed — safe to hand to untrusted watchtowers. Still fails closed rather than silently omitting an entry it COULD build (an unreadable leaf row, or a blind deadline, aborts the export). Re-export after any transfer, claim, split or re-anchor (`sdk45`) |
| `estimate_exit_cost` | `(statechain_id: &str) -> ExitCostEstimate` | for a laddered coin: `backup_vbytes` is the signed vsize of the tier walk (`TesrBundle::exit_tiers`), `wait_blocks` is **0** (an idle ladder has nothing maturing; the walk's latency is `config::tesr_exit_wait_blocks`), `branch_txs`/`branch_vbytes` are 0, and `exit_deadline_block` / `exit_deadline_blind` are both `None`. A non-zero `branch_txs` or a `Some` deadline appears only for a legacy `branch-` coin — see the note below |
| `get_withdrawal_fee_quote` | `(statechain_ids: Option<Vec<String>>) -> WithdrawalFeeQuote` | cooperative-withdrawal fee quote at the current electrum-estimated rate, ~111 vB per coin |
| `watchtower_faults` | `() -> Vec<WatchtowerFault>` | every deadline-critical pass that is currently BLIND, with `consecutive_failures`, `since_unix`, `last_unix`. Poll it next to `get_balance` and alert on a non-empty result: while it is non-empty, nothing is racing a clawback or a hostile trigger on this wallet's behalf |
| `is_watchtower_blind` | `() -> bool` | convenience over the above |
| `fee_float_solvency` | `() -> Option<mercuryrustlib::tower_float::Solvency>` | can the configured fee float cover every coin this wallet defends, in BOTH units? A float with plenty of sats in ONE utxo funds exactly one simultaneous rescue, because a v3 fee child may have only one unconfirmed ancestor. `Ok(None)` when no `fee_bump` is configured — a keyless wallet is out of scope, not underfunded |

**"No deadline" and "I could not compute a deadline" are different answers.**
`exit_deadline_block == None` with `exit_deadline_blind == None` is the answer for **every laddered
coin**: it means "laddered, event-driven" — nothing on the coin matures on its own, and the race, if
one ever starts, starts when somebody spends `F`, which the watch bundle's `WatchTrigger` covers.
`exit_deadline_blind == Some(reason)` means the coin HAS a legacy `branch-` chain, a deposit-anchored
deadline therefore exists for it, and it could not be computed. Anything deadline-critical must
branch on `ExitCostEstimate::deadline_is_unknown()`, not on the `Option`. `auto_exit_due` routes a
blind coin into `WalletEvent::WatchtowerBlind` plus a retained `WatchtowerFault` and returns `Err`;
it deliberately does NOT force-materialize on suspicion, because a single backend blip would
otherwise dump every legacy off-chain coin in the wallet on chain.

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
| `LadderEstablished` | `{statechain_id}` — the TES-R ladder was established on a root coin: at first sight of `F` in `claim()`'s establish pass (or in the deposit watcher itself under `LadderAtSight::Plain`) |
| `LadderSkipped` | `{statechain_id, reason}` — a coin was left without a ladder, i.e. without exit material. Emitted only when the recorded reason CHANGES, so read it back with `ladder_skip_reason` / `flat_only_coins` rather than relying on having been subscribed |
| `LadderDefended` | `{statechain_id, tiers_broadcast}` — the coin was found triggered and `defend_ladders` broadcast tier tx(s) this pass; emitted per pass until the exit completes |
| `ExitBranchConflict` | `{statechain_id}` — a competing tx is spending the branch root of a legacy `branch-` coin; fee-bump or re-attempt, and do **not** assume the coin exited |
| `ExitDeadlineApproaching` | `{statechain_id, deadline_block, tip}` — legacy `branch-` coins only: `auto_exit_due` is force-exiting one against its deposit-anchored deadline. A laddered coin never emits it |
| `LeafExitForced` | `{statechain_id, deadline_block, tip}` — **no longer emitted.** The variant survives on the enum so a subscriber's match arms keep compiling, but its producer — the leaf near-deadline loop of `auto_exit_due` — is deleted: a leaf has no height deadline to beat |
| `TokenCarrierMaterialized` | `{statechain_id, deadline_block, tip}` — a received carrier still carrying legacy `branch-` rows was settled on chain, branch-only, against its deposit-anchored deadline |
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
| `auto_refresh` | `true` | run the pre-spend re-anchor hook (`auto_refresh_due`) inside `transfer`/`transfer_many`. On a laddered wallet the hook finds nothing due — no coin carries a `locktime` — so the flag is inert today; kept so the cost of a future on-demand re-anchor appears as a payment fee instead of a balance shrinking in the background |
| `auto_refresh_margin_blocks` | `144` | the margin `auto_refresh_due` and `deadline_safety_due` compare a coin's `locktime` headroom against. **No laddered coin has a `locktime`** (`None` for life), so it selects nothing; it survives as the argument those passes take |
| `background_auto_refresh` | `false` | also run the ROUTINE re-anchor from the background watcher. Off by default, and inert for the same reason as `auto_refresh`. It does not gate deadline safety, which stays unconditional in `maintenance_plan` |
| `auto_exit` | `true` | run `auto_exit_due` from the background watcher. Its only subject is a legacy `branch-` coin |
| `auto_exit_margin_blocks` | **derived** — 860 regtest, 2 120 mainnet | `auto_exit_margin_blocks_for(AUDIT_17_K_MAX, interval, AUTO_EXIT_MODELLED_DEPTH)` = `k_max·interval + tesr_exit_txs(d)·144`, consumed only by the legacy `branch-` loop of `auto_exit_due`. The `k_max·interval` term was the ancestor-locktime gap of the retired flat chain and now bounds nothing on a laddered coin; the second term is one confirmation window per SEQUENTIAL transaction of an exit walk. Still derived per network rather than a shared literal, because `interval` is 10 on regtest and 100 on mainnet |
| `fee_bump` | `None` | `FeeBumpConfig{core_rpc_url, core_rpc_user, core_rpc_password, funding_secret_key_hex, target_fee_rate, reserve_bumps_per_coin}`. `None` means the wallet **cannot** bump, which is the honest default: a tier refused for fee reasons is reported as a stated limit rather than retried forever at the same committed rate. Set it and `unilateral_exit` / `defend_ladders` escalate a refused tier to a 1P1C package. The key funds FEES ONLY and is never a coin key |
| `colored_ladder` | **derived** — `true` regtest, `false` mainnet | build a COLOURED TES-R ladder over an RGB carrier in `claim()`'s establish pass instead of leaving it without exit material (`RgbCarrier`). Not a stated bool: both constructors READ `TesrParams::attestation_identity_const` for the network, because the coloured lane cannot establish a ladder without a pinned attestation identity and true-without-a-pin ships a wallet whose token lane refuses forever. Mainnet is `false` only because no mainnet enclave is provisioned yet; pinning one flips it — and with the flat lane gone there is **no fallback** on an unpinned network: a carrier there has no exit material and cannot be conveyed. Cost figures for the lane it switches on: [PARTIAL-PAYMENT-ECONOMICS.md](../spec/PARTIAL-PAYMENT-ECONOMICS.md) |
| `attestation_identity` | `None` | the enclave attestation identity this wallet verifies sig-count attestations against. Resolution is **compiled-in pin → this field → REFUSE**: a value that disagrees with a compiled-in pin is an error, not an override. A pin exists **only for regtest**: `attestation_identity_const` returns `None` for mainnet/bitcoin AND for testnet, testnet3, testnet4 and signet. With no pin and no configured value, the SDK `claim()` establish pass's `get_statechain_info` call fails, the pass records `LadderSkipReason::AttestationIdentityUnpinned` and ladders NOTHING. **The consequence is not "receiving is gated":** on an unpinned network an SDK wallet's deposit IS booked and has **no exit material** — it can neither be conveyed nor unilaterally exited, and cooperative withdrawal is the only route out. The flat backup used to supply that unilateral exit without any attestation; it no longer exists. (Mainnet has no enclave provisioned at all, so this is a not-yet-deployable state rather than a live regression.) Note the lane: `tesr::establish_auto` / `cosign_tier` never call `get_statechain_info`, so `check_deposit` under `LadderAtSight::Plain` ladders unpinned — the refusal is specific to the SDK `claim()` pass. Falls back to the `UTEXO_ATTESTATION_IDENTITY` environment variable. Read the value from the enclave's `GET /attestation_identity` |

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
  `deadline_is_unknown()`. For a laddered coin `backup_vbytes` is the tier walk's signed vsize,
  `wait_blocks` is `0`, and both deadline fields are `None` ("laddered, event-driven"); the
  `branch_*` fields and a `Some` deadline belong to legacy `branch-` coins only.
  `ExitStatus{statechain_id, complete, wait_blocks}` — `wait_blocks` is the remaining relative
  maturity of the next tier in the walk, never an absolute height;
  `WithdrawalFeeQuote{n_coins, est_vbytes, fee_rate_sat_vb, fee_sats}`
- `WatchBundle{version, wallet_name, entries}` /
  `WatchEntry{statechain_id, token_carrier, deadline_block, branch_txs, backup_tx?,
  backup_locktime?, trigger?}` / `WatchTrigger{watch_txid, watch_vout, csv_blocks, push_txs}`. Every
  entry the exporter can emit carries `deadline_block: u32::MAX`, `backup_tx: None`,
  `backup_locktime: None` and a `trigger` on `F`. The optional fields stay on the wire format so an
  older bundle still parses; **nothing in this tree writes them any more**, and a coin that would
  have needed them (no ladder, legacy `branch-` rows only) is omitted from the bundle instead
- `WatchtowerFault{pass, detail, consecutive_failures, since_unix, last_unix}`,
  `WatchtowerPass::{AutoExit, DefendLadders}`, `LadderSkipReason` (fourteen variants, with
  `as_str()`, `from_str()` and `permits_flat_conveyance()` — which now answers **`false` for every
  variant**: there is no flat lane, and a recorded reason is diagnostic, never a licence)
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
| `config::auto_exit_margin_blocks_for(k_max, interval, d) -> u32` | the derivation behind `auto_exit_margin_blocks`: `k_max·interval + tesr_exit_txs(d)·BLOCKS_PER_DAY`. Consumed only by the legacy `branch-` loop of `auto_exit_due` |
| `config::{SE_INTERVAL_DEPLOYED, SE_INTERVAL_DEFAULT}` | the regtest (10) and mainnet (100) SE `interval`, read at compile time from `TesrParams::flat_ladder_params_const`. **Compatibility constants**: `interval` was the per-hop decrement of the retired flat backup chain and is applied to nothing on a laddered coin; `initlock`, from the same table, survives as the FIXED exit window the split-depth cap (`enforce_split_depth_cap`) measures a leaf's exit walk against |
| `config::{BLOCKS_PER_DAY, AUDIT_17_K_MAX, AUTO_EXIT_MODELLED_DEPTH}` | the remaining terms, and they are chosen rather than derived: 144, 14 and 1. `AUDIT_17_K_MAX` was the **assumption** in the margin — the pre-split hops the retired flat chain's deposit-anchored deadline over-estimated by — and bounds nothing on a laddered coin |

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
