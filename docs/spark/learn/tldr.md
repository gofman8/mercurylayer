# TL;DR

**Mercury+RGB is a Bitcoin L2 with Spark-class UX on a single statechain entity.** Users deposit
BTC (or RGB tokens) onto statechain coins and then transact **off-chain, instantly, with no
per-payment on-chain cost**: exact-amount payments, token transfers, and Lightning swaps. Every
coin remains unilaterally exitable to Bitcoin L1 at any time.

- **Instant + free transfers.** Payments are SE-co-signed key handovers and off-chain splits —
  no blocks, no miner fees, sub-second.
- **Exact amounts, no denominations.** The SDK mints exact amounts by splitting coins off-chain
  (Spark needs SSP swap pools for this; here it is a native primitive).
- **Tokens are RGB.** Client-validated assets (not trusted server state) ride the same coins.
  Issue, transfer and exit tokens with the same UX as sats.
- **Lightning interop.** Preimage-locked swaps (the Mercury lightning latch) connect coins to
  BOLT11 payments through any LSP.
- **Self-custody.** 2-of-2 (you + SE) with pre-signed exits: if the SE disappears, you broadcast
  your backup (and branch) and take your funds on L1. The SE can never move funds alone.

The SDK (`mercury-spark-sdk`) hides everything operational — UTXOs, backup transactions, coin
selection, splits, consignments, claim polling. Applications deal in **addresses and amounts**.

```rust
let (wallet, mnemonic) = SparkWallet::initialize(SdkConfig::regtest("alice"), None).await?;
wallet.transfer(&bob_address, 15_000).await?;          // exact sats, off-chain
wallet.transfer_tokens(&asset, &bob_address, 250).await?; // exact tokens, off-chain
```
