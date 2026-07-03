# Testing guide

All suites live in `clients/tests/rust` and run against the local regtest stack.

## Stack

```bash
cd rgb-lightning-node && ./regtest.sh start                       # bitcoind + electrs + RGB proxy
cd mercurylayer && docker compose -f docker-compose-lockbox.yml up -d   # SE + lockbox + token server
```

Environment (from `clients/tests/rust`):

```bash
export ML_NETWORK=regtest
export RLN_BITCOIND_CONTAINER=rgb-lightning-node-bitcoind-1   # for the test faucet
```

> The tests crate pins an old toolchain in `rust-toolchain`; run with `cargo +stable run`
> (rgb-lib is edition-2024). Each flow wipes `wallet.db*` and its rgb data dirs at start.

## Suites

### SDK E2E (`SDK_E2E=n cargo +stable run`)

| n | Flow | Proves |
|---|---|---|
| 1 | `sdk01_wallet_flow` | deposit → exact-subset transfer → auto-claim → **off-chain-split transfer** (branch-verified) → auto-claim → cooperative exit |
| 2 | `sdk02_token_flow` | issue 1000 TKN → **off-chain token transfer** (consignment in-message, verified contract) → balances 750/250 → exit |
| 3 | `sdk03_lightning_swap` | latch swap: hash verified with SE, claim locked pre-settlement, preimage settles the hash |

### Off-chain DAG primitives (`RGB_E2E=1..8`)

The low-level suite under the SDK: off-chain split (1), 2-input combine (2), 2-deep un-broadcast
chain (3), SE single-use refusal (4), 3-input combine (5), 3-level DAG (6), epoch deadline (7),
wide combine (8).

### Upstream Mercury suite (default `cargo +stable run`)

The vanilla protocol tests (tb01–tb05, tm01, ta01–ta03, tv01): simple transfer, address reuse,
atomic transfer, **lightning latch**, timelock, sender-double-spend, deposit edge cases. Run this
after any change to transfer/receiver code — the branch-transfer extension must stay
regression-clean against it.

### SDK unit tests

```bash
cargo +stable test -p mercury-spark-sdk   # coin selection, config, doctest
```

## Adversarial coverage map (Spark-mirror)

| Spark test theme | Covered by |
|---|---|
| double-claim / duplicate leaf | ta02, ta03 (duplicate deposits), tm01 (sender double-spend) |
| conflicting off-chain spend | RGB_E2E=4 (SE single-use refusal) |
| wrong preimage / locked claim | SDK_E2E=3 (claim locked until settle; hash must match), tb04 |
| transfer interrupt / resume | tb01+tb02 paths; claim() is idempotent per message |
| exit-race ordering | timelock ladder (tb05) + branch-first materialization (SDK exits) |
| invalid consignment | receiver hook rejects (validate_offchain_chain) — sdk02 asserts the valid path; invalid-path unit lives in rgb-lib fork tests |
