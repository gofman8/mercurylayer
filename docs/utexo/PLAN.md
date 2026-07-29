# Build plan: Utexo (Spark-compatible API) on Mercury (single SE) + RGB

Companion to [PARITY.md](PARITY.md). Branches: mercurylayer `feat/spark` (from `dev`),
UTEXO-Protocol/rgb-lib `feat/spark` (from `dev`, only absolutely-necessary changes).

> **Status (2026-07-29) — the plan is done; the shipped protocol is TES-R, and it is the only one.**
> All phases below landed on `feat/spark`. `deposit_protocol_version` and the `UTEXO_PROTOCOL_DEFAULT`
> env are **deleted**: `claim()` establishes a ladder (trigger T → extension X_m → state S, relative
> CSV, all un-broadcast) for every fresh confirmed root coin, unconditionally. One protocol, two coin
> **shapes**, both current:
> - **Laddered** — every plain deposit. Idle coins never age, renewal is off-chain and unbounded.
> - **Un-laddered** — an RGB **carrier** is deliberately never laddered (a plain tier spend would
>   destroy the allocation; terminal-freeze, [PROTOCOL.md](PROTOCOL.md) §5.10, sdk52), and a split
>   sub-coin whose funding is un-broadcast cannot root a trigger [B0]. These keep the signed-once
>   backup and transfer by backup-chain handover. This path is load-bearing for RGB tokens, not
>   legacy and not dead code.
>
> Received split children are **first-class** ([CHILDREN.md](CHILDREN.md)): the
> claim completes the standard SE key handover (the receiver co-owns A_child across the rotation, so
> the pre-signed exit chain stays valid, and the sender is permanently locked out), and a child is
> paid onward off-chain WHOLE (`child_retransfer`) or SPLIT (`child_in_ladder_pay`, a depth-2
> `ancestors` chain). Each hop costs one co-signature and discloses one superseded state, which the
> receiver's census counts and proves out-raced — sdk60 (alice→bob→carol, funding outpoint unspent
> throughout) and sdk17 (partial second hop).
>
> Everything below is kept as the historical record of the build. Where a decision or a backlog item
> was superseded or has since landed, that is marked inline; the original reasoning is untouched.

## Ground rules

- **Single SE**: Mercury's blind-MuSig2 2-of-2 (server + lockbox). Everything Spark does with FROST
  rounds, consensus, gossip, or VSS-across-operators collapses to one sign_first/sign_second exchange.
- **RGB as the token layer**: assets are rgb-lib contracts; allocations ride statechain coins and
  off-chain sub-coins. No RGB protocol changes; rgb-lib changes minimal and in the fork branch only.
- **Frictionless UX is the product**: the SDK owns every complexity — sats/fee funding of colorable
  UTXOs, allocation placement, consignment exchange, off-chain validation, coin selection,
  split/combine to exact amounts, claim polling, backup/exit management. A user calls
  `transfer(address, amount)` and nothing else.
- Push both repos after every phase (workflow rule).

## Phases

**P0 — study + plan (this doc).** Four research passes over the Spark monorepo distilled into
`research/`; parity matrix in PARITY.md.

**P1 — primitives port.** From `feat/rgb-statechain` (proven green there as rgb01–08):
- mercurylib: colored-tx MuSig2 signing single+multi-input (`get_partial_sig_request_for_colored_tx[_multi]`),
  split/combine PSBT builders, multi-witness backup builder, `Coin.single_use` + `epoch_deadline`.
- mercuryrustlib: `rgb.rs` orchestration (`create_colored_split_tx`, `create_colored_combine_tx`),
  deposit helpers (single-use / epoch variants), coin-status backup skip for single-use.
- `mercury-rgb` bridge crate (rust-rgb): fund/register/color/validate-offchain wrappers.
- server: migrations 0002_single_use + 0003_epoch_deadline, `sign_first` refusal gates.
- rgb-lib `feat/spark`: minimal port of `rust_only.rs` statechain primitives + OffchainResolver
  multi-witness + `validate_consignment_offchain_chain` (adapted to current dev).
- Verify: compile everywhere; smoke E2E (one split + one combine) against the regtest stack.

**P2 — `mercury-utexo-sdk` (Rust core).** New crate `clients/libs/rust-sdk`:
- `Wallet::initialize(config|mnemonic)`, identity keys, bech32m address encode/decode (+ invoice fields).
- Balance engine: BTC (coin states) + tokens (rgb-lib per-asset over registered coins).
- Deposit: `get_deposit_address()` (+ static-slot re-issue), background **auto-claim** (poll →
  confirm → activate), duplicate detection.
- **Amount-maker**: given a target (sats or asset units), plan combine/split over owned coins to
  produce the exact payment; execute off-chain (SE co-signed, un-broadcast).
- Transfer send/receive: Mercury key-handover + RGB consignment (blinded or witness), off-chain
  chain validation on receive; activity log.
