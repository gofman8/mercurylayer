# RGB ⇄ Mercury Layer (statechains) integration

This document describes how RGB assets are issued, transferred and withdrawn while bound to Mercury
Layer **statechain** coins, and how to run the end-to-end regtest test.

## Idea

A statechain coin is a single UTXO (`Tx0`) co-owned via blind MuSig2 between the current owner and
the Statechain Entity (SE). Ownership is transferred **off-chain** by updating the SE key share; the
UTXO itself only ever moves on-chain on withdrawal (cooperative) or via a pre-signed *backup
transaction* (unilateral exit).

RGB binds an asset allocation to a UTXO using a **single-use seal** and commits state transitions to
Bitcoin transactions via a deterministic commitment — here an `OP_RETURN` **opret** output. We make
the statechain UTXO the seal:

- The asset stays bound to the **statechain UTXO** for the whole off-chain life of the coin.
- Every Mercury transfer/withdrawal builds a transaction that spends the statechain UTXO; we *color*
  it so its RGB transition re-assigns the asset to the new owner's output and commits to it with an
  `OP_RETURN`. We do **not** broadcast it during an off-chain transfer.
- The transition becomes on-chain-valid only when a **witness transaction** is broadcast: the
  cooperative withdrawal transaction, or — on a unilateral exit — the latest backup transaction.
- This is exactly how RGB is used over Lightning commitment transactions: the receiver validates the
  consignment against an unconfirmed witness transaction.

Because every transition closes the *same* seal (`Tx0`), conflicting (off-chain) transitions are
fine: only the one whose witness transaction is mined becomes valid, which the statechain timelock /
signature-count rules already guarantee is the latest owner's.

## Components

| Crate / module | Role | Notes |
|---|---|---|
| `rgb-lib` fork ([`UTEXO-Protocol/rgb-lib@feat/statechain`]) | RGB primitives | adds **1** method (below) |
| `clients/libs/rust-rgb` (`mercury-rgb`) | bridge | string-only API; isolates RGB's `bitcoin`/`bdk` from Mercury's `bitcoin 0.30` |
| `lib` (`mercurylib`) | core | colored-tx PSBT + sighash; relaxes single-output checks |
| `clients/libs/rust` (`mercuryrustlib`) | orchestration | `rgb.rs`: build → color → blind-MuSig2-sign |
| `clients/tests/rust` | E2E | `rgb01_full_lifecycle.rs` |

### rgb-lib fork addition (`src/wallet/rust_only.rs`)

Only **one** method is added, because it has no public equivalent:

- `fund_statechain_utxo(address, amount_sat, contract_id, rgb_amount, fee_rate, blinding)` — deposit:
  builds/colors/signs the funding tx that spends a colored UTXO, pays the statechain address and
  assigns the asset to it. (rgb-lib has no public way to send a colored UTXO to an externally-owned
  address — its `send`/`witness_receive` only target the wallet's own invoices.)

Coloring and accepting use **only the public rgb-lib API**, exactly like rgb-lightning-node:

- color: `color_psbt_and_consume` + `ColoringInfo`/`AssetColoringInfo` (done in the `mercury-rgb`
  bridge via a base64-PSBT round-trip — the same `RgbLibPsbt::from_str(psbt.to_string())` trick RLN
  uses).
- accept: the in-band consignment bytes are re-posted to the local RGB proxy (`post_consignment`)
  and validated via `accept_transfer` — no rgb-lib change needed.

### mercurylib additions (`lib/src/transaction.rs`)

- `get_unsigned_backup_psbt(...)` — builds the unsigned backup/withdrawal tx as a base64 PSBT.
- `get_partial_sig_request_for_colored_tx(coin, colored_tx_hex, network)` — computes the taproot
  key-spend sighash over the *colored* tx (committing to the OP_RETURN) and the blind-MuSig2 session.

`lib/src/transfer/receiver.rs` is relaxed to accept exactly one extra zero-value `OP_RETURN`
commitment output (output-count, fee, reconstruction and pay-to-owner checks), backward-compatible
with non-RGB coins. `BackupTx` gains optional `rgb_consignment` / `rgb_blinding` fields (serde
default) so the consignment can ride in-band inside the Mercury transfer message.

## Data flow (transfer / withdrawal)

```
mercurylib.get_unsigned_backup_psbt(coin, …)            # bitcoin 0.30: 1 input (Tx0), 1 P2TR output
        → base64 PSBT
mercury_rgb.color(psbt, contract, {0: amount}, blind)   # rgb-lib: + OP_RETURN opret, assign asset
        → (colored base64 PSBT, base64 consignment)
parse colored PSBT (bitcoin 0.30) → colored unsigned tx hex   # OP_RETURN @0, owner @1
mercurylib.get_partial_sig_request_for_colored_tx(coin, tx)   # sighash over colored tx
SE sign/second → aggregate → mercurylib.new_backup_transaction # witnessed tx
```

The only thing crossing the `mercury-rgb` boundary is strings (base64 PSBTs / consignments, hex
txids), so RGB's newer `bitcoin`/`bdk` stack never clashes with Mercury's pinned `bitcoin 0.30`.

## Running the E2E test (regtest)

```bash
# 1. Start the Mercury stack + RGB proxy (bitcoind, electrs, postgres, enclave SIM, mercury, proxy)
docker compose -f docker-compose-test.yml -f docker-compose-rgb.yml up --build

# 2. Fund bitcoind regtest
cid=$(docker ps -qf "name=mercurylayer-bitcoind-1")
docker exec $cid bitcoin-cli -regtest -rpcuser=user -rpcpassword=pass createwallet miner
addr=$(docker exec $cid bitcoin-cli -regtest -rpcuser=user -rpcpassword=pass getnewaddress)
docker exec $cid bitcoin-cli -regtest -rpcuser=user -rpcpassword=pass generatetoaddress 101 "$addr"

# 3. Run the lifecycle test
cd clients/tests/rust
cargo run --bin rgb-lifecycle   # or call rgb01_full_lifecycle::execute() from main
```

The test issues an asset, deposits a statechain coin bound to that asset, transfers it (validating
the in-band consignment), and performs a cooperative withdrawal. The unilateral path broadcasts the
latest colored backup transaction after its timelock instead of cooperating.
```
