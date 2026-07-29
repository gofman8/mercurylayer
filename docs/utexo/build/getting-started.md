# Getting started

Build a wallet that deposits, pays any amount off-chain, and exits — in about 30 lines.

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
    // NOTE: the mnemonic alone is NOT a full backup — off-chain exit material (the pre-signed
    // tier chain) lives only on disk. Use wallet.export_recovery_bundle() for a complete backup
    // (see the Wallet SDK guide).

    // Receive address (stable, shareable).
    println!("my address: {}", wallet.get_utexo_address().await?);

    // Deposit: fund this address on L1 with exactly 100_000 sats…
    let deposit = wallet.get_deposit_address(100_000).await?;
    println!("send 100k sats to {deposit}");

    // …and let the watcher confirm it + auto-claim all incoming transfers. The same background
    // pass that confirms the deposit also establishes its TES-R exit ladder
    // (WalletEvent::LadderEstablished) — nothing for you to schedule.
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

    // Pay any amount, off-chain: an exact subset of coins if one fits, otherwise an in-ladder
    // split mints the exact piece and conveys it to the recipient (used_split == true).
    let bob = "tml1...";
    let sent = wallet.transfer(bob, 15_000).await?;
    println!("paid {} sats ({} coins, split: {})", sent.total_sats, sent.coins.len(), sent.used_split);

    // Exit to L1 whenever you want — cooperative, immediate, one tx per coin, no timelock wait.
    wallet.withdraw("bcrt1...", None, None).await?;
    Ok(())
}
```

## 4. What your coin actually is

Every plain BTC deposit is **laddered** at claim — unconditionally, there is one protocol and no
version switch to set. The ladder is TES-R: three **pre-signed, un-broadcast** tiers over the
on-chain funding output `F`.

```
F ──▶ T           TRIGGER    no timelock, signed once at deposit
       └──▶ X_m   EXTENSION  relative CSV E_m — renewal replaces it horizontally
             └──▶ S_k  STATE relative CSV Δ_k — decrements by δ on every transfer
```

All three tiers are v3/TRUC with a P2A anchor. BIP-68 relative timelocks only start counting once
the **parent confirms**, and `T` carries no timelock — so **nothing matures until someone
broadcasts `T`**. An idle coin therefore never ages: there is no calendar deadline, no expiry to
watch, and **0 vB of idle rent**. Your app does not need a clock.

- **Transfer** = the SE co-signs a fresh state one δ **lower** than the one it replaces
  (replace-by-lower-timelock), so the new owner's state always matures first. The old state is
  disclosed as superseded and counted by the receiver's census at claim.
- **Renewal** (a lower-CSV extension) and **rollover** (a fresh level) are off-chain and unbounded —
  a coin can live off-chain forever, no on-chain touch required (sdk43).
- **`refresh` is the re-anchor primitive**, not a deadline reset: one on-chain tx that moves the coin
  to a fresh funding outpoint and mints a new ladder. Use it when you want a new anchor (e.g. the
  coin's funding has to change), not because time is running out (sdk30).
- **Unilateral exit** walks the pre-signed chain tier by tier, waiting out each relative timelock.
  `unilateral_exit` is incremental and idempotent — it advances as far as maturity allows and
  reports `ExitStatus { complete, wait_blocks }`; call it once per block until `complete` (sdk50).
  No SE involvement, and no race to win: you start the clock yourself.

```rust
// Unilateral (no SE cooperation): drive it until every coin reports complete.
for st in wallet.unilateral_exit(None, None).await? {
    println!("{} complete={} wait_blocks={}", st.statechain_id, st.complete, st.wait_blocks);
}
```

## 5. Non-exact amounts: the in-ladder split

When no subset of your coins sums to the amount, `transfer` pays via an **in-ladder split**: a state
tier `SP` spending `X_m.out[0]` — a *descendant* of the trigger, never a rival for `F` — funding a
piece child for the recipient and a change child for you. The piece bundle is conveyed straight to
the recipient's mailbox with the standard key handover (sdk58, sdk59).

Each child funds its own two tiers plus dust, so there is an admission floor: `min_child_value` =
**1306 sat at 2 sat/vB**. Below it the split is refused — combine or send a whole coin instead.

Received split children are **first-class**. The claim completes the standard SE key handover, so
the receiver co-owns `A_child` (that key is invariant across the rotation, which is what keeps the
pre-signed exit chain valid) and the sender is permanently locked out. A child can be paid onward
off-chain — whole (`child_retransfer`) or split again (`child_in_ladder_pay`, a depth-2 ancestors
chain) — at one co-signature and one disclosed superseded state per hop, all counted by the
receiver's census (sdk60, sdk17).

## 6. The other coin shape: un-laddered

Not every coin is laddered, by design:

| Shape | Which coins | How it transfers |
|---|---|---|
| **Laddered (TES-R)** | every plain BTC deposit | co-sign a lower-CSV state; exit walks `T → X_m → S_k` |
| **Un-laddered** | RGB carriers; split sub-coins whose funding is un-broadcast | signed-once backup with decrementing `nLockTime`; transfer hands over the backup chain |

An **RGB carrier is deliberately never laddered**: a plain tier spend would destroy the allocation,
so RGB rides the signed-once colored-carrier model under the terminal-freeze rule (PROTOCOL.md
§5.10). A **split sub-coin** whose funding output is still un-broadcast cannot root a trigger, so it
keeps the signed-once shape too. This path is current and load-bearing for assets — not deprecated
(sdk52, sdk39). If you hold RGB, the decrementing-locktime mechanics still apply to those carriers;
for plain BTC they do not.

`withdraw` and `unilateral_exit` refuse to sweep a carrier into an RGB-unaware spend rather than
silently burn the allocation; move the asset off the coin first, or let `auto_exit_due` materialize
the carrier (`WalletEvent::TokenCarrierMaterialized`).

## 7. Where next

- [Wallet SDK guide](wallet-sdk.md) — every operation with examples.
- [Issuer SDK guide](issuer-sdk.md) — launch a token in two calls.
- [Testing guide](testing-guide.md) — the E2E suites and how to run them (`SDK_E2E=1..68`, plus the
  `chaos22` soak).
- [PROTOCOL.md](../PROTOCOL.md) — the shipped TES-R design: tiers, renewal, splits, race analysis.
- [CHILDREN.md](../CHILDREN.md) — first-class split children and multi-hop off-chain payment.
- [LIGHTNING.md](../LIGHTNING.md) — Lightning in both directions on the ladder via a HODL-invoice
  latch (sdk63 pay, sdk64 receive, sdk65/sdk67 non-exact).