- Exit: `withdraw(address)` cooperative; `unilateral_exit()` = broadcast backup/branch; RGB settle.
- Event emitter (poll-driven) for DepositConfirmed / TransferClaimed / BalanceUpdate.

**P3 — `mercury-issuer-sdk`.** Thin layer over the SDK + rgb-lib: create_token (NIA/IFA),
mint (IFA inflate), burn (IFA), balances/metadata/distribution; freeze documented N/A.

**P4 — Lightning.** Wrap Mercury lightning-latch endpoints as `pay_lightning_invoice` /
`create_lightning_invoice` legs (LSP counterparty = SSP role); E2E for the latch protocol legs.
*Shipped as the HODL-invoice latch, working both directions on the ladder
([LIGHTNING.md](LIGHTNING.md)): sdk63 / sdk64 / sdk67 cover the core legs, sdk65 the non-exact
amount, sdk66 / sdk68 the failure-and-reclaim paths. The LN-latched piece is the one case that stays
terminalized — it sits unclaimed past the pending-transfer lock's window. Fixed along the way: the
one-call PAY API minted via `ensure_exact_coin` and therefore refused every laddered coin (D3).*

**P5 — bindings.** nodejs (clients/libs/nodejs pattern) + web/wasm exposure of the SDK surface.

**P6 — docs.** The sitemap in PARITY.md, mirroring docs.spark.money style (concept pages short,
build pages code-first with expected output).

**P7 — tests.** Three tracks (utexo-mirror, rgb-lib-mirror, e2e-over-utexo) runnable against the
local regtest+lockbox stack; documented runner.

**P8 — hardening + final parity report.**

## Key design decisions (made in P0)

1. **No SSP service.** Spark needs SSPs for denomination swaps, LN gateway, and coop-exit connectors.
   With multi-input combine + split we mint exact amounts natively; coop exit is a direct SE co-sign;
   LN uses the latch with any LND-running counterparty. The SSP *role* reduces to "LN counterparty".
2. **Single-use + epoch replaces timelock-decrement inside trees.** Chained sub-coins can't race
   (child spends parent), so the SE's one-spend-per-node rule + epoch deadline give the same
   guarantee Spark gets from decrementing timelocks + renewal, without renewal churn. Flat coins
   keep Mercury's native decrementing backups.
   > *Superseded for the laddered shape.* TES-R replaced the decrementing backup on plain deposits
   > with the trigger/extension/state ladder: idle coins never age (0 vB rent) and renewal is
   > off-chain and unbounded (sdk43). The reasoning still holds where it was written for: the
   > **un-laddered** shape — RGB carriers and un-broadcast split sub-coins — still runs on chained
   > sub-coins that can't race plus the one-spend-per-node rule and the deadline. See
   > [PROTOCOL.md](PROTOCOL.md).
3. **RGB freeze is N/A by design.** Client-side validation means an issuer freeze list has no
   consensus meaning; we document this as a trust-model difference, not a gap to fake.
4. **Events are poll-driven.** Mercury has no push stream; the SDK emits the same event set from a
   poller so app code is portable from Spark.

## Post-review remediation backlog (2026-07)

