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
| RLN-1 harness: RLN API survey + 2-node regtest LN setup + BOLT11 smoke | **in progress** | |
| RLN-2 SSP service (swap-out = pay invoice, swap-in = receive) + SDK client + E2E both directions | pending | SDK_E2E=5 (pay), SDK_E2E=6 (receive) |
| RLN-3 practical unilateral exit: sdk07 E2E (mine past locktime) + estimate_exit_cost + docs | pending | |
| RLN-4 invalidation design doc (Spark/Ark/Second/SuperScalar) + single-use on tree nodes + epoch on leaves | pending | |

## How to resume

1. Read this file + docs/spark/PLAN.md + PARITY.md.
2. Stack: `rgb-lightning-node/regtest.sh start` + `docker compose -f docker-compose-lockbox.yml up -d`
   (colima: `export DOCKER_HOST=unix://$HOME/.colima/default/docker.sock`, binaries in `~/bin`).
3. Suites from `clients/tests/rust`: `ML_NETWORK=regtest [SDK_E2E=n|RGB_E2E=n] cargo +stable run`.
4. Server code changes deploy via docker cp + in-container touch + restart (compose build caches).
