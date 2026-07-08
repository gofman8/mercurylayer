# Coin Granularity (Partial Amounts) — Normative Specification

This document is the authoritative reference for **coin granularity**: how the Spark-parity SDK
pays, splits, and books *partial amounts* — sats below a coin's size and token amounts below a
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
  `u32::try_from` (clients/libs/rust-sdk/src/transfer.rs:421). Other paths cast unchecked
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
  transfer.rs:537-592, and the colored variant likewise, rgb.rs:219-292); transfer messages are
  owner-encrypted and never deserialized by the SE (SPEC §3). Granularity is therefore enforced
  entirely client-side — the sender constructs, the receiver independently validates (§5) — and
  adds **zero** trust-model cost over whole-coin transfers.

## 2. Payment planning (sats)

`transfer(address, amount)` plans over confirmed, non-carrier coins via `select::plan`
(clients/libs/rust-sdk/src/select.rs:75-123; transfer.rs:35-127).

- **GRN-REQ-4 (plan semantics — refines SPEC REQ-15/INV-9)** `plan(coins, target)` MUST return
  exactly one of:
  1. `Exact(subset)` — a subset summing to `target` exactly (dynamic-programming subset search,
     select.rs:34-72). The SDK then performs **N whole-coin key handovers**, one transfer message
     per coin (transfer.rs:96-120). No split occurs.
  2. `WithSplit{whole, split, split_amount}` — whole coins are handed over and ONE coin is split
     to mint the exact remainder piece, which is handed over (transfer.rs:73-92).
  3. `Insufficient{available}` — surfaced as `SdkError::InsufficientBalance` (GRN-ERR-1 = ERR-9).

  *Note (multi-coin plans are not atomic):* the SDK executes a multi-coin plan as a SEQUENCE of
  per-coin key handovers (a `?`-aborting loop, transfer.rs:96-120): a failure partway leaves a
  partial payment — coins already handed over belong to the receiver, the rest stay with the
  sender. This is the same non-atomicity SPEC §14 "Batch atomicity" documents for
  `transfer_many`/`batch_transfer_tokens`; there is no all-or-nothing guarantee across coins on
  any multi-coin path.
- **GRN-REQ-5 (split-candidate admission — audit [29])** The split candidate MUST satisfy
  `remaining >= 330` AND `candidate_sats > remaining + fee_reserve(candidate_sats) + 330`
  (select.rs:103-113) — i.e. the planner only picks a coin the split executor (§3) will accept,
  leaving a non-dust change. A remainder below 330 sats MUST yield `Insufficient` **even when the
  wallet balance covers the target** (unit `sub_dust_remainder_is_refused`, select.rs:163-168):
  a sub-dust piece would make the split tx unbroadcastable (GRN-INV-1).

  *Note (planner conservatism — the planner is incomplete and 1 sat stricter than the executor):*
  `plan()` is greedy largest-first with NO backtracking (select.rs:85-98), so `Insufficient` does
  NOT imply that no payable composition exists — coins {1,000; 970}, target 1,300 is refused
  (`Insufficient{available: 1970}`) though payable by hand (economics §2 "greedy shadow"; the
  workaround is a manual `split_coin` + `transfer`). And because the admission filter is strictly
  `candidate > remaining + reserve + 330` (select.rs:110) while the executor admits equality
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

`split_coin(id, piece_sats)` mints `{piece, change}` from one confirmed coin in one SE-co-signed,
un-broadcast tx (transfer.rs:287-381).

