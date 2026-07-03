# Build plan: Spark parity on Mercury (single SE) + RGB

Companion to [PARITY.md](PARITY.md). Branches: mercurylayer `feat/spark` (from `dev`),
UTEXO-Protocol/rgb-lib `feat/spark` (from `dev`, only absolutely-necessary changes).

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

**P2 — `mercury-spark-sdk` (Rust core).** New crate `clients/libs/rust-sdk`:
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

**P5 — bindings.** nodejs (clients/libs/nodejs pattern) + web/wasm exposure of the SDK surface.

**P6 — docs.** The sitemap in PARITY.md, mirroring docs.spark.money style (concept pages short,
build pages code-first with expected output).

**P7 — tests.** Three tracks (spark-mirror, rgb-lib-mirror, e2e-over-spark) runnable against the
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
3. **RGB freeze is N/A by design.** Client-side validation means an issuer freeze list has no
   consensus meaning; we document this as a trust-model difference, not a gap to fake.
4. **Events are poll-driven.** Mercury has no push stream; the SDK emits the same event set from a
   poller so app code is portable from Spark.
