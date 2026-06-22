# RGB on statechains, the same way it works on-chain

This document answers "**would this work?**" (yes) and specifies how to make `rgb-lib` treat
statechain UTXOs exactly like on-chain UTXOs — so transfers consume a coin, change goes back to a
free statechain UTXO, and the only difference from on-chain RGB is that the sats move to the receiver
via the statechain instead of to a miner.

It is grounded in what was verified empirically against `UTEXO-Protocol/rgb-lib@dev` + Mercury Layer
on a local regtest (see `RUN_RGB_E2E.md`, `rgb01_full_lifecycle.rs`, `rgb02_deposit_coop_exit.rs`).

## The core problem (discovered empirically)

There are two ways to put an asset on a statechain UTXO (a MuSig2 aggregate output that no BDK
wallet owns), and each has a fatal gap for a full flow:

| Deposit method | Standard balances update? | Owner can later re-color the statechain UTXO? |
|---|---|---|
| rgb-lib `send` to the statechain address (witness recipient) | **Yes** (`get_asset_balance` 2000→0, a `Send` in `list_transfers`, change UTXO) | **No** — the asset is "sent away"; the owner's stash no longer treats it as spendable |
| low-level `color_psbt_and_consume` on a funding tx | **No** (DB send-accounting bypassed) | **Yes** — the stash keeps the allocation queryable by outpoint |

So with `send` the deposit looks right but you can't transfer/exit; with `color_psbt` you can
transfer/exit but the wallet's books don't move. Neither is "RGB the way it works on-chain."

**Resolution (your design):** make rgb-lib *own* the statechain UTXO — register it as a wallet
colorable UTXO whose existence/confirmation is asserted by the **statechain** rather than the
blockchain. Then it is spendable like any on-chain UTXO: the wallet can build transitions from it,
assign change to other (free) statechain UTXOs, and `get_asset_balance`/`list_unspents` reflect
everything. This is the RGB-over-Lightning model with the statechain playing the role the funding
UTXO plays in a channel.

## The model

```
1. Issuance (on-chain): color a normal wallet UTXO by issuing the asset.
2. Deposit: move sats + asset onto statechain UTXO(s). rgb-lib registers each statechain UTXO as a
   wallet-owned colorable UTXO (existence proven by the SE / Mercury, not the indexer).
3. Transfer (off-chain), per RGB-over-LN:
   a. Receiver creates an RGB invoice whose seal is the statechain UTXO it will own after the
      Mercury key-share update (recipient_id references that outpoint).
   b. Sender builds the backup / unilateral-exit tx that SPENDS the statechain UTXO (effectively
      "to itself" — the coin's bitcoin ownership is updated by Mercury, not by this tx) and adds an
      OP_RETURN committing the RGB state transition that assigns `amount` to the receiver's seal and
      the change back to one of the sender's free statechain UTXOs.
   c. The tx is blind-MuSig2 co-signed with the SE but NOT broadcast (it is the exit escape hatch).
   d. Receiver VALIDATES (offchain) that this pre-signed exit tx carries the correct OP_RETURN
      commitment — otherwise the sender could later broadcast a different tx and keep the asset.
      rgb-lib already has `validate_consignment_offchain` + `OffchainResolver` for exactly this:
      validating a consignment against a witness tx that is not yet on-chain.
4. Exit: the operator (or owner, unilaterally) signs a tx spending the statechain UTXO(s) to
   rgb-lib-controlled outputs + OP_RETURN, finalizing the allocations on-chain. The receiver settles
   it with a standard `refresh`.
```

Each transfer consumes the whole statechain UTXO (its sats are split between the receiver's new coin
and the sender's change coin) — identical to on-chain RGB, except the sats are not burned as fees to
a miner; they move to the receiver through the statechain. This matches your "multiple coins, change
back to free allocation, each transfer consumes a UTXO" requirement.

## What rgb-lib needs (the real, minimal changes)

1. **`register_statechain_utxo(outpoint, sats)`** — insert an *externally-owned* outpoint into
   rgb-lib's `txo` table as an existing wallet UTXO, bypassing the BDK ownership/descriptor check.
   Source of truth for existence = the statechain, not the indexer. Touch points discovered while
   investigating (`get_asset_balance` reads DB allocations on wallet txos; `set_txo` inserts a txo):
   - `DbTxo` has no `colorable` column — colorability is *derived* from whether the txo's script
     belongs to the colored keychain. A statechain UTXO is a MuSig key in neither keychain, so the
     derivation must be extended to treat registered statechain outpoints as colorable.
   - `list_unspents` enumerates **BDK**'s unspents (descriptor-owned) and joins DB allocations; it
     must additionally include DB-registered statechain txos that BDK can't see.
   - allocation mapping (`get_rgb_allocations`) already keys by outpoint, so once the txo is
     registered and the stash holds the allocation (from a color-based deposit), the balance follows.
   So it is a few coordinated touch points, not a one-liner - but all in rgb-lib, no protocol change.

2. **A statechain-aware resolver/indexer** — when validating or syncing, treat a registered
   statechain UTXO as "exists / confirmed" by consulting Mercury (the SE's published key-share list /
   the deposit tx) instead of (or in addition to) the electrum/esplora indexer. rgb-lib already
   abstracts this via `AnyResolver`/`Indexer`; add a `StatechainResolver` wrapper, analogous to the
   existing `OffchainResolver`.

3. **Reuse, no change needed:** `color_psbt_and_consume` (build the transition + OP_RETURN),
   `witness_receive`/blinded invoices (receiver seal), `post_consignment`/`accept_transfer` and
   `validate_consignment_offchain` (receiver validation of the unbroadcast exit tx), `refresh`
   (settle on exit). Mercury side: `get_unsigned_backup_psbt` (build the tx, now with a change
   output) + `get_partial_sig_request_for_colored_tx` (blind-MuSig2 sign the colored tx).

With (1)+(2), the statechain UTXO is a first-class colorable UTXO: `send`/transfer from it assigns
the requested amount to the receiver and the **change to a free statechain UTXO**, `get_asset_balance`
tracks it, and the owner can always re-color it for the exit. That is "RGB the same way it works
on-chain."

## Security (your last point — critical)

The receiver must not accept the transfer until it has verified the sender's **pre-signed unilateral
exit transaction** and confirmed its OP_RETURN equals the RGB commitment that assigns the asset to
the receiver's seal. If the receiver skips this, a malicious sender could co-sign a *different* exit
tx (no/!wrong OP_RETURN) and, on a unilateral exit, keep the asset while the receiver believes it
owns it. The validation primitive is `validate_consignment_offchain(consignment, exit_txid, …)`,
which re-derives the commitment from the consignment and checks it against the exit tx — done before
the witness tx is ever broadcast. This is the statechain analogue of an LN counterparty checking the
commitment transaction before revoking the previous state.

## Status / what is already proven on regtest

- **Deposit** with standard-method visibility: `create_statechain_utxo` (sats-sized, deposits the
  free allocation) makes `get_asset_balance` drop 2000→0 with a `Send` in `list_transfers` and the
  colored UTXO spent + change UTXO created. (`rgb02_deposit_coop_exit.rs`.)
- **Transfer + unilateral/cooperative exit** end-to-end via the low-level color path, with the
  receiver cryptographically validating the consignment and the statechain UTXO spent on-chain.
  (`rgb01_full_lifecycle.rs`.)
- **Gap to close** for full on-chain-parity: items (1) and (2) above — registering statechain UTXOs
  as wallet-owned colorable UTXOs + the statechain resolver — so a single code path does
  deposit→partial-transfer-with-change→exit with standard balances on both sides throughout.
