# Core concepts

## The statechain entity (SE)

The SE is Mercury Layer's co-signing service: a server plus a lockbox (key enclave) holding **one
half of a 2-of-2 key for every coin** (blind MuSig2). It:

- co-signs spends without seeing what it signs (blind signing — no amounts, no addresses),
- rotates its key share on transfer, so previous owners lose the ability to co-sign,
- refuses double-spends: one active spend chain per coin (plus optional per-coin `single_use`
  hard rule and an `epoch_deadline` after which it stops co-signing new state).

The SE **cannot move funds** (it has one of two keys) and **cannot freeze you out** (you hold a
pre-signed exit). This is the same role Spark's operator set plays — collapsed to one entity, so
the trust assumption is "the SE is honest about refusing conflicting state" instead of "1 of n
operators is honest". Either way, unilateral exit never needs the operator's cooperation.

## Coins and sub-coins (Spark: trees and leaves)

A **coin** is a statechain: an on-chain UTXO whose key is `owner + SE`, plus a chain of pre-signed
backup transactions with **decrementing locktimes** (each transfer hands the new owner a backup
that unlocks *earlier* than every previous owner's — the current owner always wins the exit race).

An **off-chain split** spends a coin into new **sub-coins** in a single SE-co-signed transaction
that is *not broadcast*. Sub-coins are full statechain coins (transferable, splittable again);
the un-broadcast split tx is their **exit branch**. This is exactly Spark's tree: the deposit is
the root, splits are branches, sub-coins are leaves — one tree per deposit, a forest overall.

```
on-chain deposit (root)
   └─ split tx S1 (un-broadcast, SE-co-signed)
        ├─ sub-coin A  → transferred to Bob (branch = [S1])
        └─ sub-coin B  → split again → S2 → sub-sub-coins …
```

- **Width is free**: splitting into N pieces is one off-chain tx.
- **Depth costs on exit**: a unilateral exit broadcasts the branch — one tx per level.

## Transfers

A transfer is a **key handover**: the sender pre-signs the receiver's backup, the SE rotates its
key share to the receiver, and the sender can no longer co-sign. Transfers move whole coins —
the SDK makes arbitrary amounts by picking an exact subset of coins or minting an exact piece via
an off-chain split (see [transfers](transfers.md)).

Transfers of **sub-coins** carry their exit branch in the transfer message; the receiver verifies
the branch (script-level consensus validation back to an on-chain, unspent, confirmed root)
before accepting.

## Tokens

Tokens are **RGB assets**: client-validated contracts whose allocations live on coins and
sub-coins. The server knows nothing about tokens — validation is done by the receiving wallet
against cryptographic consignments. See [tokens](tokens.md).

## Exits

- **Cooperative (normal, 1 tx)**: the SE co-signs a direct spend to your L1 address. For
  sub-coins the branch is broadcast first. No timelock wait.
- **Unilateral (SE gone)**: broadcast your pre-signed backup (and branch); your backup's
  locktime is the earliest of all owners', so you win the race. See [exits](exits.md).
