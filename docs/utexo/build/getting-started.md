# Getting started

Build a wallet that deposits, pays exact amounts off-chain, and exits — in about 30 lines.

## 1. Run the local stack (regtest)

```bash
# bitcoind + electrs + RGB proxy
cd rgb-lightning-node && ./regtest.sh start
# Mercury SE (server + lockbox + token server)
cd mercurylayer && docker compose -f docker-compose-lockbox.yml up -d
```

## 2. Add the SDK

```toml
[dependencies]
mercury-utexo-sdk = { path = "clients/libs/rust-sdk" }
tokio = { version = "1", features = ["full"] }
```

> rgb-lib does blocking I/O internally — run on a multi-thread tokio runtime
> (`#[tokio::main(flavor = "multi_thread")]`).

## 3. Wallet in 30 lines

```rust
use mercury_utexo_sdk::{SdkConfig, UtexoWallet, WalletEvent};

#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    // Create (or re-open — pass Some(mnemonic)) a wallet.
    let (wallet, mnemonic) = UtexoWallet::initialize(SdkConfig::regtest("alice"), None).await?;
    println!("seed phrase: {mnemonic}");
    // NOTE: the mnemonic alone is NOT a full backup — off-chain exit material lives only on disk.
    // Use wallet.export_recovery_bundle() for a complete backup (see the Wallet SDK guide).

    // Receive address (stable, shareable).
    println!("my address: {}", wallet.get_utexo_address().await?);

    // Deposit: fund this address on L1 with exactly 100_000 sats…
    let deposit = wallet.get_deposit_address(100_000).await?;
    println!("send 100k sats to {deposit}");

    // …and let the watcher confirm it + auto-claim all incoming transfers.
    let mut events = wallet.subscribe();
    let _bg = wallet.start_background();
    while let Ok(ev) = events.recv().await {
        match ev {
            WalletEvent::DepositConfirmed { amount_sats, .. } => {
                println!("deposit confirmed: {amount_sats} sats");
                break;
            }
            _ => {}
        }
    }

    // Pay any exact amount, off-chain (auto coin-selection / off-chain split).
    let bob = "tml1...";
    let sent = wallet.transfer(bob, 15_000).await?;
    println!("paid {} sats ({} coins, split: {})", sent.total_sats, sent.coins.len(), sent.used_split);

    // Exit to L1 whenever you want (cooperative, immediate).
    wallet.withdraw("bcrt1...", None, None).await?;
    Ok(())
}
```

## 4. Where next

- [Wallet SDK guide](wallet-sdk.md) — every operation with examples.
- [Issuer SDK guide](issuer-sdk.md) — launch a token in two calls.
- [Testing guide](testing-guide.md) — the E2E suites and how to run them.
