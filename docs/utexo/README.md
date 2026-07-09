# Spark-parity on Mercury + RGB — documentation

Full Spark (buildonspark) feature parity on Mercury Layer with a **single statechain entity**
(blind-MuSig2 2-of-2, no FROST multi-operator) and **RGB** as the token standard.

## Learn

- [TL;DR](learn/tldr.md) — what this is in one page
- [Core concepts](learn/core-concepts.md) — SE, coins & sub-coins (trees & leaves), transfers, exits
- [Trust model](learn/trust-model.md) — what you trust and why exits never need permission; the full party-by-party matrix incl. watchtowers/indexers and the irreducible boundaries: [TRUST-MODEL.md](TRUST-MODEL.md)
- [Transfers](learn/transfers.md) — key handover, the exact-amount maker, branch verification
- [Tokens on RGB](learn/tokens.md) — issuance, lifecycle, why there is no freeze
- [Lightning](learn/lightning.md) — preimage swaps on the Mercury latch
- [Deposits & exits](learn/exits.md) — cooperative vs unilateral, the timelock ladder
- [Invalidation & granularity](learn/invalidation.md) — Spark/Ark/Second/SuperScalar comparison, terminal nodes, measured exit costs
- [Partial amounts (granularity)](learn/granularity-deep-dive.md) — paying 0.1 out of 1, token packaging, the floors; normative: [GRANULARITY-SPEC.md](GRANULARITY-SPEC.md), pricing: [research/granularity-economics.md](research/granularity-economics.md)

## Build

- [Getting started](build/getting-started.md) — stack + a wallet in 30 lines
- [Wallet SDK guide](build/wallet-sdk.md) — every operation, with code
- [Issuer SDK guide](build/issuer-sdk.md) — launch and distribute a token
- [API reference](build/api-reference.md) — `UtexoWallet` method-by-method
- [Testing guide](build/testing-guide.md) — E2E suites, adversarial coverage map

## Design & status

- [PARITY.md](PARITY.md) — Spark ↔ Mercury+RGB feature matrix with statuses
- [PLAN.md](PLAN.md) — the build plan and key design decisions
- [research/](research/) — condensed notes from the Spark protocol/SDK study
