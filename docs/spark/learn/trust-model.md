# Trust model

## What you trust, exactly

| Property | Guarantee | Mechanism |
|---|---|---|
| SE cannot steal | cryptographic | 2-of-2 keys: the SE holds one share, never the full key |
| You can always exit | cryptographic (+ timeliness) | pre-signed backup txs; branches for sub-coins; no SE cooperation needed |
| Current owner wins the exit race | economic/timelock | decrementing locktimes: your backup unlocks before any previous owner's |
| No double-spend of off-chain state | SE honesty | the SE refuses to co-sign conflicting spends (blind, but sequence-aware); optional per-coin `single_use` hard rule |
| Token correctness | cryptographic | RGB client-side validation — the SE never sees or vouches for token state |

Spark distributes the "refuses conflicting state" role across n operators with FROST (honest if
≥1 of n is honest). Here the role is one SE. In both systems a fully-colluding operator side can
sign an old owner's state; in both, the current owner's earlier locktime + timely exit is the
backstop. Nothing about *custody* rests on the SE in either design.

## Timeliness obligations

- **Exit before old state unlocks.** Each transfer decrements the backup locktime by the SE's
  `interval`. When a coin's remaining lifetime gets low, exit (or transfer to yourself via a
  fresh deposit). The SDK surfaces coin locktimes; vanilla Mercury behaviour.
- **Epoch deadline (optional).** A coin created with `epoch_deadline` stops getting new SE
  co-signatures after the deadline — transact or exit before it. Unilateral exit still works
  after (it needs no SE).

## What the receiver of an off-chain sub-coin verifies

1. The transfer signature binds the coin to *their* key.
2. The **exit branch**: every branch tx is consensus-valid (scripts + signatures verified
   locally), links parent→child, and terminates at an on-chain, unspent, confirmed root.
3. The backup chain: latest backup pays the receiver; locktimes decrement correctly.
4. For tokens: the RGB consignment validates off-chain against the same branch, and the balance
   is booked under the consignment's **verified** contract id.
