# Running the RGB ⇄ statechain E2E on a local regtest

This runs the full RGB-over-statechain lifecycle (issuance → deposit → transfer → withdraw) against
the **rgb-lightning-node** regtest (bitcoind + electrs:50001 + RGB proxy:3000) with the Mercury Layer
server stack on top.

## Prerequisites

- Docker + `docker compose`, and Rust (stable).
- Three repos checked out **side by side** (the `mercury-rgb` crate uses a path dependency on
  `../../../../utexo-rgb-lib`, so the rgb-lib fork must sit next to `mercurylayer`):

```
workdir/
├── mercurylayer/        # gofman8/mercurylayer @ feat/rgb-statechain
├── utexo-rgb-lib/       # gofman8/rgb-lib       @ feat/statechain   (path-dep target)
└── rgb-lightning-node/  # UTEXO-Protocol/rgb-lightning-node
```

```bash
git clone -b feat/rgb-statechain https://github.com/gofman8/mercurylayer
git clone -b feat/statechain     https://github.com/gofman8/rgb-lib utexo-rgb-lib
git clone                        https://github.com/UTEXO-Protocol/rgb-lightning-node
```

> If you prefer the git dependency over the sibling checkout, edit
> `mercurylayer/clients/libs/rust-rgb/Cargo.toml` to use the commented-out `rgb-lib = { git = … }` line.

## 1. Start the regtest (bitcoind + electrs:50001 + proxy:3000)

```bash
cd rgb-lightning-node
./regtest.sh start          # creates the "miner" wallet and mines 103 blocks
```

## 2. Start the Mercury server stack (lockbox SE — no bitcoind/electrs; we reuse the regtest's)

Use the **lockbox** key-manager backend (`docker-compose-lockbox.yml`), not the SGX enclave: it builds
natively on Apple Silicon (`ubuntu:22.04` + vcpkg) and exposes the same SE API on `:18080`. The
`web` service is removed from that compose (it collided with the proxy on `:3000`).

```bash
cd ../mercurylayer
docker compose -f docker-compose-lockbox.yml up -d --build \
    vault vault-init db_lockbox db_server lockbox mercury-server
# wait until the mercury server answers on :8000
curl -s http://127.0.0.1:8000/info/config        # {"initlock":1000,"interval":10,...}
```

> The mercury-server image pins Rust 1.83 (`rust-toolchain.toml`) and builds with the committed
> `Cargo.lock`. If a transitive dep requires `edition2024`, build the server image with the upstream
> (pre-RGB) `Cargo.lock` — the server/lib crates are unchanged by this integration. The RGB client
> crates (below) need Rust ≥1.85 because `rgb-lib` is edition 2024.

## 3. Run the lifecycle test

```bash
cd clients/tests/rust
export ML_NETWORK=regtest                                   # selects regtest.Settings (statechain_entity :8000, electrum :50001)
export RGB_E2E=1                                            # run only rgb01_full_lifecycle
export COMPOSE_FILE="$(cd ../../../../rgb-lightning-node && pwd)/compose.yaml"  # so regtest.sh resolves from any CWD
export COMPOSE_PROJECT_NAME=rgb-lightning-node
export RLN_REGTEST="$(cd ../../../../rgb-lightning-node && pwd)/regtest.sh"     # drive bitcoind via regtest.sh
export RLN_BITCOIND_CONTAINER="$(docker ps --format '{{.Names}}' | grep bitcoind | head -1)"
cargo +stable run     # +stable: rgb-lib needs Rust >= 1.85 (edition 2024); the dir pins 1.83
```

Actual passing output:

```
RGB01 - issued assets <contract_a> (path A) and <contract_b> (path B), 1000 units each
RGB01 - deposit funding tx <txid> (statechain UTXO at vout 1)
RGB01 - deposit A confirmed, statechain_id <id>
RGB01 - built colored backup tx <txid> for transfer
RGB01 - receiver validated transfer consignment (1000 units)
RGB01 - unilateral exit: backup tx <txid> broadcast and confirmed (transfer finalized on-chain)
RGB01 - deposit funding tx <txid> (statechain UTXO at vout 1)
RGB01 - deposit B confirmed, statechain_id <id>
RGB01 - cooperative withdrawal: tx <txid> broadcast and confirmed
RGB01 - RGB statechain lifecycle complete (issuance, deposit, transfer, unilateral + cooperative withdraw)
```

The test uses two issuer wallets (one per path) so each does a single clean deposit. rgb-lib's
synchronous calls run via `tokio::task::block_in_place` (the runtime is multi-threaded) to avoid
nesting a blocking reqwest runtime inside the async runtime.

## What the test exercises

| Phase | How |
|---|---|
| Issuance | `RgbWallet::issue_nia` in the sender's rgb-lib wallet |
| Deposit  | `fund_statechain` builds/colors/signs the funding tx paying the Mercury aggregated address (OP_RETURN opret), broadcast via electrs; Mercury sees the coin |
| Transfer | `mercuryrustlib::rgb::create_colored_backup_tx` colors + blind-MuSig2-signs the backup tx; receiver validates the in-band consignment (unconfirmed witness, LN-style) |
| Withdraw (cooperative) | colored withdrawal tx co-signed with the SE and broadcast → asset moves on-chain |
| Withdraw (unilateral)  | instead of cooperating, broadcast the latest colored backup tx after its timelock |

## Troubleshooting

- **`No container found` / wrong bitcoind**: set `RLN_BITCOIND_CONTAINER` to the exact name from
  `docker ps` (compose names it `<dir>-bitcoind-1`, usually `rgb-lightning-node-bitcoind-1`).
- **electrum connection refused**: ensure `./regtest.sh start` finished (`electrs … finished full compaction`).
- **proxy**: rgb01 relays consignments in-band; the proxy (`:3000`) is only used by rgb-lib for
  transport-endpoint validation while going online.
- **token/deposit fails**: check `docker compose logs token-server` — the deposit token flow needs the
  token server up; for pure-regtest you can also wire a stub token.
- Paste the failing output and I'll iterate on the specific call.
