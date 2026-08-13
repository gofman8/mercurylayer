# Admission-gate inputs and their provenance
> **Citations here are by SYMBOL, deliberately.** Every one of the six line numbers this file
> originally carried had rotted: at the time of the Stage-0.3 sweep (2026-08-13) the five citations
> into `tesr.rs` (lines 3288, 3233, 3232, 3220, 3031) all resolved into `cosign_colored_renewal` and
> `cosign_colored_receiver_state` — functions with no relationship to anything claimed below — and
> the one into `receiver.rs` (line 684) landed in
> `verify_if_locktime_is_reasonable_tx_version_and_output_size`. A
> citation that silently points somewhere else is worse than none: it reads as verified.

> **Status:** normative. Describes what is built, at `feat/spark`.

The exit-headroom gate (`check_exit_headroom`, `lib/src/transfer/receiver.rs`) is the check that
refuses a conveyed child whose pre-signed exit provably cannot finish before the funding epoch
expires. It is called once, from `verify_conveyed_child` (`clients/libs/rust/src/tesr.rs`).

This file exists because the same defect shape has now appeared **three times** — a gate computing
its own admission requirement from a number the sender chose:

| # | where | the sender-supplied term |
|---|---|---|
| C-1 | the ancestor census in `verify_bundle_ex` | the disclosed tier count |
| — | the flat-conveyance classifier in `transfer_sender.rs` | the shape of the coin's own record |
| B1 | this gate | `TesrTier::csv`, a plain serde field |

Per-site fixes did not converge, so the rule is now stated positively and the inputs enumerated.
**Every term of the requirement must be receiver-derived.** A term is receiver-derived when the
receiver reads it from its own backend, its own config, or from a signature the receiver verified
against on-chain data it fetched itself. Being *present in a struct the sender sent* is not
provenance.

## The requirement

```
required  = Σ over the exit chain of (csv_i + 1)          exit_wait_blocks()
available = epoch_expiry_height − tip
refuse if available < required
```

Three terms, plus the chain's length. Each one below.

## 1. `csvs` — the per-tier relative timelocks

**Receiver-derived, from signatures.** `child_exit_chain_bound`
(`clients/libs/rust/src/tesr.rs`) parses every transaction in the chain and reads its timelock
from the signed `nSequence` under BIP-68 — the only copy of that number Bitcoin will ever enforce.

`bind_declared_csv` (`lib/src/transfer/receiver.rs`) then compares the signed value against the
bundle's declared `TesrTier::csv` and returns `DeclaredCsvMismatch` when they differ, naming the
tier, both values, and which one is authoritative. The refusal is deliberate rather than a silent
preference for the signed value: a bundle whose two copies disagree is either forged or corrupt, and
neither should be admitted quietly. `None` and `Some(0)` are **not** treated as equal — "no relative
lock at all" and "a relative lock of zero" are different claims.

Two encodings are refusals rather than values:

* **bit 31 set** — the relative lock is disabled. Reported as `None` (this is the trigger's
  `TRIGGER_SEQUENCE`), and `exit_wait_blocks` still charges it the one block its parent needs to
  confirm.
* **bit 22 set** — a 512-second-denominated lock. This schedule is expressed entirely in blocks, so
  such a tier cannot be placed on the exit timeline at all. Refused, never counted as costing zero.

The binding is applied on **every** acceptance path, not only at the gate: `verify_bundle_ex` binds
the trigger and each ladder tier, `verify_child_bundle` binds each ancestor segment's pair and the
child's own two tiers. The SSP's pre-payment census therefore refuses a forged bundle too.

`exit_child_pass` deliberately keeps reading the raw chain: it ignores the csv entirely and
broadcasts signed transactions, so binding there would only refuse an owner their own exit.

## 2. `tip` — the chain tip

**Receiver-derived.** `cc.electrum_client.block_headers_subscribe_raw()`, called inside
`verify_conveyed_child` (`clients/libs/rust/src/tesr.rs`) — the receiver's own chain backend. A read failure propagates;
it is never defaulted to 0, which would make every epoch look infinitely long.

## 3. `epoch_expiry_height` — the absolute clock

**Sender-supplied material, receiver-validated; the term that needs the most care.** It is the lowest
`nLockTime` of the parent's flat backup chain — the first height at which the parent's current owner
can broadcast a transaction that spends `F` and voids the entire tree. It is the only absolute clock
in the structure.

The chain arrives inside the bundle (`cb.parent_flat_backups`). Four independent checks make the
resulting number usable:

1. **Every entry is signature-verified under `F`'s key** and its prevout is pinned to
   `(F.txid, F.vout)` — checked explicitly inside `validate_backup_chain_v2`, because
   `verify_transaction_signature` alone would accept a backup naming a foreign txid.
2. **`F` itself is fetched from chain by the receiver**, not taken from the bundle.
3. **INV-5 strict decrement** (`validate_backup_chain_v2`) rejects duplicate padding and chain
   inversion alike.
4. **The count is pinned by the exact-equality census.** This is the load-bearing one. Without it a
   sender would simply omit the low entries and present a chain whose minimum is far in the future.

### The two operator-supplied bounds, named honestly

`initlock` and `interval` come from `info_config(cc)` (`clients/libs/rust/src/utils.rs`) — the
**operator**, not the receiver. They bound the locktime validation: `initlock` caps any entry at
`tip + lockheight_init`.

They are not a bypass, and the direction is worth stating rather than asserting. An inflated
`initlock` would let a colluding operator and sender co-sign a backup with a locktime further in the
future, making `epoch_expiry_height` **larger** and the gate more permissive. That admits a coin
whose declared epoch is longer than intended — but the sender's power to void the tree is bounded by
the **lowest** entry they hold, not the highest, and the census pins the count so low entries cannot
be hidden. The remaining exposure is therefore a coin admitted against a longer-than-configured
epoch, not a coin whose sender can void it earlier than the receiver computed.

`cc.fee_rate_tolerance` and `cc.max_fee_rate` are the receiver's own config.

## 4. The chain's length

**Structurally pinned, not declared.** `verify_child_bundle` links every tier to the outpoint of the
tier above it, so a segment cannot be omitted to shorten the walk without breaking the funding chain
outright. The number of terms in the sum is thus as unforgeable as the sum itself.

## Residual — the one sender-supplied term left in this neighbourhood

`cb.parent.params` (the `TesrParams` schedule) is still a serde field on the conveyed bundle.

It is **not** an input to the requirement any more — that is now `Σ(signed nSequence + 1)` — but it
is the source of the per-tier `[e_floor, e0]` / `[d_floor, d0]` sanity bounds in `verify_bundle_ex`
and `verify_child_bundle`. A sender declaring wide params relaxes its own bounds check.

This is not a gate bypass: a wider bound only admits a tier whose real CSV is then counted **in
full**, which makes the requirement larger, not smaller. It is recorded here rather than fixed
because deriving params from the receiver's own network config would refuse coins legitimately built
under a different preset — a compatibility decision, not a security one.

## The rule, for the next gate

Before adding any admission check, write its inputs down and mark each one:

* **backend** — the receiver's own chain/DB read. Safe.
* **config** — the receiver's own config. Safe.
* **operator** — from `info_config`. State which direction an inflated value fails toward.
* **signature** — read from a transaction the receiver parsed and verified. Safe, provided it is read
  from the signed bytes and not from a field beside them.
* **declared** — a serde field. **Not admissible.** Either bind it to a signature, derive it, or
  delete it from the calculation.
