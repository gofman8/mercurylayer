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

1. **`register_statechain_utxo(...)` — IMPLEMENTED & PROVEN (`rgb04`).** Inserts an
   *externally-owned* statechain outpoint into rgb-lib's `txo` table as an existing wallet UTXO,
   bypassing the BDK ownership/descriptor check, so standard `get_asset_balance` / `list_unspents` /
   `blind_receive` treat it exactly like an on-chain colorable UTXO. What the investigation actually
   found (simpler than feared):
   - `DbTxo` has **no `colorable` column** — every txo in the `txo` table is colorable; BDK's own
     outputs come in as `colorable:false`. So "register = `set_txo`": being in the table *is*
     colorability. No keychain-derivation change needed.
   - `list_unspents` already returns DB txos joined with allocations *plus* BDK internal unspents, so
     a registered statechain txo shows up automatically — no BDK-enumeration change needed.
   - `get_asset_balance`/`list_unspents` read **DB `coloring` rows**, not the stash. A color deposit
     only updates the stash, so registration also synthesizes the settled `Receive`
     coloring/asset_transfer/batch_transfer rows (the same rows issuance writes) to surface the
     allocation, **and marks the consumed on-chain source UTXO `spent`** so the moved allocation is
     not double-counted (`settled()` excludes spent txos). Net: the allocation *moves* from the spent
     on-chain UTXO onto the statechain UTXO with the balance unchanged.
   The whole primitive is ~60 lines in `rust_only.rs`, all rgb-lib, no protocol change.

2. **A statechain-aware resolver/indexer (partially needed).** One real caveat surfaced:
   `reconcile_orphaned_colored_txos` (run only on the user-facing `Wallet::sync`/`refresh` path)
   marks any colored txo that BDK can't see as **spent** — which would clobber a registered
   statechain UTXO. Two options: (a) don't call `Wallet::sync` on a wallet holding registered
   statechain UTXOs (the internal `sync_bdk_and_db_txos` used by `blind_receive`/`list_unspents`
   does *not* reconcile, so balances/invoices are safe — this is what `rgb04` relies on), or
   (b) teach `reconcile` to skip statechain-registered outpoints (consult Mercury for existence),
   the `StatechainResolver` analogous to the existing `OffchainResolver`. (a) is enough today; (b)
   is the clean long-term fix.

3. **The one remaining gap — `color_psbt` beneficiaries.** `color_psbt` builds transition
   beneficiaries **only from `output_map` (witness vouts = new outputs of the tx being colored)**. A
   fully faithful statechain→statechain *blinded* transfer assigns `amount` to the receiver's
   **existing** statechain UTXO (a `BuilderSeal::Concealed` from the `blind_receive` recipient_id)
   and the **change to the sender's existing free statechain UTXO** — both *existing outpoints*, not
   witness vouts. So `color_psbt` needs a small extension to accept blinded/existing-outpoint
   beneficiaries (the logic rgb-lib's `send` already has for normal blinded recipients). The witness
   half — receiver gets the asset on a *new* output of the sender's exit tx — already works today via
   `output_map` (that is exactly `rgb03`'s exit-to-on-chain and the cooperative exit).

4. **Reuse, no change needed:** `color_psbt_and_consume` (build the transition + OP_RETURN),
   `witness_receive` / `blind_receive` (receiver seal — `blind_receive` now picks a statechain UTXO),
   `post_consignment`/`accept_transfer` and `validate_consignment_offchain` (receiver validation of
   the unbroadcast exit tx), `refresh` (settle on exit). Mercury side: `get_unsigned_backup_psbt`
   (build the tx) + `get_partial_sig_request_for_colored_tx` (blind-MuSig2 sign the colored tx).

With (1) (done) + (2a) (done) the statechain UTXO is a first-class colorable UTXO: `get_asset_balance`
tracks it and `blind_receive` makes invoices on it. Adding (3) lets a single transfer assign the
requested amount to the receiver and the **change to a free statechain UTXO**, finishing "RGB the
same way it works on-chain."

## Security (your last point — critical)

The receiver must not accept the transfer until it has verified the sender's **pre-signed unilateral
exit transaction** and confirmed its OP_RETURN equals the RGB commitment that assigns the asset to
the receiver's seal. If the receiver skips this, a malicious sender could co-sign a *different* exit
tx (no/!wrong OP_RETURN) and, on a unilateral exit, keep the asset while the receiver believes it
owns it. The validation primitive is `validate_consignment_offchain(consignment, exit_txid, …)`,
which re-derives the commitment from the consignment and checks it against the exit tx — done before
the witness tx is ever broadcast. This is the statechain analogue of an LN counterparty checking the
commitment transaction before revoking the previous state.

## Status / what is already proven on regtest (all green)

- **Full lifecycle** — deposit → transfer → unilateral exit, and deposit → cooperative withdraw —
  end-to-end via the color path, with the receiver cryptographically validating the consignment and
  the statechain UTXO spent on-chain. (`rgb01_full_lifecycle.rs`.)
- **Deposit** with standard-method visibility: `create_statechain_utxo` (send-based) makes
  `get_asset_balance` drop 2000→0 with a `Send` in `list_transfers` and the colored UTXO spent +
  change UTXO created. (`rgb02_deposit_coop_exit.rs`.)
- **Exit to on-chain via a standard rgb-lib witness invoice, and back.** The receiver settles a
  statechain exit with the *same* `witness_receive` + `refresh` flow as on-chain RGB, so the asset
  lands on its on-chain wallet (`get_asset_balance` = 1000); then it is re-deposited onto a fresh
  statechain UTXO (exit output spent, new coin confirmed & re-colorable). (`rgb03_exit_to_onchain.rs`.)
- **Statechain UTXOs as on-chain colorable UTXOs (the register primitive), proven with standard
  methods.** Sender: after `register_statechain_utxo`, `get_asset_balance` reads 1000 and
  `list_unspents` shows the statechain UTXO as `colorable/exists` carrying the allocation (moved off
  the spent on-chain source, not double-counted). Receiver: a *free* statechain UTXO is onboarded and
  registered, then standard `blind_receive` reserves it as the invoice seal (`pending_blinded`),
  i.e. the rgb invoice's `recipient_id` references the statechain UTXO. (`rgb04_register_statechain_utxo.rs`.)
- **Gap to close** for a single deposit→partial-transfer-with-change→exit path with standard balances
  on both sides throughout: item (3) above — extend `color_psbt` to assign to blinded/existing-outpoint
  beneficiaries (receiver's statechain seal + change to a free statechain UTXO).
