# Transfers

## What a transfer is

Mercury transfers move **whole coins** by key handover (like Spark moves whole leaves):

1. Sender fetches `x1` from the SE and signs the coin over to the receiver's address.
2. Sender pre-signs the receiver's backup tx (locktime = previous − interval) and posts an
   encrypted transfer message through the SE's relay.
3. Receiver (async, whenever online) validates everything and calls the SE, which **rotates its
   key share** — from now on only receiver+SE can sign; the sender's share is dead.

No block confirmation, no fee, sub-second, fully async.

## Exact amounts: the amount-maker

`transfer(address, amount)` never asks you to think about coins:

- **Exact subset**: if some subset of your coins sums to the amount, each is handed over
  (works with any Mercury wallet as receiver).
- **Off-chain split**: otherwise the SDK splits one coin — a single SE-co-signed, un-broadcast
  tx minting an exact piece plus your change — and hands the piece over. The receiver gets the
  **exit branch** inside the transfer message and verifies it end-to-end (consensus-valid txs
  back to an on-chain root). Sender keeps the change as a normal coin.

Spark achieves the same UX with SSP swap pools (fixed leaf denominations swapped server-side);
here exact amounts are a first-class off-chain operation with no third party.

```
alice coins: [60k]                    alice pays 15k:
                                      split 60k → [15k piece][44.4k change]  (off-chain)
                                      handover piece → bob                    (off-chain)
```

## Receiving

Receiving is passive: share your address (`get_spark_address()` — stable, reusable). The SDK's
background watcher claims incoming transfers automatically and emits `TransferClaimed` /
`TokenTransferClaimed` events. A claim:

- verifies the transfer signature, backup chain and (for sub-coins) the exit branch,
- completes the SE key rotation,
- for tokens: validates the consignment off-chain and books the verified asset.

## Token transfers

`transfer_tokens(asset, address, amount)` = a **colored** off-chain split (the piece sub-coin
carries exactly `amount` of the asset; change keeps the rest) + the same handover. The RGB
consignment rides the transfer message. See [tokens](tokens.md).
