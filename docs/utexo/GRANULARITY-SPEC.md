# Coin Granularity (Partial Amounts) — Normative Specification

This document is the authoritative reference for **coin granularity**: how the Utexo SDK
(Spark-compatible API) pays, splits, and books *partial amounts* — sats below a coin's size and token amounts below a
carrier's allocation. Requirements are numbered **GRN-REQ-n**, invariants **GRN-INV-n**, error
semantics **GRN-ERR-n**; MUST / MUST NOT / SHOULD / MAY are RFC-2119. Items overlapping the
system spec cite [SPEC.md](SPEC.md) (REQ-n / INV-n / ERR-n) and the invalidation spec
[INVALIDATION-SPEC.md](INVALIDATION-SPEC.md) (IVL-*) — their content is cross-referenced, never
restated. Long-form explainer of this spec:
[learn/granularity-deep-dive.md](learn/granularity-deep-dive.md); further explainers:
[learn/transfers.md](learn/transfers.md) (the amount-maker), [learn/tokens.md](learn/tokens.md),
[learn/invalidation-deep-dive.md](learn/invalidation-deep-dive.md) §3b/§4,
[learn/exits.md](learn/exits.md); pricing:
[research/granularity-economics.md](research/granularity-economics.md) (which extends
[research/invalidation-economics.md](research/invalidation-economics.md) §5); audit trail:
[AUDIT-2026-07.md](AUDIT-2026-07.md).

## 0. Scope, terminology, relationship to the other specs

**Scope.** The amount model (sats and RGB raw units), payment planning, the off-chain split
primitive (plain and colored), receiver obligations specific to partial amounts, the token-carrier
lifecycle, granularity's effect on invalidation and exit, and the error surface. The invalidation
machinery itself (ladders, budgets, branches, deadlines) is IVL-* territory; token consignment
validation mechanics are SPEC §7.

**One protocol, two coin SHAPES.** There is ONE protocol. `claim()` establishes a TES-R exit ladder
(trigger `T` → extension `X_m` → state `S`, relative CSV, all un-broadcast) for every fresh
CONFIRMED ROOT coin, unconditionally (wallet.rs:451-520); there is no protocol-version switch and no
"legacy" lane. Granularity therefore has two admission regimes, both current:

- **LADDERED** — every plain deposit. A partial payment is an **IN-LADDER split**: the split state
  `SP` spends `X_m.out[0]`, so it DESCENDS from the trigger instead of racing it for the funding
  outpoint `F` [B1]. It pays a piece child (conveyed straight to the recipient, Model A) and a
  change child (`in_ladder_pay`, transfer.rs:783); a received child re-splits at its own level via
  `child_in_ladder_pay` (transfer.rs:677, a depth-2 `ancestors` chain). Its fee model is the TIER
  model, not §3's backup model: the children share `tier_out_total(prev, n, rate)` (parent value
  minus the committed tier fee and the P2A anchor) and each child must clear
  `min_child_value(rate, dust) = 2·(committed_fee + P2A) + dust` — **1306 sats at the default
  2 sat/vB**, because a child funds its OWN extension and state tiers before clearing dust
  (lib/src/tesr.rs:60-77, 340-348).
- **UN-LADDERED** — an RGB **carrier** is deliberately never laddered (a plain tier spend would
  destroy the allocation — terminal freeze, PROTOCOL.md §5.10 rule 1; wallet.rs:459-470, sdk52), and a
  split sub-coin cannot be laddered either: its funding `F` is an un-broadcast split output, so a
  trigger would have no prevout to spend [B0] (wallet.rs:481-503). These coins keep the signed-once
  backup chain and transfer by backup-chain handover. This shape is LOAD-BEARING — §3's plain split
  primitive and the whole of §4 (colored splits) live here, not in dead code.

Consequently `split_coin` (§3) hard-refuses a LADDERED parent (transfer.rs:556-561, [B1]) as well as
a carrier (transfer.rs:534-538), and `transfer()` routes a `WithSplit` plan to `child_in_ladder_pay`
/ `in_ladder_pay` / `split_coin` according to the selected parent's shape (transfer.rs:139-170).

| Term | Meaning |
|---|---|
| **piece** | The sub-coin minted by a split carrying the *exact* requested amount (sats, or token raw units). |
| **change** | The sub-coin carrying the residual: `parent − piece − fee_reserve` sats, and for colored splits the residual token allocation. |
| **carrier** | A statechain coin whose UTXO currently holds an RGB token allocation. Its sats are packaging; the allocation is the payload (clients/libs/rust-sdk/src/tokens.rs:364-385). |
| **colored split** | A split whose tx is RGB-colored: each output may be assigned part of the carrier's allocation; rgb-lib adds one OP_RETURN opret commitment (clients/libs/rust/src/rgb.rs:201-298). |
| **consignment envelope** | `ConsignmentEnvelope{c, a, s}` in `BackupTx.rgb_consignment`: base64 consignment, advisory amount hint, piece sats (tokens.rs:45-54). |
| **raw units** | The `u64` integers RGB contracts account in. All protocol amounts are raw units. |
| **precision** | A `u8` in the contract's metadata declaring how UIs *display* raw units. Never touched by the protocol (§1.2). |

**Relationship.** SPEC.md defines the system; INVALIDATION-SPEC.md defines how consumed state is
invalidated. This document refines their granularity cluster — REQ-15/17/18/21/22,
INV-9/10/11/13/22/26, ERR-8/9 and IVL-REQ-7/10-12, IVL-INV-5/13/14 — into exact admission rules,
boundaries and lifecycle semantics. Where documents disagree, SPEC.md / INVALIDATION-SPEC.md win
for their own items; this document wins for GRN-* items.

## 1. The amount model

### 1.1 Sats

- **GRN-REQ-1 (width)** The SDK API takes sat amounts as `u64`; each coin's amount is *booked* as
  `u32` (cap 4,294,967,295 sats ≈ 42.9 BTC — SPEC §14 "Amount width"). On the split path an
  over-cap output MUST error, not truncate: every registered sub-coin amount goes through
  `u32::try_from` (clients/libs/rust-sdk/src/transfer.rs:1076, `register_split_subcoins_n`). Other paths cast unchecked
  (SPEC §14); the cap is an accepted limitation, out of intended per-coin range.
- **Resolution.** Above the dust floor, sats granularity is exact to 1 sat (SPEC INV-22): any
  piece in the GRN-INV-2 domain is mintable at 1-sat steps.

### 1.2 RGB raw units and precision

- **GRN-REQ-2 (raw units only)** Every token amount in the protocol — issuance supply, transfer
  amount, envelope hint, booked balance — is RAW `u64` units. The SDK MUST NOT scale any amount
  by `precision`: `transfer_tokens(asset_id, addr, token_amount)` passes `token_amount` straight
  into the colored split (tokens.rs:392-402, 531-534). `precision` is contract METADATA set at
  issuance (`issue_nia(ticker, name, precision, amounts)`,
  clients/libs/rust-rgb/src/lib.rs:146-151) and reported back with balances (tokens.rs:347-361);
  it is display advice, immutable post-issuance.
- **Consequence.** "0.1 token" exists **iff** `precision >= 1`, and equals `10^(precision−1)` raw
  units. The minimum transferable token amount is **1 raw unit**, always, at any precision.
  A precision-0 asset is indivisible below whole units by construction, not by any SDK check.

### 1.3 The SE is amount-blind

- **GRN-REQ-3 (zero trust surface)** No amount — sats or token — MUST ever reach the SE on any
  granularity path. The SE blind-co-signs a MuSig2 challenge over a tx it never sees
  (SPEC §1 "never sees amounts/addresses"; `sign_split_tx` sends only nonce/challenge material,
  transfer.rs:1357-1414, and the colored variant likewise, rgb.rs:219-292); transfer messages are
  owner-encrypted and never deserialized by the SE (SPEC §3). Granularity is therefore enforced
  entirely client-side — the sender constructs, the receiver independently validates (§5) — and
  adds **zero** trust-model cost over whole-coin transfers.