- **GRN-INV-1 (admission rule — refines SPEC INV-10)** With
  `fee_reserve = clamp(parent/100, 300, 2000)` sats (`split_fee_reserve`, transfer.rs:596-599),
  a split of `piece` out of `parent` is admissible **iff** (`split_amounts`, transfer.rs:612-628):
  1. `piece + fee_reserve < parent` (strict; `>=` errors GRN-ERR-3), and
  2. `piece >= 330` and `change = parent − piece − fee_reserve >= 330`
     (`DUST_LIMIT = 330`, transfer.rs:605; audit [9]).

  **Exact boundary.** The dust floor on `change` binds first: `parent >= piece + reserve + 330`.
  The minimum splittable parent is therefore **960 sats exactly** — at `parent = 960`
  (`reserve = clamp(9,300,2000) = 300`) the only admissible piece is 330, giving
  `change = 960 − 330 − 300 = 330`, which passes both checks; at `parent = 959` the same piece
  yields `change = 329` and is refused (GRN-ERR-4). The fit check (1) is strict but non-binding
  at this boundary (330 + 300 = 630 < 960). The planner (GRN-REQ-5) is one sat stricter: it first
  accepts a 961-sat candidate for a 330-sat remainder (see the GRN-REQ-5 conservatism note).

  **GRN-INV-1b (backup-fee floor — the true minimum *mintable* piece).** The 330-sat dust floor
  above guards the split-tx *output*; it is necessary but **not** sufficient for a usable
  sub-coin. Each sub-coin also needs a valid backup tx, and `create_tx1` sweeps
  `sub_coin_sats − ceil(BACKUP_TX_SIZE·fee_rate)` and rejects it below the dust floor
  (`MercuryError::FeeTooLow`, lib/src/transaction.rs:122-132; `BACKUP_TX_SIZE = 112` vB,
  `fee_rate = min(SE-quoted, client max_fee_rate)`). So the minimum **mintable** piece (and change)
  is `330 + ceil(112·fee_rate)` — **442 sats at 1 sat/vB** (measured, sdk28:
  `backup_fee=112 min_mintable_piece=442`), rising with feerate. A 330-sat piece is a valid split
  output whose *backup* is un-broadcastable, so it cannot be minted into a usable coin. **Guard
  gap (known limitation, flagged for fix):** `split_amounts` (and the planner filter) enforce only
  the 330 floor, not the backup-fee floor, so a piece in `[330, 330 + ceil(112·fee_rate))` passes
  admission, the parent is made terminal (GRN-REQ-7) and the split co-signed, and only *then* does
  the piece backup fail — stranding the parent to unilateral-exit-only (degradation class of the
  audit [15] brick; no theft, funds recoverable via the parent's own backup). The fix raises the
  admission floor to `330 + backup_fee` on every split path, checked before the terminal-guard.
- **GRN-INV-2 (resolution domain)** For a given parent, every
  `piece ∈ [330, parent − fee_reserve − 330]` is admissible, at 1-sat steps (SPEC INV-22).
- **GRN-INV-3 (tx shape)** A plain split tx has locktime 0 (IVL-INV-5 = SPEC INV-4), exactly one
  input (the parent) and one output per split entry (SPEC INV-11); `get_unsigned_split_psbt`
  additionally rejects `Σ outputs >= input` and any output `< 330` with `MercuryError::FeeTooLow`
  (lib/src/transaction.rs:346-360, GRN-ERR-5).
- **GRN-REQ-7 (ordering)** `split_coin` MUST validate GRN-INV-1 *before* touching the parent
  (transfer.rs:307-310), then set the parent's spend budget to 1 immediately before co-signing
  (transfer.rs:349-355; SPEC REQ-18 = IVL-REQ-7). The split consumes the budget: the parent is
  terminal, permanently (§7).
- **GRN-REQ-8 (transfer_many — refines SPEC REQ-27)** `transfer_many(recipients)` MUST carve all
  N recipient pieces plus one change in ONE split tx (N+1 outputs, no OP_RETURN, so vout i = i;
  transfer.rs:133-247). Parent selection: the **smallest** confirmed, non-carrier coin with
  `amount > total + fee_reserve(amount)` (transfer.rs:149-160);
  `change = parent − Σ amounts − fee_reserve`. *Note (sharp edge):* unlike `transfer`, this
  parent filter does not add the +330 change margin and the SDK does not pre-check per-piece
  dust; a sub-330 piece or change is only caught by the lib floor (GRN-ERR-5) *after* the
  terminal-guard, leaving the parent pinned to exactly one remaining co-signature (recoverable —
  the budget admits precisely the one structural co-sign a corrected retry needs; monotonicity
  per IVL-INV-7). This late-dust edge is currently **spec-only**: `sdk28`'s boundary part
  exercises the single-coin `transfer` refusal and the 330-sat minimum piece (not
  `transfer_many`), and `granularity_model.rs` pins `split_amounts`/`plan`, which this filter
  bypasses — a dedicated `transfer_many` boundary probe remains a test gap.
- **GRN-REQ-9 (ensure_exact_coin)** `ensure_exact_coin(sats)` MUST reuse an existing confirmed
  non-carrier coin of exactly `sats` if one exists, else split the smallest sufficient coin
  (same filter as GRN-REQ-8) to mint it (transfer.rs:249-281). This is the amount-maker behind
  single-coin flows (Lightning swaps, latches).
- **GRN-REQ-10 (carrier refusal)** `split_coin` MUST refuse a token carrier with a hard error
  (transfer.rs:299-306, GRN-ERR-7): a plain split spends the carrier RGB-unaware and would
  destroy the allocation (review H2 / audit [7]). Token amounts move ONLY via §4.
- *Note (deposit-token slots):* every split output is a fresh statechain slot and consumes one
  deposit token from the SE's anti-spam token server, requested automatically — `split_coin`
  takes two (transfer.rs:315-330), a colored split two (tokens.rs:494-509), `transfer_many` /
  `batch_transfer_tokens` N+1 (transfer.rs:170-186; tokens.rs:736-755). When the SE charges for
  tokens, the operation fails with the typed `SdkError::TokenPaymentRequired` (GRN-ERR-13)
  **before** the terminal-guard on every path, so a token failure never pins the parent.

## 4. Colored (RGB) splits

`transfer_tokens` (tokens.rs:392-673) and `batch_transfer_tokens` (tokens.rs:678-878) perform a
**colored split** of one carrier via `create_colored_split_tx` (rgb.rs:201-298).

- **GRN-INV-4 (output triple)** A colored split output is
  `SplitOutput = (address, sats, rgb_amount)` (rgb.rs:165-167). An output with `rgb_amount = 0`
  is left **uncolored** — plain sats, no zero-value allocation (rgb.rs:248-255).
- **GRN-REQ-11 (fixed piece sats)** Every token piece MUST carry exactly
  `TOKEN_PIECE_SATS = 1500` sats (tokens.rs:23, 531-534, 744). Rationale: the sats are packaging
  (comfortably above the 330 floor, with margin for the piece's own backup fee, §7); the token
  amount is the payload. The receiver's booked BTC for a token receive is therefore always
  1500 sats per piece, independent of token value. *Consequence (one-hop):* 1,500 is below the
  2,130 minimum carrier (GRN-INV-6), so a received piece can NEVER fund a further
  `transfer_tokens` — the receiver holds or exits (§11 limitation 10; economics §3).
- **GRN-INV-5 (change formulas)** For a single-recipient colored split:
  `change_sats = carrier_sats − 1500 − fee_reserve` (same reserve clamp as GRN-INV-1,
  tokens.rs:484-490) AND `token_change = carrier_amount − token_amount` (tokens.rs:491). When
  `token_change > 0` the change output is colored with it and registered as the residual carrier;
  when `token_change = 0` the change output is uncolored and the change coin is **plain BTC**
  (tokens.rs:578-593; §6).
- **GRN-INV-6 (minimum carrier)** Derivation: `1500 (piece) + fee_reserve (>= 300) +
  change (>= 330)` ⇒ the minimum carrier for a token send is **2130 sats**. Layering: the SDK
  guard only rejects `1500 + fee_reserve >= carrier_sats` (tokens.rs:485-489, GRN-ERR-9), i.e.
  admits carriers ≥ 1801; carriers in [1801, 2129] then fail the lib dust floor on change
  (GRN-ERR-5) — *after* the terminal-guard, with the same recoverable one-co-sign pinning as
  GRN-REQ-8's note. Batch: min carrier = `1500·N + reserve + 330`, layered the same way — the
  SDK guard rejects only `1500·N + fee_reserve >= carrier_sats` (tokens.rs:723-728); the +330
  change floor is again the lib backstop (GRN-ERR-5), reached after the terminal-guard.
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
     (`validate_offchain_chain_info`, tokens.rs:931-941; rust-rgb lib.rs:538-564);
  2. book **the amount the consignment assigns to the receiver's own witness outpoint**
     (`accept_offchain_amount`, rust-rgb lib.rs:512-533) — SPEC REQ-21. The envelope hint `a` is
     advisory ONLY: `booked != a` MUST reject (tokens.rs:947-955, GRN-ERR-11 = ERR-8);
  3. book under the consignment's cryptographically verified `contract_id`, never a
     sender-claimed id (tokens.rs:942-943; SPEC REQ-22);
  4. count Fungible assignments only — an InflationRight never books as balance (SPEC INV-26).

  Only after all of the above does `register_statechain` record the allocation
  (tokens.rs:956-962). A lying sender can neither inflate the booked amount (the consignment
  governs) nor redirect it to a different contract.

  *Receiver-configuration hazard (silent token loss).* Steps 1-4 run only when the receiving
  wallet has token support configured: `accept_incoming_tokens` returns `Ok(None)` without
  validating anything when `rgb_data_dir`/`rgb_proxy_url` are unset (tokens.rs:884-886). Such a
  receiver books the piece as plain 1,500-sat BTC, no token event fires, and — with no RGB
  engine — `token_carrier_outpoints` is empty (tokens.rs:364-385), so NONE of the GRN-REQ-14
  carrier guards apply: the receiver can split/withdraw/exit the packaging and permanently
  destroy the allocation. Spark addresses do not encode token capability, so a sender CANNOT
  detect this. A receiver MUST have RGB configured before claiming token pieces; the consignment
  does remain in the claimed coin's backup rows, but recovery after late reconfiguration is
  untested (§11 limitation 11).

## 6. Carrier lifecycle

- **GRN-INV-10 (allocation moves piece-ward)** A colored split moves the transferred allocation
  onto the piece (the new carrier of `token_amount`, for the receiver) and the residual onto the
  change (the sender's new carrier of `token_change`); on a full spend (`token_change = 0`) the
  original carrier is marked spent in the RGB engine and the change sub-coin is **plain BTC**
  (tokens.rs:578-593). `sdk29` asserts the freed change surfaces in `available_sats` and that a
  plain `split_coin` on it SUCCEEDS; `transfer`/`withdraw`/`unilateral_exit` admit it through the
  same `is_token_carrier` predicate the split guard reads (transfer.rs:299-306;
  wallet.rs:527-547, 726-746), though those paths are not directly asserted on the freed coin.
- **GRN-REQ-14 (carrier exclusion matrix — audit [6][7], review H2)** Carriers MUST be excluded
  from every plain-BTC default and hard-errored when explicitly named:

  | Operation | Carrier handling | Where |
  |---|---|---|
  | `transfer` / `transfer_many` / `ensure_exact_coin` selection | silently excluded from candidates | transfer.rs:46, 153, 261-270 |
  | `split_coin` (named) | hard error | transfer.rs:299-306 (GRN-ERR-7) |
  | `withdraw` default / named | excluded / hard error | wallet.rs:524-547 (GRN-ERR-8) |
  | `unilateral_exit` default / named | excluded / hard error | wallet.rs:723-746 (GRN-ERR-8) |
  | `auto_exit_due` watchtower sweep | carriers **silently skipped** — the watchtower gives token pieces NO deadline protection (GRN-INV-14) | wallet.rs:484-489 |
  | `start_lightning_swap` auto-select | excluded (audit [6]) | lightning.rs |
  | `get_balance` spendable sats | carrier sats excluded; fails CLOSED for token wallets (audit [23]) | wallet.rs:284-299 |

- **GRN-INV-11 (one carrier per transfer; one allocation per carrier)** A token transfer draws on
  a SINGLE carrier holding `>= amount` of the asset (tokens.rs:452-478); allocations spread over
  multiple carriers CANNOT be merged into one payment — the typed failure is GRN-ERR-10 even when
  the wallet's total asset balance suffices (colored combine exists only at lib level,
  `create_colored_combine_tx`, rgb.rs:330, exercised by rgb02/05/08 — not an SDK operation).
  Additionally the SDK opens its rgb-lib wallet with `max_allocations_per_utxo: 1`
  (rust-rgb lib.rs:93), so a carrier holds exactly ONE allocation of ONE contract in these flows;
  multi-asset carriers are out of scope by configuration.
- **Sats-on-carrier are not independently splittable.** The only way sats leave a carrier is
  inside a colored split (piece 1500 / change residual) — GRN-REQ-10 forbids the plain path.
  A sibling change-holder broadcasting the shared branch is benign (byte-identical txs,
  IVL-REQ-13 note), it merely materializes funding early.

## 7. Effect on invalidation and exit

Cross-references made normative for granularity; mechanics are IVL-* and
[research/invalidation-economics.md](research/invalidation-economics.md).

- **GRN-INV-12 (structural consequence of a partial send)** Every partial send consumes its
  parent PERMANENTLY: budget 1 set then consumed (GRN-REQ-7), terminal at the SE, publicly
  queryable (IVL-INV-8/9), irreversible (IVL-INV-7). It mints exactly two (or N+1) depth+1
  sub-coins with FRESH ladders anchored `H_split + initlock` sharing ONE branch
  (INVALIDATION-SPEC §9 "Fresh sub-ladders"); the tree's root deadline
  (`H_deposit + initlock`, exactness caveats per IVL-INV-10 / audit [17]) is UNCHANGED by
  splitting (IVL-INV-14). Depth does not consume ladder capacity; it does grow exit cost.
- **GRN-INV-13 (cost scaling: width beats depth)** k successive partial sends from one coin
  chain the LAST change coin to depth k; exit cost grows exactly **155 vB per level** (measured,
  sdk26; economics §3b) on top of the 112 vB backup. One `transfer_many`/`batch_transfer_tokens`
  fan-out costs ~**60 vB of branch per piece** (sdk26: 3 pieces + change = 241 vB). Payers of
  many parties SHOULD use width (one split) over depth (chained splits).
- **GRN-INV-14 (token exit: what is and is not guaranteed)** For a token piece:
  - **Guaranteed (no SE):** broadcasting the exit branch materializes the colored split txs —
    they ARE the RGB witnesses, their opret commitments confirm with them — and the allocation
    settles as an on-chain RGB holding at the piece outpoint (SPEC INV-16; rgb-lib refresh
    observes the confirmed witness; `sdk29` asserts on-chain settlement of the exact partial
    amount at depth 2). Carriers are excluded/hard-errored from the plain exit paths
    (GRN-REQ-14), so the SDK cannot destroy the allocation on the way out. **The guarantee is
    protocol-level, not one SDK call:** NO shipped SDK operation broadcasts a carrier's branch —
    `unilateral_exit`/`withdraw` refuse the carrier (wallet.rs:726-746, 524-547),
    `broadcast_branch_if_any` is `pub(crate)` (wallet.rs:571), and the `auto_exit_due`
    watchtower silently skips carriers (wallet.rs:484-489) — so the root-deadline discipline of
    IVL-REQ-16 is entirely MANUAL for token pieces. The holder MUST broadcast the stored branch
    rows (`branch-<id>` / recovery bundle) directly — as `sdk29` does with a raw
    transaction-broadcast loop — or rely on a co-descendant's exit materializing the shared
    branch (deep-dive §5.6), and MUST apply the eager-broadcast rule before the tree's root
    deadline on their own.
  - **Exit material:** branch rows + backup + **consignment** (`BackupTx.rgb_consignment` on the
    piece's first backup row, tokens.rs:595-617). All of it lives in the recovery bundle and is
    **NOT seed-derivable** (deep-dive §5.9; wallet.rs:56-60) — a mnemonic-only restore cannot
    exit or prove the token.
  - **NOT guaranteed:** (a) SE-less *onward movement* of the settled allocation — the piece's
    only pre-signed spend is its PLAIN backup (`create_tx1`, transfer.rs:441-449), which is
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
| GRN-ERR-2 | planner cannot mint the remainder despite sufficient balance: remainder < 330, OR no unused coin > remainder + fee_reserve + 330 (both audit [29]; the latter asserted E2E by sdk28 — 4,800 from a single 5,000-sat coin) | same `Insufficient`/ERR-9 refusal | select.rs:103-121; unit `sub_dust_remainder_is_refused` | ERR-9 |
| GRN-ERR-3 | piece + reserve ≥ parent | `piece {p} + fee reserve {f} does not fit in coin of {n} sats` | transfer.rs:615-619 | INV-10 |
| GRN-ERR-4 | sub-dust piece/change (plain split) | `split would create a sub-dust output (piece {p}, change {c}, dust floor 330) — the split tx would be unbroadcastable` | transfer.rs:621-626 | audit [9] |
| GRN-ERR-5 | any split output < 330, or no fee room, at PSBT build | `MercuryError::FeeTooLow` (backstop for transfer_many + colored paths) | lib/src/transaction.rs:346-360 | audit [9] |
| GRN-ERR-6 | no parent big enough | `no confirmed coin large enough for {total} sats + fee` / `no coin large enough to mint {sats} sats` | transfer.rs:160, 277 | — |
| GRN-ERR-7 | plain split of a carrier | `coin {id} carries an RGB token allocation; splitting it as plain BTC would destroy the token — use a token transfer or pick a different coin` | transfer.rs:302-306 | audit [7] |
| GRN-ERR-8 | withdraw / unilateral exit naming a carrier | `coin {id} carries an RGB allocation; withdrawing it as plain BTC would destroy the tokens — move the asset off this coin first` / `…a plain unilateral exit would destroy the tokens — move the asset off this coin first` | wallet.rs:532-536, 731-735 | audit [7] |
| GRN-ERR-9 | carrier sats too small for a token split | `carrier coin too small ({c} sats) for a token split` / batch: `carrier coin too small ({c} sats) for {n} pieces + fee` | tokens.rs:485-489, 723-728 | — |
| GRN-ERR-10 | no single carrier covers the asset amount | `no single coin carries >= {amt} of {asset} (multi-coin token combine not yet wired)` / batch: `no confirmed coin carries >= {total} of {asset} for the batch` | tokens.rs:476-478, 717-719 | — |
| GRN-ERR-11 | consignment/envelope mismatch or invalid consignment at claim | `token consignment assigns {booked} to this coin but the envelope claimed {a} — rejecting` / `incoming token consignment INVALID: {detail}` | tokens.rs:936-955 | ERR-8 |
| GRN-ERR-12 | fulfilling an expired invoice | `invoice expired at {exp} (now {now})` | invoice.rs:85-93 | ERR-11 |
| GRN-ERR-13 | a split output's deposit-token slot is unpaid (SE charges for tokens) | `deposit token payment required: pay {fee_sats} sats to {deposit_address} (token {token_id}), then retry` (`SdkError::TokenPaymentRequired`); raised BEFORE the terminal-guard on every split path (§3 note) | types.rs:63-68; take_token call sites transfer.rs:315-330, tokens.rs:494-509, 736-755 | — |

## 9. Invoices

- **GRN-REQ-15 (invoice amount units — refines SPEC REQ-28)** `SparkInvoice.amount` is `u64` and
  its unit is selected by `asset_id`: **sats** when `None`, **raw token units** when
  `Some(contract_id)` (invoice.rs:14-26). `fulfill_spark_invoice` MUST check expiry
  (GRN-ERR-12 = ERR-11) and route to `transfer` (sats) or `transfer_tokens` (raw units)
  accordingly (invoice.rs:83-98) — inheriting every guarantee and error of §2-§4.
- *Non-normative:* on RGB Lightning legs the SSP enforces asset value at claim time (the
  GRN-REQ-13 consignment validation) plus a post-claim balance-delta gate before reporting the
  preimage (ssp.rs:390-430, 469-484; audit [3][4] closed).

## 10. Traceability

Tests marked **(new)** were added in the granularity test pass (E2E slots 28/29 + unit
`granularity_model.rs`).

| Item(s) | Verifying test(s) |
|---|---|
| GRN-REQ-4/5/6, GRN-ERR-1/2 | sdk01 (exact subset + split + sub-dust refusal); `unit::select`; **sdk28_granularity_sats (new)** — exact-subset and split paths, plus the no-admissible-split-candidate refusal (4,800 from a single 5,000-sat coin, typed error, coin untouched) |
| GRN-INV-1/1b/2, GRN-ERR-3/4/5 | `unit::split_math` (transfer.rs:630-647); **unit `granularity_model.rs` (new)** — boundary matrix over the real `split_amounts`/`split_fee_reserve`/`select::plan` (dust floor 330, min parent 960 ± 1, reserve-clamp interactions; the planner-side 961 pin is `unit::invalidation_model::split_size_floor`); **sdk28 (new)** — E2E: a 330-sat piece is refused (backup FeeTooLow) and the TRUE minimum mintable piece `330 + backup_fee` (measured 442 at 1 sat/vB, GRN-INV-1b) mints cleanly |
| GRN-REQ-7 (terminal ordering) | sdk08 (terminal node); SPEC REQ-18 rows |
| GRN-REQ-8 (transfer_many), GRN-INV-13 | sdk11 (multi-recipient); sdk26 (width vs depth, measured 241 vB / 155 vB per level) |
| GRN-REQ-9 | sdk05/sdk06 (LN swaps mint exact coins via `ensure_exact_coin`) |
| GRN-REQ-10/14, GRN-ERR-7/8 | audit [6][7] fix verification; **sdk29_granularity_tokens (new)** — spent-carrier change is plain BTC (surfaces in `available_sats`; plain `split_coin` succeeds — GRN-INV-10's transfer/withdraw claim rides the shared predicate, not a direct assert) |
| GRN-REQ-11, GRN-INV-4/5/6, GRN-ERR-9 | sdk02 (250/750 token split + envelope checks); rgb01 (750/250 colored); **unit `granularity_model.rs::token_split_bounds_model` (new)** — token min-carrier boundary (2130 ± 1) over the real `split_amounts`; **sdk29 (new)** — 1-raw-unit send + the typed GRN-ERR-9 refusal on a 1,500-sat received piece (fit-guard boundary 1,800, not 2,130) |
| GRN-REQ-12 (batch) | sdk09 (IFA mint + batch 200/300) |
| GRN-INV-7 | rgb01/rgb03 (output_vouts over OP_RETURN); SPEC INV-11 rows |
| GRN-INV-9 | sdk02/sdk09 (conservation); chaos sdk22 oracle (INV-13) |
| GRN-REQ-13, GRN-ERR-11 | sdk02, sdk09, rgb13 (consignment integrity / ERR-8); `unit::envelope` (tokens.rs:967-993) |
| GRN-INV-10/11, GRN-ERR-10 | rgb03/rgb06 (chained colored splits); **sdk29 (new)** — single-carrier insufficiency: carriers 60+50, pay 100 ⇒ typed error |
| GRN-INV-12/13 | sdk26 (depth/width scaling, measured); INVALIDATION-SPEC §10 rows |
| GRN-INV-14 | rgb01-06 (materialized colored exits, lib level); **sdk29 (new)** — token exit at depth 2 with on-chain settlement of the exact partial amount (branch broadcast performed manually via raw txs, per the GRN-INV-14 manual-broadcast note) |
| GRN-REQ-15, GRN-ERR-12 | sdk11; `unit::invoice::tests` |
| GRN-REQ-2 (precision metadata) | rgb14 (precision/metadata); **unit `granularity_model.rs::raw_units_precision_model` (new)** — raw-unit/precision arithmetic up to 10^18; **sdk29 (new)** — precision-2 issuance + 1-raw-unit send (the SDK never scales) |

Unit module: `clients/libs/rust-sdk/src/granularity_model.rs` **(new)** mirrors
`invalidation_model.rs` — it calls the real admission functions, so a guard change fails the
executable spec until this document is updated.

## 11. Known limitations

None of these may be omitted when citing this spec.

1. **One carrier per transfer / no SDK combine.** A payment cannot merge allocations across
   carriers (GRN-INV-11); colored combine is a lib-level primitive only (rgb02/05/08). Roadmap:
   wire combine into the SDK, after which GRN-ERR-10 becomes reachable only on true insufficiency.
2. **One allocation per carrier UTXO.** By SDK configuration (`max_allocations_per_utxo: 1`,
   rust-rgb lib.rs:93) — an rgb-lib wallet option, not an RGB protocol limit.
3. **Fixed `TOKEN_BLINDING = 777`** (GRN-INV-8): a design simplification, safe only because
   consignments travel owner-encrypted; not a privacy feature.
4. **u32 sats cap** (~42.9 BTC/coin; GRN-REQ-1, SPEC §14).
5. **Sats on a carrier are not independently splittable** (§6); they move only inside colored
   splits, and the packaging sats' recovery can be uneconomic at high feerates (GRN-INV-14).
6. **Precision is immutable post-issuance** and purely display-level; a precision-0 asset can
   never be sent in fractions (GRN-REQ-2).
7. **Late dust floor on `transfer_many` and colored paths** (GRN-REQ-8 note, GRN-INV-6): a
   boundary-violating attempt fails after the terminal-guard, pinning the parent to one remaining
   co-signature (recoverable, never bricked).
8. **No SDK colored exit — and no SDK call broadcasts a carrier's branch at all**: SE-less onward
   transfer of a settled token allocation is not possible today; the pre-signed backup of a piece
   is plain BTC and allocation-destroying if misused (GRN-INV-14; audit [7]). Materializing a
   token piece's branch is likewise not a shipped SDK operation — the plain exit paths refuse
   carriers and `auto_exit_due` skips them, so the holder broadcasts the stored branch rows
   (recovery bundle / `branch-<id>`) directly, as sdk29 does, or relies on a co-descendant's exit
   (GRN-INV-14 manual-broadcast note; IVL-REQ-16 discipline is manual for token pieces).
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
    is a no-op when `rgb_data_dir`/`rgb_proxy_url` are unset (tokens.rs:884-886): the piece books
    as plain 1,500-sat BTC, no carrier guard applies (empty carrier set), and the allocation is
    destroyable by the receiver's ordinary BTC operations. Senders cannot detect the receiver's
    configuration from the address (GRN-REQ-13 hazard note). The consignment survives in the
    backup rows; recovery after late reconfiguration is untested.
12. **Planner conservatism; multi-coin non-atomicity.** `plan()` is greedy and non-backtracking —
    some fundable targets are refused, and the planner is one sat stricter than the executor at
    every boundary (GRN-REQ-5 note; economics §2). Multi-coin plans are handed over sequentially,
    not atomically (GRN-REQ-4 note; SPEC §14).
13. **Backup-fee floor is unguarded — a sub-viable piece strands its parent (GRN-INV-1b).** The
    split admission checks only the 330-sat dust floor, not that each sub-coin can fund its own
    backup (`330 + ceil(112·fee_rate)` ≈ 442 sats at 1 sat/vB). A piece in `[330, 442)` passes
    admission, the parent is made terminal, the split is co-signed, and only then does the piece
    backup fail (`FeeTooLow`) — stranding the parent to unilateral-exit-only (no theft; funds
    recoverable via the parent's own backup). Reachable via `transfer` for small payments. Fix:
    raise the admission floor to `330 + backup_fee` on every split path, before the terminal-guard.
    Measured by sdk28 (min mintable = 442); flagged for fix.
14. **A wallet cannot receive the SAME asset twice.** `accept_incoming_tokens` re-imports the
    asset genesis on every receive (tokens.rs:956-960 → `import_asset_offchain` →
    `save_new_asset`), so a wallet that already holds asset `X` fails to book a second, separate
    allocation of `X` (`UNIQUE constraint failed: asset.id`), and the retriable-booking path
    (wallet.rs:430-442) retries this PERMANENT error every claim, spinning the watcher. "Receive
    the same token twice" is a normal flow. Fix: make `save_new_asset` idempotent on an
    already-known asset (skip the metadata insert; the runtime contract import + allocation
    registration still run) and classify the constraint error as terminal. Until fixed, each
    receiver must be first-sight per asset (sdk29 routes each receive to a distinct wallet).
