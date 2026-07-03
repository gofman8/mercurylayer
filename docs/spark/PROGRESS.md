# Progress tracker — Spark parity on Mercury + RGB

Working file: updated at every push so work is resumable at any point.
Branches: mercurylayer `feat/spark` · rgb-lib `feat/spark` (both on gofman8/UTEXO-Protocol).

## Done (verified green, pushed)

| Phase | Commit | Proof |
|---|---|---|
| P0 plan + parity matrix + research notes | 5533b34 | docs/spark/{PLAN,PARITY,research/} |
| P1 primitives port (mercurylib colored signing, rgb.rs, bridge, server single_use+epoch, rgb-lib minimal) | 508ced8 | RGB_E2E=1,2 |
| P2 Rust SDK (`mercury-spark-sdk`): frictionless wallet, off-chain split amount-maker, **branch-carrying transfers** | 1cfbcd3 | SDK_E2E=1 + upstream suite |
| P3 tokens on RGB (issue/balances/off-chain transfer, verified-contract booking) | 61c5e84 | SDK_E2E=2 |
| P4 lightning latch swap legs | eebc570 | SDK_E2E=3 |
| P6 docs suite (learn/ + build/) | 2e4d280 | docs/spark/ |
| P7 adversarial SDK suite | 975818c | SDK_E2E=4 |
| P5 nodejs binding (`mercury-spark-sdkd` daemon + @mercury/spark-sdk) | 769fd34 | stdio protocol driven live |
| P8 final verification 13/13 | 439e2f4 | SDK1-4 + RGB1-8 + upstream in one pass |

## In progress — RLN + SSP directive (2026-07-03)

Requirements: real Lightning flows over UTEXO-Protocol/rgb-lightning-node (both directions,
tested on regtest); an actual **SSP service**; practical unilateral exit (tests + cost
calculation); old-state invalidation Spark-grade or better (review Ark/Second/SuperScalar);
single-use+epoch inside trees, decrementing backups on flat coins.

| Step | Status | Notes |
|---|---|---|
| RLN-1 harness: RLN API survey + 2-node regtest LN setup + BOLT11 smoke | **DONE** | rln.rs harness; LN_SMOKE=1 green (real channel + payment). RLN bin: build UTEXO fork w/ `git submodule update --init` first. Hodl invoices: /lninvoice payment_hash param + /claimhodlinvoice. Latch facts: preimage release gated on locked=false (confirm-first) ✓ receive-flow atomic as-is; batch EXPIRY kills the claim (no auto-unlock) → pay-flow needs SE unlock-by-preimage extension. |
| RLN-2 SSP service + both LN directions | **DONE** | SE latch v2 (migration 0004: external payment_hash + /transfer/paymenthash/external + /transfer/unlock/preimage — unlock by presenting the LN preimage); SspService (sdk ssp.rs) + mercury-ssp HTTP bin; SDK pay_lightning_invoice / create_lightning_invoice; SDK_E2E=5 GREEN (Mercury→LN: coin latched to invoice hash, SSP pays over real channel, preimage unlocks coin + proves payment), SDK_E2E=6 GREEN (LN→Mercury: zero-on-chain wallet receives real LN payment as a coin; preimage release gated on coin release). Deploy gotcha: crashed container → docker cp works while stopped, exec/touch needs it running; recreate resets code (recopy ALL changed files incl. lib/). |
| RLN-3 practical unilateral exit | **DONE** | estimate_exit_cost(coin) -> {branch_txs, vbytes, fee_sats_at(rate), wait_blocks}; unilateral_exit returns per-coin ExitStatus (branch instant, backup waits locktime) instead of erroring non-final. SDK_E2E=7 GREEN: sub-coin exit = 2 txs, 267 vB, 534 sats @2sat/vB, 990-block wait mined through; zero SE involvement. Ladder facts: create_tx1 qt=0 -> h+initlock fresh ladder per coin (deposits AND sub-coins); transfers decrement by interval; branch txs locktime-free. |
| RLN-4 invalidation design doc (Spark/Ark/Second/SuperScalar) + single-use on tree nodes + epoch on leaves | pending | |

## How to resume

1. Read this file + docs/spark/PLAN.md + PARITY.md.
2. Stack: `rgb-lightning-node/regtest.sh start` + `docker compose -f docker-compose-lockbox.yml up -d`
   (colima: `export DOCKER_HOST=unix://$HOME/.colima/default/docker.sock`, binaries in `~/bin`).
3. Suites from `clients/tests/rust`: `ML_NETWORK=regtest [SDK_E2E=n|RGB_E2E=n] cargo +stable run`.
4. Server code changes deploy via docker cp + in-container touch + restart (compose build caches).