## 2. Payment planning (sats)

`transfer(address, amount)` plans over confirmed, non-carrier coins via `select::plan`
(clients/libs/rust-sdk/src/select.rs:89-133 `plan_with_floor`, a 2-line wrapper `plan` at 81-83;
transfer.rs:48-200).

- **GRN-REQ-4 (plan semantics — refines SPEC REQ-15/INV-9)** `plan(coins, target)` MUST return
  exactly one of:
  1. `Exact(subset)` — a subset summing to `target` exactly (dynamic-programming subset search,
     select.rs:38-79). The SDK then performs **N whole-coin key handovers**, one transfer message
     per coin (transfer.rs:174-232). No split occurs.
  2. `WithSplit{whole, split, split_amount}` — whole coins are handed over and ONE coin is split
     to mint the exact remainder piece (transfer.rs:126-172).
  3. `Insufficient{available}` — surfaced as `SdkError::InsufficientBalance` (GRN-ERR-1 = ERR-9).

  *Note (the split lane follows the parent's SHAPE — §0):* on a `WithSplit` plan `transfer()`
  dispatches on the selected parent (transfer.rs:139-170): a received in-ladder CHILD →
  `child_in_ladder_pay`, a LADDERED root coin → `in_ladder_pay`, an UN-LADDERED coin (a split
  sub-coin, or a coin whose ladder was never established) → `split_coin` (§3). On both in-ladder
  routes the piece child is CONVEYED to the recipient inside the split itself (with the standard SE
  key handover, completed by the receiver at claim) rather than handed over in the loop below, and
  the change child stays with the sender; the returned `TransferResult` still books it as sent.
  Live proof: `sdk01` (end-to-end non-exact payment through `transfer()`), `sdk59` (root in-ladder
  payment: Alice pays Bob, Bob claims and exits the child), `sdk60` (alice→bob→carol whole-child
  re-transfer, funding outpoint unspent throughout), `sdk17` (partial second hop — a received child
  split again), `sdk29` (the un-laddered lane: a plain `split_coin` of a freed sub-coin).

  *Note (multi-coin plans are not atomic):* the SDK executes a multi-coin plan as a SEQUENCE of
  per-coin key handovers (a `?`-aborting loop, transfer.rs:174-232): a failure partway leaves a
  partial payment — coins already handed over belong to the receiver, the rest stay with the
  sender. This is the same non-atomicity SPEC §14 "Batch atomicity" documents for
  `transfer_many`/`batch_transfer_tokens`; there is no all-or-nothing guarantee across coins on
  any multi-coin path.
- **GRN-REQ-5 (split-candidate admission — audit [29])** The split candidate MUST satisfy
  `remaining >= min_output` AND `candidate_sats > remaining + fee_reserve(candidate_sats) +
  min_output` (`plan_with_floor`, select.rs:114-127) — i.e. the planner only picks a coin the split
  executor (§3) will accept, leaving a viable change. `min_output` is the backup-fee floor
  `min_split_output(fee_rate)` of GRN-INV-1b (442 sats at 1 sat/vB), which `transfer()` passes in
  (transfer.rs:97); the bare `330` below is the dust special case of it. A remainder below the floor
  MUST yield `Insufficient` **even when the wallet balance covers the target** (unit
  `sub_dust_remainder_is_refused`, select.rs:178): a sub-dust piece would make the split tx
  unbroadcastable (GRN-INV-1).

  *Note (planner conservatism — the planner is incomplete and 1 sat stricter than the executor):*
  `plan()` is greedy largest-first with NO backtracking (select.rs:89-112), so `Insufficient` does
  NOT imply that no payable composition exists — coins {1,000; 970}, target 1,300 is refused
  (`Insufficient{available: 1970}`) though payable by hand (economics §2 "greedy shadow"; the
  workaround is a manual `split_coin` + `transfer`). And because the admission filter is strictly
  `candidate > remaining + reserve + min_output` (select.rs:123-127) while the executor admits equality
  (GRN-INV-1), the planner is one sat more conservative at every boundary: for a 330-sat
  remainder it first accepts a **961**-sat candidate, while a direct `split_coin` admits the
  960-sat parent — in the safe direction (audit [29]: the planner never picks a parent the
  executor then rejects; executor side pinned by `granularity_model.rs`, planner side by
  `invalidation_model::split_size_floor`).
- **GRN-REQ-6 (receiver-of-N consequence)** On the `Exact` path the receiver ends up with N
  independent coins, each with its own ladder, claim validation and future exit cost — the SDK
  MUST NOT merge them (plain-BTC combine is not a shipped SDK operation,
  INVALIDATION-SPEC §3 "Combine status"). Payers SHOULD prefer `Exact` plans for fee efficiency
  (no split), accepting the receiver-side coin count.

## 3. The split primitive (sats)

`split_coin(id, piece_sats)` mints `{piece, change}` from one confirmed **un-laddered** coin in one
SE-co-signed, un-broadcast tx (transfer.rs:520-660). It hard-refuses a laddered parent
(transfer.rs:556-561: a prior owner's no-timelock trigger over `F` could void the split [B1]) and a
carrier (GRN-REQ-10) — the laddered lane's partial payment is the in-ladder split (§0), whose
admission floor is `min_child_value`, not the backup floor of GRN-INV-1b.

- **GRN-INV-1 (admission rule — refines SPEC INV-10)** With
  `fee_reserve = clamp(parent/100, 300, 2000)` sats (`split_fee_reserve`, transfer.rs:1416-1424),
  a split of `piece` out of `parent` is admissible **iff** (`split_amounts`, transfer.rs:1477-1480 —
  a thin wrapper over `split_amounts_floored`, transfer.rs:1449-1476):
  1. `piece + fee_reserve < parent` (strict; `>=` errors GRN-ERR-3), and
  2. `piece >= 330` and `change = parent − piece − fee_reserve >= 330`
     (`DUST_LIMIT = 330`, transfer.rs:1425; audit [9]).

  **Exact boundary.** The dust floor on `change` binds first: `parent >= piece + reserve + 330`.
  The minimum splittable parent is therefore **960 sats exactly** — at `parent = 960`
  (`reserve = clamp(9,300,2000) = 300`) the only admissible piece is 330, giving
  `change = 960 − 330 − 300 = 330`, which passes both checks; at `parent = 959` the same piece
  yields `change = 329` and is refused (GRN-ERR-4). The fit check (1) is strict but non-binding
  at this boundary (330 + 300 = 630 < 960). The planner (GRN-REQ-5) is one sat stricter: it first
  accepts a 961-sat candidate for a 330-sat remainder (see the GRN-REQ-5 conservatism note).

  **GRN-INV-1b (backup-fee floor — the true minimum *mintable* piece).** The 330-sat dust floor
  above guards the split-tx *output*; it is necessary but **not** sufficient for a usable
  sub-coin. Each sub-coin also needs a valid backup tx: `create_tx1` (deposit.rs:46) selects
  `fee_rate = min(SE quote, max_fee_rate)` and the dust/fee rejection happens in `create_tx_out`,
  which sweeps
  `sub_coin_sats − ceil(BACKUP_TX_SIZE·fee_rate)` and rejects it below the dust floor
  (`MercuryError::FeeTooLow`, lib/src/transaction.rs:116-132; FeeTooLow at 129/131;
  `BACKUP_TX_SIZE = 112` vB). So the minimum **mintable** piece (and change)
  is `min_split_output(fee_rate) = 330 + ceil(112·fee_rate)` — **442 sats at 1 sat/vB**
  (transfer.rs:1437; pinned by unit
  `granularity_model::backup_fee_floor_is_the_true_mintable_minimum`, and minted live by `sdk29`,
  which splits a freed sub-coin at exactly `330 + ceil(112·r)`), rising with feerate. A 330-sat
  piece is a valid split output whose *backup* is un-broadcastable, so it cannot be minted into a
  usable coin.
  **Enforced (FIXED):** every split path validates EVERY output against `min_split_output(fee_rate)`
  *before* the parent is made terminal — `split_coin` via `split_amounts_floored` and `transfer_many`
  via its own per-recipient guard (transfer.rs), the colored `transfer_tokens`/`batch_transfer_tokens`
  paths (tokens.rs), and the planner via `select::plan_with_floor`. On the LADDERED lane the larger
  GRN-INV-1c floor binds instead (limitation 13). A piece in `[330, min_split_output)` is refused with the
  parent untouched. Previously the guard checked only the 330 dust floor, so such a piece passed
  admission, the parent was made terminal, and only *then* did the backup fail (`FeeTooLow`) —
  stranding the parent to unilateral-exit-only (degradation class of the audit [15] brick; no
  theft). The bare-dust `split_amounts` remains only for the model's dust-boundary tests.

  **GRN-INV-1c (the LADDERED lane's floor — `min_child_value`).** GRN-INV-1b sizes an UN-LADDERED
  sub-coin, which needs only its own 112-vB backup. An IN-LADDER split child needs more: it gets its
  OWN headless ladder (an extension tier and a state tier hung off `SP.out[j]`), each burning
  `committed_fee(rate) + P2A_VALUE`, and its final state output must still clear dust — hence
  `min_child_value(rate, dust) = 2·(committed_fee + P2A) + dust` = **1306 sats at the default
  2 sat/vB** (lib/src/tesr.rs:60-77). Every in-ladder admission guard MUST take the LARGER of the two
  floors, `max(min_split_output(rate), min_child_value(parent.fee_rate, DUST_LIMIT))`
  (transfer.rs:704-705 child level, 819-820 root level), and MUST check it BEFORE the parent's spend
  budget is consumed. **FIXED (D1):** the guard previously used the GRN-INV-1b backup floor alone
  (442), so a child in `[442, 1306)` was admitted, the parent was terminalized, and `establish_child`
  then died with `FeeTooHigh` — stranding the parent exactly the way GRN-INV-1b's own bug did.
  Pinned by `sdk30` (which sizes a sponsored rebate at `max(fee + DUST_LIMIT, min_child_value)` and
  states the 1306 identity) and exercised by `sdk58`/`sdk59`/`sdk17`.
- **GRN-INV-2 (resolution domain)** For a given parent, every
  `piece ∈ [330, parent − fee_reserve − 330]` is admissible, at 1-sat steps (SPEC INV-22).
- **GRN-INV-3 (tx shape)** A plain split tx has locktime 0 (IVL-INV-5 = SPEC INV-4), exactly one
  input (the parent) and one output per split entry (SPEC INV-11); `get_unsigned_split_psbt`
  additionally rejects `Σ outputs >= input` and any output `< 330` with `MercuryError::FeeTooLow`
  (lib/src/transaction.rs:346-360, GRN-ERR-5).
- **GRN-REQ-7 (ordering)** `split_coin` MUST validate GRN-INV-1 *before* touching the parent
  (transfer.rs:565-572), then set the parent's spend budget to 1 immediately before co-signing
  (transfer.rs:624; SPEC REQ-18 = IVL-REQ-7). The split consumes the budget: the parent is
  terminal, permanently (§7). The in-ladder split obeys the same ordering with the GRN-INV-1c floor
  (transfer.rs:704-705, 819-820) — `sdk58` asserts the parent IS terminal after `SP` is co-signed
  and that an unlatched split CHILD stays NON-terminal so it can be re-transferred.
- **GRN-REQ-8 (transfer_many — refines SPEC REQ-27)** `transfer_many(recipients)` MUST carve all
  N recipient pieces plus one change in ONE split tx (N+1 outputs) and MUST dispatch on the parent's
  SHAPE, exactly as `transfer` does (GRN-REQ-4 note) — a plain split of a **laddered** parent is the
  [B1] shape `split_coin` hard-refuses, so `transfer_many` must never build one
  (transfer.rs:370-603):

  | parent shape | route | split tx |
  |---|---|---|
  | laddered ROOT coin | `in_ladder_pay_many` (transfer.rs:1255) | one split state `SP` over `X_m.out[0]` — N children + change + P2A |
  | received in-ladder CHILD | `child_in_ladder_pay_many` (transfer.rs:930) | one `CSP` over `ext_child.out[0]` — N grandchildren + change + P2A |
  | un-laddered coin (plain sub-coin) | the plain split | N+1 outputs, no OP_RETURN, so vout i = i |

  Because `claim()` ladders every fresh confirmed root coin unconditionally, the **in-ladder route is
  the default**; the plain split now serves only un-laddered sub-coins (whose funding outpoint no
  trigger can spend, so nothing can race it). Both in-ladder routes convey each recipient's child
  bundle through the mailbox WITH the standard key handover, so every piece is a first-class coin the
  recipient adopts at claim — and each records a `"Transfer"` history row (transfer.rs:1476), which an
  in-ladder payment otherwise lacks (it never calls `transfer_sender::execute`).

  Parent selection is therefore **shape-aware** (transfer.rs:399-476): candidates are sorted
  smallest-first as before, but each one's real capacity is computed per route —
  `tier_out_total(X_m.out[0], N+1, fee_rate)` in-ladder (the coin's value net of its already-committed
  tier fees) vs `amount > total + fee_reserve(amount) + min_split_output(fee_rate)` plain — and the
  first candidate that fits wins. Judging a laddered coin on `coin.amount` alone would admit parents
  the in-ladder route cannot fund.

  **Up-front guards (before any parent is made terminal):** `transfer_many` rejects any recipient
  amount below `min_split_output(fee_rate)` (transfer.rs:391-397), and each in-ladder route re-checks
  every output — pieces AND change — against the larger GRN-INV-1c floor
  `max(min_split_output, min_child_value(fee_rate, DUST_LIMIT))` (transfer.rs:976-987, 1323-1335),
  since every child funds its own extension + state tier before it must clear dust. Value is
  conserved by the builder: `Σ pieces + change == tier_out_total(X_m.out[0], N+1)`, so the change is
  derived, not free. Covered by `sdk69` (laddered parent, one `SP` with 2 pieces + change, the [B1]
  retained-trigger attack executed for real) and `sdk11` (route asserted, not just the amounts).
- **GRN-REQ-9 (ensure_exact_coin)** `ensure_exact_coin(sats)` MUST reuse an existing confirmed
  non-carrier coin of exactly `sats` if one exists, else split the smallest sufficient coin
  (same filter as GRN-REQ-8) to mint it (transfer.rs:466-513). Because `split_coin` refuses a
  laddered parent (§0/§3), the candidate filter also skips LADDERED coins and the call errors when
  none is left — so this amount-maker only works on the un-laddered shape.

  *Note (Lightning does NOT depend on it — D3, FIXED):* the one-call Lightning APIs used to mint via
  `ensure_exact_coin` and therefore refused every laddered coin, making them unusable on an ordinary
  wallet (whose deposits are all laddered). They now fall back to the NON-EXACT in-ladder lane on
  that error: `pay_lightning_invoice` → `pay_lightning_invoice_inladder` over the largest laddered
  coin (ssp.rs:955-972) and `create_receive` → `in_ladder_pay` with an SE-minted latch
  (ssp.rs:547-614, LIGHTNING.md §2b). Exact legs are still preferred when a coin of the exact size
  exists. Live: `sdk63`/`sdk64` (exact pay/receive on the ladder), `sdk65`/`sdk67` (non-exact
  in-ladder pay/receive), `sdk66`/`sdk68` (failure paths).
- **GRN-REQ-10 (carrier refusal)** `split_coin` MUST refuse a token carrier with a hard error
  (transfer.rs:534-538, GRN-ERR-7): a plain split spends the carrier RGB-unaware and would
  destroy the allocation (review H2 / audit [7]). Token amounts move ONLY via §4.
- *Note (deposit-token slots):* every split output is a fresh statechain slot and consumes one
  deposit token from the SE's anti-spam token server, requested automatically — `split_coin`
  takes two (transfer.rs:579-597), a colored split two (tokens.rs:494-509), `transfer_many` /
  `batch_transfer_tokens` N+1 (transfer.rs:379; tokens.rs:736-755). When the SE charges for
  tokens, the operation fails with the typed `SdkError::TokenPaymentRequired` (GRN-ERR-13)
  **before** the terminal-guard on every path, so a token failure never pins the parent.

## 4. Colored (RGB) splits

`transfer_tokens` (tokens.rs:392-673) and `batch_transfer_tokens` (tokens.rs:678-878) perform a
**colored split** of one carrier via `create_colored_split_tx` (rgb.rs:201-298).

- **GRN-INV-4 (output triple)** A colored split output is
  `SplitOutput = (address, sats, rgb_amount)` (rgb.rs:165-167). An output with `rgb_amount = 0`
  is left **uncolored** — plain sats, no zero-value allocation (rgb.rs:248-255).
- **GRN-REQ-11 (fixed piece sats)** Every token piece MUST carry exactly
  `TOKEN_PIECE_SATS = 1500` sats (tokens.rs:23 const, 559 single, 834 combine, 1051 batch). Rationale: the sats are packaging
  (comfortably above the 330 floor, with margin for the piece's own backup fee, §7); the token
  amount is the payload. The receiver's booked BTC for a token receive is therefore always
  1500 sats per piece, independent of token value. *Consequence (one-hop):* 1,500 is below the
  2,130 minimum carrier (GRN-INV-6), so a received piece can NEVER fund a further
  `transfer_tokens` — the receiver holds or exits (§11 limitation 10; economics §3).
- **GRN-INV-5 (change formulas)** For a single-recipient colored split:
  `change_sats = carrier_sats − 1500 − fee_reserve` (same reserve clamp as GRN-INV-1,
  tokens.rs:504) AND `token_change = carrier_amount − token_amount` (tokens.rs:505). When
  `token_change > 0` the change output is colored with it and registered as the residual carrier;
  when `token_change = 0` the change output is uncolored and the change coin is **plain BTC**
  (tokens.rs:606-617; §6).
- **GRN-INV-6 (minimum carrier)** Dust-only derivation: `1500 (piece) + fee_reserve (>= 300) +
  change (>= 330)` ⇒ **2130 sats**. With the backup-fee floor now enforced on the colored path
  (change and the 1500-sat piece must each clear `min_split_output(fee_rate)`), the **live minimum
  carrier is `1500 + fee_reserve + min_split_output(fee_rate)` = 2242 sats at 1 sat/vB**, refused
  UP-FRONT before the terminal-guard (tokens.rs, after the fit guard). The old layering — SDK fit
  guard admits ≥ 1801, then [1801, 2129] fail the lib dust floor on change *after* the terminal
  guard — no longer applies: the backup-fee check now rejects the whole `[1801, 2242)` band before
  the carrier is made terminal, so no carrier is pinned. Batch: min carrier =
  `1500·N + reserve + min_split_output(fee_rate)`, guarded the same way (tokens.rs).
- **GRN-INV-7 (colored tx shape — refines SPEC INV-11)** rgb-lib inserts exactly ONE OP_RETURN
  (the opret commitment); the sub-coin vouts MUST be recomputed from the colored tx by filtering
  non-OP_RETURN outputs in order, and their count MUST equal the split-entry count
  (rgb.rs:263-278). Consumers MUST use `output_vouts`, never assume pre-coloring indices.
- **GRN-INV-8 (fixed blinding)** SDK token flows use the fixed seal blinding
  `TOKEN_BLINDING = 777` (tokens.rs:18-20). This is a stated design simplification, safe in
  scope because the consignment travels inside the owner-encrypted transfer message and both
  sides derive validation from the consignment itself; it is NOT a secrecy mechanism.
  Per-transfer randomization is future work (tokens.rs:18-19 comment). Lib-level flows accept
  arbitrary blinding (rgb.rs:213).
- **GRN-REQ-12 (batch)** `batch_transfer_tokens` MUST carve N pieces
  `(1500 sats, amount_i)` + one change in ONE colored split (N+1 outputs + OP_RETURN), share ONE
  consignment across all pieces, and attach a per-piece envelope with that piece's own amount
  hint (tokens.rs:732-878). Hand-offs are per-piece and not atomic across recipients (SPEC §14
  "Batch atomicity").
- **GRN-INV-9 (conservation — = SPEC INV-13)** `Σ piece amounts + token_change =
  carrier_amount`, enforced by rgb-lib at coloring time (the transition must consume the input
  allocation exactly; rgb.rs:197-199, 246-256). The SDK arithmetic (GRN-INV-5) matches by
  construction; a violation cannot produce a valid consignment.

## 5. Receiver obligations for a partial amount

- **GRN-REQ-13 (everything IVL, plus booking)** A receiver of a piece (sats or token) MUST run
  the full sub-coin acceptance of INVALIDATION-SPEC §5 — transfer binding + ladder (IVL-REQ-10),
  branch validation (IVL-REQ-11), terminal ancestors (IVL-REQ-12 = SPEC REQ-17/ERR-7). For a
  token piece it MUST additionally:
  1. validate the envelope's consignment off-chain against the branch txids
     (`validate_offchain_chain_info`, tokens.rs:1240-1242; rust-rgb lib.rs:538-564);
  2. book **the amount the consignment assigns to the receiver's own witness outpoint**
     (`accept_offchain_amount`, rust-rgb lib.rs:512-533) — SPEC REQ-21. The envelope hint `a` is
     advisory ONLY: `booked != a` MUST reject (tokens.rs:1254-1262, GRN-ERR-11 = ERR-8);
  3. book under the consignment's cryptographically verified `contract_id`, never a
     sender-claimed id (tokens.rs:942-943; SPEC REQ-22);
  4. count Fungible assignments only — an InflationRight never books as balance (SPEC INV-26).

  Only after all of the above does `register_statechain` record the allocation
  (tokens.rs:1263-1269). A lying sender can neither inflate the booked amount (the consignment
  governs) nor redirect it to a different contract.

  *Receiver-configuration hazard (silent token loss).* Steps 1-4 run only when the receiving
  wallet has token support configured: `accept_incoming_tokens` returns `Ok(None)` without
  validating anything when `rgb_data_dir`/`rgb_proxy_url` are unset (tokens.rs:1191-1193). Such a
  receiver books the piece as plain 1,500-sat BTC, no token event fires, and — with no RGB
  engine — `token_carrier_outpoints` is empty (tokens.rs:364-385), so NONE of the GRN-REQ-14
  carrier guards apply: the receiver can split/withdraw/exit the packaging and permanently
  destroy the allocation. Utexo addresses do not encode token capability, so a sender CANNOT
  detect this. A receiver MUST have RGB configured before claiming token pieces; the consignment
  does remain in the claimed coin's backup rows, but recovery after late reconfiguration is
  untested (§11 limitation 11).

## 6. Carrier lifecycle

- **GRN-INV-10 (allocation moves piece-ward)** A colored split moves the transferred allocation
  onto the piece (the new carrier of `token_amount`, for the receiver) and the residual onto the
  change (the sender's new carrier of `token_change`); on a full spend (`token_change = 0`) the
  original carrier is marked spent in the RGB engine and the change sub-coin is **plain BTC**
  (tokens.rs:578-593). `sdk29` asserts the freed change surfaces in `available_sats` and that a
  plain `split_coin` on it SUCCEEDS (the freed sub-coin is un-laddered by [B0], so the plain
  primitive applies — §0); `transfer`/`withdraw`/`unilateral_exit` admit it through the
  same `is_token_carrier` predicate the split guard reads (transfer.rs:534-538;
  wallet.rs:814-825, 1038-1049), though those paths are not directly asserted on the freed coin.
- **GRN-REQ-14 (carrier exclusion matrix — audit [6][7], review H2)** Carriers MUST be excluded
  from every plain-BTC default and hard-errored when explicitly named:

  | Operation | Carrier handling | Where |
  |---|---|---|
  | `transfer` / `transfer_many` / `ensure_exact_coin` selection | silently excluded from candidates | transfer.rs:107, 408, 618-633 |
  | `split_coin` (named) | hard error | transfer.rs:677-681 (GRN-ERR-7) |
  | `withdraw` default / named | excluded / hard error | wallet.rs:814-825 (GRN-ERR-8) |
  | `unilateral_exit` default / named | excluded / hard error | wallet.rs:1038-1049 (GRN-ERR-8) |
  | `auto_exit_due` watchtower sweep | excluded from the plain-exit loop, then **MATERIALIZED** in a second pass (branch only, no sats-sweeping backup) — a received carrier DOES get deadline protection (GRN-INV-14) | wallet.rs:698-704 (exclusion), 726-760 (materialize) |
  | `claim()` ladder auto-establish | carriers **never laddered** (terminal freeze); fails CLOSED when RGB state is unavailable | wallet.rs:459-482 (sdk52) |
  | `start_lightning_swap` auto-select | excluded (audit [6]) | lightning.rs |
  | `get_balance` spendable sats | carrier sats excluded; fails CLOSED for token wallets (audit [23]) | wallet.rs:284-299 |

- **GRN-INV-11 (multi-carrier combine; one allocation per carrier)** A token transfer first tries a
  SINGLE carrier holding `>= amount` (tokens.rs:452-478); if none holds enough, the SDK **COMBINES**
  several carriers of the asset into one payment via `colored_combine_transfer` (tokens.rs) — a
  single SE-co-signed colored combine tx (N inputs → piece + change) built with
  `create_colored_combine_tx` (rgb.rs:330). Every combined carrier is made terminal first, and the
  receiver requires ALL N terminal (GRN-INV-11b). Only when the total asset balance across carriers
  is below `amount` does it fail, with a typed insufficient error (GRN-ERR-10). Verified by
  `SDK_E2E=31` (60 + 50 across two carriers pays 100). The SDK opens its rgb-lib wallet with
  `max_allocations_per_utxo: 1` (rust-rgb lib.rs:93), so a carrier holds exactly ONE allocation of
  ONE contract; multi-asset carriers are out of scope by configuration.
- **GRN-INV-11b (combine branch safety — receiver invariants)** A combined sub-coin's exit branch
  is a MULTI-INPUT DAG (the combine tx consumes N carriers). The receiver's `validate_branch`
  enforces, for the combine as for any branch: (1) it is a **tree** — no outpoint is consumed by
  two branch inputs (`reject_non_tree_branch`, transfer_receiver.rs); a non-tree branch is
  un-broadcastable (two conflicting spends of one outpoint) and would strand the receiver, which
  matters because token carriers are `single_use=false` so the SE re-signs them freely; (2) every
  on-chain root is unspent AND **confirmed** (`height > 0`, not a 0-conf mempool utxo); (3) value
  conservation per tx across ALL N inputs (SPEC INV-25); (4) locktime-free (SPEC INV-4). The
  terminal-ancestor count is `Σ inputs` across the branch (one per structural input, not per hop),
  so an N-input combine names + proves N terminal ancestors. **Substitution caveat (SPEC §14):** the
  blind SE binds no id to an outpoint, so the receiver's count check defeats OMISSION of ancestors,
  not SUBSTITUTION of terminal decoys for the real carriers; the residual defence is that the
  receiver holds the fully-signed (locktime-0) combine branch and MUST be able to exit immediately —
  the same guarantee as for splits, and why an online receiver is safe.
- **Sats-on-carrier are not independently splittable.** The only way sats leave a carrier is
  inside a colored split (piece 1500 / change residual) — GRN-REQ-10 forbids the plain path.
  A sibling change-holder broadcasting the shared branch is benign (byte-identical txs,
  IVL-REQ-13 note), it merely materializes funding early.

## 7. Effect on invalidation and exit

Cross-references made normative for granularity; mechanics are IVL-* and
[research/invalidation-economics.md](research/invalidation-economics.md).

- **GRN-INV-12 (structural consequence of a partial send)** Every partial send consumes its
  parent PERMANENTLY: budget 1 set then consumed (GRN-REQ-7), terminal at the SE, publicly
  queryable (IVL-INV-8/9), irreversible (IVL-INV-7). *On the UN-LADDERED shape* it mints exactly
  two (or N+1) depth+1 sub-coins with FRESH backup ladders anchored `H_split + initlock` sharing ONE
  branch (INVALIDATION-SPEC §9 "Fresh sub-ladders"); the tree's root deadline
  (`H_deposit + initlock`, exactness caveats per IVL-INV-10 / audit [17]) is UNCHANGED by
  splitting (IVL-INV-14). Depth does not consume ladder capacity; it does grow exit cost.

  *On the LADDERED shape the deadline half of that statement does not apply.* An in-ladder split
  replaces the parent's state with `SP` and hangs each child's own extension + state tiers off
  `SP.out[j]`: the tiers carry RELATIVE (CSV) locks and stay un-broadcast, so nothing ticks until
  someone starts the chain, an idle coin never ages toward a deadline, and renewal is off-chain and
  unbounded (`sdk43`). What replaces the old absolute-deadline discipline is the CENSUS: each hop
  discloses exactly one superseded state, which the receiver counts against the SE's signature count
  and proves out-raced (`sdk46`/`sdk47`/`sdk54`; a stale state loses at consensus, `sdk40` PART 2;
  a hostile trigger is defended by the watchtower, `sdk51`, keyless bundle `sdk45`). The
  invalidation-era E2Es that measured deadline arithmetic (sdk26 at scale, sdk27 over time) are
  retired as obsolete rather than repointed — there is no ladder floor to approach on this shape.
- **GRN-INV-13 (cost scaling: width beats depth)** k successive partial sends from one coin
  chain the LAST change coin to depth k; exit cost grows exactly **155 vB per level** on top of the
  112 vB backup. One `transfer_many`/`batch_transfer_tokens` fan-out costs ~**60 vB of branch per
  piece** (3 pieces + change = 241 vB). Payers of many parties SHOULD use width (one split) over
  depth (chained splits). *Evidence:* these are measured constants, now carried by the executable
  model — unit `granularity_model::width_vs_depth_weight_model` pins 112/155/241 and the width-beats-
  depth ordering through the real `ExitCostEstimate::fee_sats_at`, cross-pinned by
  `invalidation_model::exit_cost_scaling_model` (economics §3b). The E2E that originally measured
  them (sdk26) is retired; no live E2E re-measures the vsizes.

  *Scope:* the 155-vB-per-level figure is the UN-LADDERED branch model (§0). An in-ladder split adds
  TIER txs instead of branch txs, priced by `committed_fee_for_outputs(n, rate) + P2A_VALUE` per tier
  (lib/src/tesr.rs:340-348) and bounded per child by GRN-INV-1c; a laddered coin pays no rent while
  idle, so depth there costs tiers, not ladder capacity.
- **GRN-INV-14 (token exit: what is and is not guaranteed)** For a token piece:
  - **Guaranteed (no SE):** broadcasting the exit branch materializes the colored split txs —
    they ARE the RGB witnesses, their opret commitments confirm with them — and the allocation
    settles as an on-chain RGB holding at the piece outpoint (SPEC INV-16; rgb-lib refresh
    observes the confirmed witness; `sdk29` asserts on-chain settlement of the exact partial
    amount at depth 2; `sdk39` repeats it end-to-end for a piece two colored splits deep).
    Carriers are excluded/hard-errored from the plain exit paths (GRN-REQ-14), so the SDK cannot
    destroy the allocation on the way out. **The guarantee is protocol-level, and it now has ONE
    automatic SDK path:** the `auto_exit_due` watchtower MATERIALIZES a received carrier whose
    deadline is within `margin_blocks` — it broadcasts the exit branch ONLY (never the
    sats-sweeping backup, which would close the seal without a transition and burn the allocation),
    emitting `TokenCarrierMaterialized` (wallet.rs:726-760; `sdk34`: an issued/flat carrier has no
    branch and is skipped, a received one is materialized near the deadline and the sender's later
    stale-backup clawback FAILS because the shared root is already spent). No other shipped call
    broadcasts a carrier's branch: `unilateral_exit`/`withdraw` refuse the carrier
    (wallet.rs:1038-1049, 814-825) and `broadcast_branch_if_any` is `pub(crate)`. So a holder who
    runs neither `auto_exit_due` nor the background watcher still owns the IVL-REQ-16 root-deadline
    discipline manually: broadcast the stored branch rows (`branch-<id>` / recovery bundle) directly
    — as `sdk29`/`sdk39` do with a raw transaction-broadcast loop — or rely on a co-descendant's
    exit materializing the shared branch (deep-dive §5.6).
  - **Exit material:** branch rows + backup + **consignment** (`BackupTx.rgb_consignment` on the
    piece's first backup row, tokens.rs:595-617). All of it lives in the recovery bundle and is
    **NOT seed-derivable** (deep-dive §5.9; wallet.rs:56-60) — a mnemonic-only restore cannot
    exit or prove the token.
  - **NOT guaranteed:** (a) SE-less *onward movement* of the settled allocation — the piece's
    only pre-signed spend is its PLAIN backup (`create_tx1`, via `register_split_subcoins_n`,
    transfer.rs:1059-1140), which is
    RGB-unaware and would burn the allocation; no colored exit/backup path is shipped in the SDK
    (audit [7] fix chose exclusion + hard error). Moving the settled asset requires an
    SE-co-signed colored spend. (b) *Economic recovery of the packaging sats:* the piece's backup
    sweeps `1500 − ceil(112·r)` sats where `r` is the fee rate frozen at creation
    (lib/src/transaction.rs:116-132; `min(SE quote, max_fee_rate)`, default 1.0 sat/vB ⇒ net
    ≈ 1388 sats), and backup creation itself fails once `1500 − fee < 330` (r ≳ 10 sat/vB). At
    high feerates the 1500 sats MAY be economically dust while the TOKEN value settles intact —
    the exit guarantee attaches to the allocation, not to the packaging sats.

## 8. Error semantics

Message strings verified in code; `{}` are runtime values.

| ID | Trigger | Exact behaviour | Where | SPEC |
|---|---|---|---|---|
| GRN-ERR-1 | balance < target (the same typed error also surfaces GRN-ERR-2's planner refusals) | `insufficient balance: requested {r} sats, available {a}` (`SdkError::InsufficientBalance`) | types.rs:69-73; transfer.rs:59-65 | ERR-9 |
| GRN-ERR-2 | planner cannot mint the remainder despite sufficient balance: remainder < `min_output`, OR no unused coin > remainder + fee_reserve + `min_output` (both audit [29]). **Unit-level evidence only** — the E2E that asserted the second clause (sdk28: 4,800 from a single 5,000-sat coin) is retired and nothing replaces it | same `Insufficient`/ERR-9 refusal | select.rs:114-127; unit `sub_dust_remainder_is_refused` (select.rs:178), `granularity_model::plan_paths_matrix` | ERR-9 |
| GRN-ERR-3 | piece + reserve ≥ parent | `piece {p} + fee reserve {f} does not fit in coin of {n} sats` | transfer.rs:1455-1460 | INV-10 |
| GRN-ERR-4 | sub-dust piece/change (plain split) | `split would create an unviable output (piece {p}, change {c}, minimum {min_output}) — each sub-coin must clear the 330-sat dust floor AND fund its own backup; the split tx or a sub-coin backup would be unbroadcastable` | transfer.rs:1464-1469 | audit [9] |
| GRN-ERR-5 | any split output < 330, or no fee room, at PSBT build | `MercuryError::FeeTooLow` (backstop for transfer_many + colored paths) | lib/src/transaction.rs:346-360 | audit [9] |
| GRN-ERR-6 | no parent big enough | `no confirmed coin large enough for {total} sats + fee` / `no coin large enough to mint {sats} sats` | transfer.rs:371, 510 | — |
| GRN-ERR-7 | plain split of a carrier | `coin {id} carries an RGB token allocation; splitting it as plain BTC would destroy the token — use a token transfer or pick a different coin` | transfer.rs:534-538 | audit [7] |
| GRN-ERR-8 | withdraw / unilateral exit naming a carrier | `coin {id} carries an RGB allocation; withdrawing it as plain BTC would destroy the tokens — move the asset off this coin first` / `…a plain unilateral exit would destroy the tokens — move the asset off this coin first` | wallet.rs:818-822, 1042-1046 | audit [7] |
| GRN-ERR-9 | carrier sats too small for a token split | `carrier coin too small ({c} sats) for a token split` / batch: `carrier coin too small ({c} sats) for {n} pieces + fee` | tokens.rs:499-503, 1021-1024 | — |
| GRN-ERR-10 | total asset balance across carriers is below the amount (after trying single-carrier then combine) | `insufficient {asset}: wallet holds {total} across {n} carrier(s), need {amount}` / `no combination of carriers covers {amount} … with enough sats` (tokens.rs:764) / batch: `no confirmed coin carries >= {total} of {asset} for the batch` (tokens.rs:1015) | tokens.rs:741, 764, 1015 | — |
| GRN-ERR-11 | consignment/envelope mismatch or invalid consignment at claim | `token consignment assigns {booked} to this coin but the envelope claimed {a} — rejecting` / `incoming token consignment INVALID: {detail}` | tokens.rs:936-955 | ERR-8 |
| GRN-ERR-12 | fulfilling an expired invoice | `invoice expired at {exp} (now {now})` | invoice.rs:85-93 | ERR-11 |
| GRN-ERR-13 | a split output's deposit-token slot is unpaid (SE charges for tokens) | `deposit token payment required: pay {fee_sats} sats to {deposit_address} (token {token_id}), then retry` (`SdkError::TokenPaymentRequired`); raised BEFORE the terminal-guard on every split path (§3 note) | types.rs:63-68; take_token call sites transfer.rs:579-597, tokens.rs:494-509, 736-755 | — |

## 9. Invoices

- **GRN-REQ-15 (invoice amount units — refines SPEC REQ-28)** `UtexoInvoice.amount` is `u64` and
  its unit is selected by `asset_id`: **sats** when `None`, **raw token units** when
  `Some(contract_id)` (invoice.rs:14-26). `fulfill_utexo_invoice` MUST check expiry
  (GRN-ERR-12 = ERR-11) and route to `transfer` (sats) or `transfer_tokens` (raw units)
  accordingly (invoice.rs:83-98) — inheriting every guarantee and error of §2-§4.
- *Non-normative:* on RGB Lightning legs the SSP enforces asset value at claim time (the
  GRN-REQ-13 consignment validation) plus a post-claim balance-delta gate before reporting the
  preimage (ssp.rs:390-430, 469-484; audit [3][4] closed).

## 10. Traceability

Tests marked **(new)** were added in the granularity test pass (E2E slot 29 + unit
`granularity_model.rs`). The sats-granularity E2E of that pass (slot 28) is RETIRED — the plain-sats
lane it drove is now only one of the two shapes (§0) and its fee model no longer governs a laddered
coin; rows below name the live replacement, and say so explicitly where nothing replaces it.

| Item(s) | Verifying test(s) |
|---|---|
| GRN-REQ-4/5/6, GRN-ERR-1/2 | sdk01 (exact subset, then a non-exact payment through `transfer()`); sdk59 (the in-ladder route of a `WithSplit` plan, end to end: Alice pays Bob, Bob claims + exits the child); sdk60 (whole-child re-transfer alice→bob→carol), sdk17 (partial second hop); `unit::select`, `unit::granularity_model::plan_paths_matrix` (all four plan paths over the real planner). **Gap:** the no-admissible-split-candidate refusal (sdk28's 4,800-from-5,000 case) now has UNIT evidence only |
| GRN-INV-1/1b/2, GRN-ERR-3/4/5 | `unit::split_math_tests` (transfer.rs:1482+); **unit `granularity_model.rs`** — boundary matrix over the real `split_amounts`/`split_fee_reserve`/`select::plan` (dust floor 330, min parent 960 ± 1, reserve-clamp interactions; the planner-side 961 pin is `unit::invalidation_model::split_size_floor`) and `backup_fee_floor_is_the_true_mintable_minimum` (the whole `[330, 442)` band refused up-front, floor climbing with feerate); **sdk29** — E2E: a freed sub-coin is split at exactly the TRUE minimum mintable piece `330 + backup_fee` (442 at 1 sat/vB) and both outputs confirm (replaces sdk28's measurement) |
| GRN-INV-1c (in-ladder floor, D1) | sdk30 (sponsored rebate sized at `max(fee + DUST_LIMIT, min_child_value)`, the 1306-at-2-sat/vB identity — the D2 regression that made `refresh_sponsored` fail after the user had paid the on-chain fee); sdk58/sdk59 (admitted children exit), sdk17 (child-level split); the floor is applied per-output on the multi-child batch routes too (transfer.rs:976-987, 1323-1335) |
| GRN-REQ-7 (terminal ordering) | sdk58 (parent IS terminal once `SP` is co-signed; an unlatched child stays NON-terminal so it can be re-transferred); sdk50 (unilateral exit of a laddered coin); SPEC REQ-18 rows |
| GRN-REQ-8 (transfer_many) | **sdk69** — the LADDERED-parent route: the plain split of that parent is refused, `transfer_many` carves one `SP` with 2 recipient children + change + P2A off `X_m.out[0]`, the batch stays off-chain, and the [B1] attack is then executed for real (alice broadcasts her retained trigger, spending `F`) with both recipients still exiting for their exact amounts; **sdk11** — multi-recipient parity, now asserting the ROUTE (both pieces are children of ONE `SP`, at `SP.out[0]`/`SP.out[1]`) and not just the amounts |
| GRN-INV-13 (width vs depth) | `unit::granularity_model::width_vs_depth_weight_model` (112/155/241 vB pinned, width beats depth for every k ≥ 2 through the real `fee_sats_at`), cross-pinned by `unit::invalidation_model::exit_cost_scaling_model`. The E2E that measured the vsizes (sdk26) is retired; no live E2E re-measures them |
| GRN-REQ-9 | sdk63/sdk64 (Lightning pay/receive on the ladder via the HODL latch — the exact leg still uses `ensure_exact_coin`); sdk65/sdk67 (the NON-EXACT in-ladder fallback that makes the one-call APIs usable on laddered coins — D3); sdk66/sdk68 (pay-failure reclaim, non-exact and exact) |
| GRN-REQ-10/14, GRN-ERR-7/8 | audit [6][7] fix verification; **sdk29** — spent-carrier change is plain BTC (surfaces in `available_sats`; plain `split_coin` succeeds — GRN-INV-10's transfer/withdraw claim rides the shared predicate, not a direct assert) and the typed carrier refusal on `unilateral_exit` + the exit-everything sweep; sdk34 (the watchtower's carrier lane: skip an issued/flat carrier, MATERIALIZE a received one); sdk52 (a carrier is never laddered — the terminal-freeze row) |
| GRN-REQ-11, GRN-INV-4/5/6, GRN-ERR-9 | sdk02 (250/750 token split + envelope checks); rgb01 (750/250 colored); **unit `granularity_model.rs::token_split_bounds_model` (new)** — token min-carrier boundary (2130 ± 1) over the real `split_amounts`; **sdk29 (new)** — 1-raw-unit send + the typed GRN-ERR-9 refusal on a 1,500-sat received piece (fit-guard boundary 1,800, not 2,130) |
| GRN-REQ-12 (batch) | sdk09 (IFA mint + batch 200/300) |
| GRN-INV-7 | rgb01/rgb03 (output_vouts over OP_RETURN); SPEC INV-11 rows |
| GRN-INV-9 | sdk02/sdk09 (conservation); chaos22 oracle (INV-13) |
| GRN-REQ-13, GRN-ERR-11 | sdk02, sdk09, rgb13 (consignment integrity / ERR-8); `unit::envelope_tests` (tokens.rs:1398+) |
| GRN-INV-10/11/11b, GRN-ERR-10 | rgb03/rgb06 (chained colored splits); **sdk31 (new)** — multi-carrier COMBINE: carriers 60+50 pay 100 in one 2-input colored combine, receiver requires 2 terminal ancestors, combined coin exits on-chain; over-balance ⇒ typed insufficient error; `unit::transfer_receiver::terminal_parents_tests` (required-ancestors = Σ inputs; non-tree branch rejected) |
| GRN-INV-12/13 | `unit::granularity_model::width_vs_depth_weight_model` + `unit::invalidation_model::exit_cost_scaling_model` (the depth/width scaling model; sdk26, which measured it on-chain, is retired); INVALIDATION-SPEC §10 rows |
| GRN-INV-14 | rgb01-06 (materialized colored exits, lib level); **sdk29** — token exit at depth 2 with on-chain settlement of the exact partial amount (branch broadcast manually via raw txs); sdk39 (a dedicated depth-2 token exit, independent observer validates the consignment from the indexer); sdk34 (the AUTOMATIC path: `auto_exit_due` materializes a received carrier near its deadline and the sender's later stale-backup clawback fails) |
| GRN-REQ-15, GRN-ERR-12 | sdk11; `unit::invoice::tests` |
| GRN-REQ-2 (precision metadata) | rgb14 (precision/metadata); **unit `granularity_model.rs::raw_units_precision_model` (new)** — raw-unit/precision arithmetic up to 10^18; **sdk29 (new)** — precision-2 issuance + 1-raw-unit send (the SDK never scales) |

Unit module: `clients/libs/rust-sdk/src/granularity_model.rs` mirrors
`invalidation_model.rs` — it calls the real admission functions, so a guard change fails the
executable spec until this document is updated.

## 11. Known limitations

None of these may be omitted when citing this spec.

1. **Multi-carrier combine — SHIPPED.** A payment spanning several carriers is minted by one
   SE-co-signed colored combine (GRN-INV-11/11b; `SDK_E2E=31`); GRN-ERR-10 now fires only on true
   insufficiency. Remaining edges: the sender-side `register_combine_subcoins` rejects combining
   carriers that share a common ancestor (disjoint sub-branches only — rare); and the blind-SE
   SUBSTITUTION caveat (SPEC §14) applies to a combined coin exactly as to a split (an online
   receiver must exit promptly to win the race).
2. **One allocation per carrier UTXO.** By SDK configuration (`max_allocations_per_utxo: 1`,
   rust-rgb lib.rs:93) — an rgb-lib wallet option, not an RGB protocol limit.
3. **Fixed `TOKEN_BLINDING = 777`** (GRN-INV-8): a design simplification, safe only because
   consignments travel owner-encrypted; not a privacy feature.
4. **u32 sats cap** (~42.9 BTC/coin; GRN-REQ-1, SPEC §14).
5. **Sats on a carrier are not independently splittable** (§6); they move only inside colored
   splits, and the packaging sats' recovery can be uneconomic at high feerates (GRN-INV-14).
6. **Precision is immutable post-issuance** and purely display-level; a precision-0 asset can
   never be sent in fractions (GRN-REQ-2).
7. ~~**Late dust floor on `transfer_many` and colored paths**~~ — **SUPERSEDED by limitation 13
   (FIXED).** Both paths now check the backup-fee floor up-front, before the terminal-guard
   (GRN-REQ-8, GRN-INV-6), so a boundary-violating attempt no longer pins the parent. Kept as a
   record of the old behaviour.
8. **No SDK colored exit**: SE-less onward transfer of a settled token allocation is not possible
   today; the pre-signed backup of a piece is plain BTC and allocation-destroying if misused
   (GRN-INV-14; audit [7]). Materializing a carrier's branch, by contrast, IS shipped: the
   `auto_exit_due` watchtower broadcasts the branch (branch only, never the backup) for a received
   carrier near its deadline (sdk34). The plain exit paths still refuse carriers, so a holder who
   runs no watchtower pass broadcasts the stored branch rows (recovery bundle / `branch-<id>`)
   directly, as sdk29/sdk39 do, or relies on a co-descendant's exit (GRN-INV-14).
9. **Consignments are not seed-derivable** — the recovery bundle, not the mnemonic, is the token
   backup (GRN-INV-14; deep-dive §5.9).
10. **Received token pieces are re-send-incapable at the SDK layer.** A received piece carries
    exactly 1,500 sats (GRN-REQ-11) — below the 2,130 minimum carrier (GRN-INV-6) — so
    `transfer_tokens` from it always fails GRN-ERR-9: the receiver can hold or exit, never
    forward off-chain. This holds for ANY fixed packaging `P` (`P >= P + reserve + 330` never
    holds), so SDK token rails are structurally **one-hop** (issuer/holder with a fat carrier
    fans out; receivers terminate — economics §3, load-bearing; §7 there shows no constant fixes
    it). Fix is combine/top-up or variable packaging. Asserted by sdk29 (typed refusal on a
    1,500-sat received piece).
11. **A receiver without token support silently books packaging only.** `accept_incoming_tokens`
    is a no-op when `rgb_data_dir`/`rgb_proxy_url` are unset (tokens.rs:1191-1193): the piece books
    as plain 1,500-sat BTC, no carrier guard applies (empty carrier set), and the allocation is
    destroyable by the receiver's ordinary BTC operations. Senders cannot detect the receiver's
    configuration from the address (GRN-REQ-13 hazard note). The consignment survives in the
    backup rows; recovery after late reconfiguration is untested.
12. **Planner conservatism; multi-coin non-atomicity.** `plan()` is greedy and non-backtracking —
    some fundable targets are refused, and the planner is one sat stricter than the executor at
    every boundary (GRN-REQ-5 note; economics §2). Multi-coin plans are handed over sequentially,
    not atomically (GRN-REQ-4 note; SPEC §14).
13. **Backup-fee floor (GRN-INV-1b) — FIXED.** The split admission now requires each sub-coin
    output to clear `330 + ceil(112·fee_rate)` (`min_split_output`, ≈ 442 sats at 1 sat/vB), not
    just the 330-sat dust floor, on every split path (`split_coin`, `transfer_many`, colored
    `transfer_tokens`/`batch_transfer_tokens`, and `select::plan_with_floor`), checked BEFORE the
    parent is made terminal. A sub-viable piece is refused with the parent untouched — previously
    it was admitted, made the parent terminal, then failed at backup creation (`FeeTooLow`),
    stranding the parent to unilateral-exit-only. Verified by
    `unit::granularity_model::backup_fee_floor_is_the_true_mintable_minimum` (the `[330, 442)` band
    refused up-front) and sdk29 (a piece minted at exactly the true floor); the E2E that first
    measured the floor, sdk28, is retired.
    **The LADDERED lane's counterpart is GRN-INV-1c (D1, also FIXED):** an in-ladder split child must
    clear `min_child_value` (1306 sats at 2 sat/vB) because it funds its own extension + state tiers,
    and the guard used to apply the 442 backup floor instead — admitting a child that terminalized
    the parent and only then died with `FeeTooHigh`, stranding it. Its sibling D2: `refresh_sponsored`
    sized its rebate into that same dead window, so a sponsored refresh failed AFTER the user had
    paid the on-chain fee; the rebate is now `max(fee + DUST_LIMIT, min_child_value)` (sdk30).
14. **Same-asset double-receive — FIXED.** A wallet can now receive a second, separate allocation
    of an asset it already holds. `save_new_asset` (rgb-lib fork, offline.rs:1710) is idempotent on
    an already-known asset: it skips the duplicate asset-metadata insert (which previously hit
    `UNIQUE constraint failed: asset.id` and stranded the second allocation) while the runtime
    contract import (new transitions) and the allocation registration still run. Verified by sdk29
    (bob receives PT2 three times: 10 → 11 → 9,996, balance sums each time).
15. ~~**`transfer_many` has no in-ladder route and does not refuse laddered parents**~~ — **FIXED
    (multi-child in-ladder split).** Its parent filter used to exclude carriers only, so on an
    ordinary wallet — where every root deposit is laddered (§0) — a batch plain-split a laddered
    coin: exactly the [B1] shape `split_coin` hard-refuses, because whoever retains the parent's
    no-timelock trigger over the same funding outpoint could void the whole batch and kill every
    recipient's piece at once. `transfer_many` now dispatches on the parent's shape like `transfer`
    does (GRN-REQ-8): a laddered ROOT goes through `in_ladder_pay_many` (one `SP` over `X_m.out[0]`
    carving N children + change), a received CHILD through `child_in_ladder_pay_many` (one `CSP` at
    the child's level), and only an un-laddered coin still takes the plain split. `build_split_state`
    and `in_ladder_split`/`child_in_ladder_split` already accepted N children (lib/src/tesr.rs:371,
    clients/libs/rust/src/tesr.rs:285, 645), so the fix is in the SDK's routing + selection +
    per-output floors, not the tier builders. Covered by **sdk69**, which pays two recipients out of
    one laddered parent and then executes the B1 attack for real — alice broadcasts her retained
    trigger, consuming `F`; both recipients still exit unilaterally for their exact amounts, because
    `SP` descends from that trigger rather than racing it. `sdk11` now asserts the route (both pieces
    are children of ONE `SP`, at `SP.out[0]`/`SP.out[1]`), not merely the amounts.
16. **Status polling of a unilaterally-exiting child — FIXED (D4).** `withdraw` routes an in-ladder
    child to `unilateral_exit` (its funding `SP.out[j]` is un-broadcast, so a cooperative withdraw
    has no outpoint to spend) and marks it `WITHDRAWING` (wallet.rs:838-859). Such a coin has
    neither a withdrawal tx nor a withdrawal address, which used to make every subsequent status
    poll error for the life of the coin; the poller now treats that combination as "tracked by the
    pre-signed exit chain, nothing to check" (clients/libs/rust/src/coin_status.rs:154-164). This
    belongs here because every in-ladder piece and change IS a child (GRN-INV-1c).