Source: the [second adversarial review](REVIEW.md#second-adversarial-review-2026-07-05--full-protocol-production-readiness-pass)
(verdict **NOT production-ready**). **P0 blocks mainnet** and must be fixed + re-reviewed. Status is
tracked here as items land; each P0 gets a regression test.

### P0 — mainnet blockers  ·  ALL LANDED on `feat/spark` (2026-07-05)

| # | Item | Closes | Status |
|---|------|--------|--------|
| P0-1 | **Enclave secnonce single-use.** `lockbox` `load_and_consume_secnonce` atomically `SELECT … FOR UPDATE` + null `sealed_secnonce` in one txn; `generate_partial_signature` refuses (400) when absent. Server: `acquire_signfirst_lock` serializes `sign/first` per `statechain_id` (tx-scoped advisory lock). | C1 (nonce-reuse → key-share leak → theft) + L1 (sign/first race DoS) | ✅ **code done; lockbox needs SGX rebuild+redeploy** |
| P0-2 | **SSP pre-payment gate.** New `GET /transfer/batch_statechains` + `peek_pending_transfers` (decrypt-only): `execute_pay` requires every latched coin addressed to the SSP AND total ≥ invoice+fee before `send_payment`; `claimed_transfers==0` is a hard error; server `unlock_by_preimage` returns 404 on 0 rows. 6 unit tests. | C2 + C3 + M3 | ✅ |
| P0-3 | **Branch/split locktime.** `get_unsigned_split_psbt` sets a non-withdrawal split branch to locktime height 0 (INV-4) — unconditionally broadcastable, always below any deposit-anchored parent backup. Both split paths. mercurylib unit test. | H5 (exit-race inversion) | ✅ |
| P0-4 | **Branch broadcast conflict not swallowed.** `broadcast_branch_if_any` drops `txn-mempool-conflict` from tolerated set, emits `WalletEvent::ExitBranchConflict`, returns a hard error; tolerates only `already`/`in block chain`. | H1 | ✅ |
| P0-5 | **Token-carrier exclusion.** `token_carrier_outpoints()` (rgb-lib allocations) excludes carriers from all four BTC selection paths; `split_coin` hard-refuses a carrier; `compute_balance_excluding` drops carrier sats from BTC buckets. | H2 (token destruction) | ✅ |
| P0-6 | **Backup truth + recovery bundle.** `export_recovery_bundle`/`import_recovery_bundle` (wallet record + all backup/branch-*/parents-* rows + RGB seed); corrected `initialize()` docs + getting-started/wallet-sdk. RGB stash dir still copied separately (embed = follow-up). | H3 | ✅ |

> **Remaining before mainnet:** rebuild + redeploy the SGX lockbox so P0-1's enclave consume takes
> effect; run the full E2E suite (regtest + lockbox + RLN) against these fixes; then re-review.

### P1 — pre-mainnet hardening

| # | Item | Closes |
|---|------|--------|
| P1-1 | Decouple LN-latch batches from the 120 s `batch_timeout`; gate `validate_batch` on the latch's own `expires_at`; refuse `send_payment` without ample batch time. | H4 |
| P1-2 | Make incoming-token booking retriable: each `claim()`, scan CONFIRMED coins with a consignment-bearing backup but no booked allocation and re-run `accept_incoming_tokens` (idempotent). | H6 |
| P1-3 | ✅ `create_tx_out`: `checked_sub` + reject when `input − fee < DUST_LIMIT` (330 P2TR). *(Split/combine-time dust floor ✅ shipped — `min_split_output` = 330 + backup fee, enforced on every split/combine/colored path; `transfer.rs:816`, audit [9].)* *(The in-ladder split carries its own floor: a child funds its OWN two tiers + dust, so admission is gated on `min_child_value` — 1306 sat at 2 sat/vB, from `tier_out_total` / `committed_fee_for_outputs` — not the 442-sat backup-fee floor. Using the old floor admitted a split that terminalized the parent and THEN failed, stranding it; fixed (D1). Sponsored refresh sized its rebate into that same dead window, so it failed after the user had already paid the on-chain fee; fixed (D2).)* | M2 |
| P1-4 | Bind owner auth sigs to `(statechain_id ‖ endpoint_tag ‖ server nonce/expiry)` + mutating params; reject seen-nonce/stale. Prioritize `withdraw`/`complete`/`transfer`/`set_spend_budget`. | M1 |

### P2 — robustness / DX / privacy

| # | Item | Closes |
|---|------|--------|
| P2-1 | ✅ Replaced request-path unwraps in `endpoints/utils.rs` (auth `RowNotFound`/`PoolTimedOut`, `Signature`/`PublicKey`/`XOnlyPublicKey` parse) + `database/utils.rs` (enclave-index pool timeout) with graceful `false`/`None`; raised the SE pool 10→50. **Empirically found by the chaos test (SDK_E2E=22) under concurrent load — these `unwrap()`s panicked the SE worker on pool exhaustion.** | L2 |
| P2-2 | ✅ The watchtower is first-class SDK behavior behind the `auto_exit` config flag (default **on**): `watchtower.rs` computes each off-chain coin's `exit_deadline_block` and auto-broadcasts branch + exit at tip + `auto_exit_margin_blocks` (288); `WalletEvent::ExitDeadlineApproaching` is emitted; the online obligation is documented in [TRUST-MODEL.md](TRUST-MODEL.md). Evidence: **sdk51** (a tower defends a laddered coin against a hostile trigger) and **sdk45** (keyless watch bundle — it carries no key material, and a second independent tower is idempotent). | L7 |
| P2-3 | ✅ `unilateral_exit` distinguishes a legitimately branch-less coin from a missing/corrupt branch — via the coin's own funding txid being on-chain rather than `coin.single_use` (audit [20]) — and returns an explicit, actionable error instead of an opaque "missing inputs". *(Also fixed here: a received split child is routed to a UNILATERAL exit, since its funding `SP.out[j]` is un-broadcast and a cooperative withdraw has no outpoint to spend, but it was then marked `WITHDRAWING` with no withdrawal tx, so `check_withdrawal` errored on every later status poll for the life of the coin (D4). A `WITHDRAWING` coin with neither txid nor withdrawal address is now tracked by its pre-signed exit chain.)* Evidence: **sdk50** (unilateral exit), **sdk58** (11 adversarial in-ladder-split cases). | L5 |
| P2-4 | RGB privacy: per-output random blinding threaded through the split path; prune each recipient's consignment to only its own piece (replace fixed `TOKEN_BLINDING=777` + shared consignment). | L3 |
| P2-5 | DB hygiene: run expired-row DELETE + TTL-GC of unconsumed token / expired external-latch rows; per-IP rate-limit the no-server `get_token` path; document the single authoritative latch clock. | L4, L6 |
