# Getting started

A wallet that deposits, pays an arbitrary amount off-chain, and exits — in about thirty lines.

## 1. Run the local stack (regtest)

Two stacks. The chain half (bitcoind, electrs, the RGB consignment proxy) comes from the
`rgb-lightning-node` checkout; the statechain half (coordinator, lockbox, its Vault and Postgres)
comes from this repo.

```bash
# bitcoind + electrs + RGB proxy
cd rgb-lightning-node && ./regtest.sh start
# coordinator (:8000) + lockbox (:18080) + vault + postgres
cd mercurylayer && docker compose -f docker-compose-lockbox.yml up -d
```

`SdkConfig::regtest` points at exactly those endpoints: coordinator `http://127.0.0.1:8000`,
electrum `tcp://localhost:50001`, RGB proxy `rpc://127.0.0.1:3000/json-rpc`. Deposit tokens are
issued free by the coordinator on this stack — `docker-compose-lockbox.yml` sets no token-server
URL, so `get_token_no_server` mints them unpriced and `deposit_token_id: None` just works.

> `docker-compose-lockbox.yml` also defines a `web` service publishing port 3000 — the port the RGB
> proxy uses. Name the services you want (`… up -d mercury-server lockbox`) if that collides.

> **Bitcoin Core 28+.** Every ladder tier is a v3/TRUC transaction with a P2A anchor output.

### Pin the enclave attestation identity — or nothing gets a ladder

The client verifies the enclave's signature-count attestation against a **pinned** identity, and the
resolution order is *compiled-in pin → configuration → refuse*. Never the key the coordinator
serves: verifying an attestation against a key its own counterparty hands you proves nothing. No
network ships a compiled-in pin (`TesrParams::attestation_identity_const` returns `None` for every
one of them) and both `SdkConfig` constructors ship `attestation_identity: None`.

So on the bare defaults every laddering claim **refuses**, records
`LadderSkipReason::AttestationIdentityUnpinned`, and leaves the coin flat. That is the correct
direction to fail, and it is configuration you must supply. Read the value off your lockbox and pin
it:

```bash
curl -s http://127.0.0.1:18080/attestation_identity   # -> {"attestation_identity_pubkey":"0x…"}
export UTEXO_ATTESTATION_IDENTITY=0x…                 # or set SdkConfig::attestation_identity
```

`SdkConfig::attestation_identity` is read first and the environment variable is the fallback
(`ClientConfig::from_params`). A value that disagrees with a compiled-in pin is an error, not an
override. The identity is derived from the lockbox container's seed, so a differently-seeded lockbox
refuses rather than passes.

## 2. Add the SDK

```toml
[dependencies]
mercury-utexo-sdk = { path = "clients/libs/rust-sdk" }
tokio = { version = "1", features = ["full"] }
```

The crate links as `mercury_utexo_sdk`. Its RGB bridge (`mercury-rgb`) pins `rgb-lib` to the
`gofman8/rgb-lib` fork **by git revision**, so a clean clone builds with no sibling checkout. Every
client crate that ships a `rust-toolchain.toml` — `clients/libs/rust`, `clients/apps/rust`,
`clients/tests/rust`, `lib` — pins channel **1.83.0**; build the SDK on that.

> `rgb-lib` does blocking I/O and the SDK wraps it in `tokio::task::block_in_place`, which panics on
> a current-thread runtime. Run on a multi-thread one:
> `#[tokio::main(flavor = "multi_thread")]`.

## 3. A wallet

```rust
use mercury_utexo_sdk::{SdkConfig, UtexoWallet, WalletEvent};

#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    // Create (or re-open — pass Some(mnemonic)) a wallet.
    let mut cfg = SdkConfig::regtest("alice");
    cfg.attestation_identity = Some(std::env::var("UTEXO_ATTESTATION_IDENTITY")?);
    let (wallet, mnemonic) = UtexoWallet::initialize(cfg, None).await?;
    println!("seed phrase: {mnemonic}");
    // The mnemonic alone is NOT a backup. Off-chain exit material — the pre-signed tier chain and
    // the backup rows — lives only on disk. `export_recovery_bundle` is the complete one, and it
    // must be re-taken after every transfer, claim or split.

    // Receive address (stable, shareable, bech32m `ml1…` / `tml1…`).
    println!("my address: {}", wallet.get_utexo_address().await?);

    // Deposit: fund this single-use address on L1 with exactly 100_000 sats…
    let deposit = wallet.get_deposit_address(100_000).await?;
    println!("send 100k sats to {deposit}");

    // …and let the background watcher confirm it and auto-claim incoming transfers. The same pass
    // that confirms the deposit establishes its exit ladder (WalletEvent::LadderEstablished) —
    // nothing for you to schedule.
    let mut events = wallet.subscribe();
    let _bg = wallet.start_background();
    while let Ok(ev) = events.recv().await {
        match ev {
            WalletEvent::DepositConfirmed { amount_sats, .. } => {
                println!("deposit confirmed: {amount_sats} sats");
                break;
            }
            // A coin left FLAT says so, once, when the reason changes.
            WalletEvent::LadderSkipped { statechain_id, reason } => {
                println!("{statechain_id} stayed flat: {reason:?}");
            }
            _ => {}
        }
    }

    // Pay any amount, off-chain: an exact subset of coins if one exists, otherwise an in-ladder
    // split mints the exact piece and conveys it to the recipient (`used_split == true`).
    let bob = "tml1…";
    let sent = wallet.transfer(bob, 15_000).await?;
    println!("paid {} sats ({} coins, split: {})",
             sent.total_sats, sent.coins.len(), sent.used_split);

    // Exit to L1 whenever you want — cooperative, one tx per coin, no timelock wait.
    wallet.withdraw("bcrt1…", None, None).await?;
    Ok(())
}
```

Each pass of `start_background` runs, in order: `claim`; then `deadline_safety_due`, which is
**unconditional** — `maintenance_plan` returns it for every configuration, it tries the cooperative
re-anchor first (`auto_refresh_due` is its route 1) and severs from `F` for whatever the counterparty
declines to co-sign; then `defend_ladders`, once per new block, a no-op while `F` is unspent and the
thing that races a hostile trigger when it is not; then `auto_exit_due` when `SdkConfig::auto_exit`
is set (it ships **on**). A pass that cannot *see* fails closed and says so —
`WalletEvent::WatchtowerBlind`, retained on `watchtower_faults` / `is_watchtower_blind`. Treat it as
an alert: while it persists, nothing is racing a hostile trigger or a clawback for you.

## 4. What your coin actually is

Every plain BTC deposit is **laddered** at `claim()`. There is one protocol and no version switch.
The ladder is three **pre-signed, un-broadcast** tiers over the on-chain funding output `F`:

```
F ──▶ T           TRIGGER    no timelock, signed once at deposit
       └──▶ X_m   EXTENSION  relative CSV E_m — renewal replaces it horizontally
             └──▶ S_k  STATE relative CSV Δ_k — decrements by δ on every transfer
```

All three are v3/TRUC with a P2A anchor (`P2A_VALUE` = 240 sat) and each bakes in
`committed_fee(rate)` = `ceil(TIER_VBYTES × rate)` with `TIER_VBYTES` = 125 and
`TesrParams::committed_fee_rate` a protocol constant of **3.0 sat/vB** on every network, so a rung
costs 375 + 240 = 615 sat — a tier is signed years before it is broadcast, so its fee is
fixed at build time and topped up through the anchor if the mempool has moved.

BIP-68 relative timelocks start counting only once the **parent confirms**, and `T` carries no
timelock — so **nothing matures until someone broadcasts `T`**. On the CSV side an idle coin never
ages (INV-27): the tier chain adds **0 vB of rent** and no deadline, however long the coin — or a
whole idle split DAG — sits.

**That is a claim about the tiers, not about the coin.** A laddered coin keeps its flat backup chain
(below), whose locktimes are **absolute** and do age, so it carries exactly one calendar: `min(L_k)`,
a height its *prior owners* hold. On the mainnet profile a fresh anchor buys **10 000 blocks ≈ 69
days**, every whole-coin hop spends **100** of them (100 decrements, 99 usable), and one 112-vB
re-anchor buys the next `min(99 payments, 69 days)` — ≈ 1.13 vB amortised per payment, ≈ 589 vB per
coin-year idle. Your app does not have to *watch* that clock, because `deadline_safety_due` runs on
every background tick and defends it; but the clock exists, and no client surfaces the height itself.

- **Transfer** = the SE co-signs a fresh state one δ **lower** than the one it replaces
  (replace-by-lower-timelock), so the new owner's exit always matures first. The superseded state is
  disclosed and counted by the receiver's census at claim.
- **Renewal** (a lower-CSV extension) and, at `m_max`, **rollover** (an off-chain self-split onto a
  fresh level) are both free of the chain — the CSV budget is renewable without limit; what is
  finite is the flat calendar above, and a re-anchor is what buys more of it. The mainnet schedule is `d0 = 1440`, `δ = 36`, `d_floor = 144`, `e0 = 720`, `δE = 36`,
  `e_floor = 144`, `m_max = 15` (`TesrParams::mainnet`); regtest runs a scaled-down copy so an E2E
  lifecycle mines in seconds. Testnet and signet run the **mainnet** schedule, deliberately.
- **`refresh` is the re-anchor primitive**: one SE-co-signed, single-input on-chain tx moving the
  coin into a fresh deposit aggregate — new `statechain_id`, same owner, a full ladder of its own —
  which permanently invalidates every exit right rooted at the old outpoint, each prior owner's
  backup and every old tier included. It is **not** a deadline reset for the *exit* (the CSV chain
  never matures while idle), but it *is* what resets the flat calendar, minting a fresh chain at
  `tip + initlock`. That makes it the answer for a coin that has spent most of its hop budget. The
  fee comes out of the coin (single input, blind SE); `refresh_sponsored` rebates it off-chain.
- **Unilateral exit** walks the pre-signed chain tier by tier, waiting out each relative timelock.
  `unilateral_exit` is incremental and idempotent — it advances as far as maturity allows and
  reports `ExitStatus { statechain_id, complete, wait_blocks }`. Call it once per block until
  `complete`. No SE involvement and no race to win: you start the clock yourself.

```rust
// Unilateral (no SE cooperation): drive it until every coin reports complete.
for st in wallet.unilateral_exit(None, None).await? {
    println!("{} complete={} wait_blocks={}", st.statechain_id, st.complete, st.wait_blocks);
}
```

A **separate flat backup chain** hangs off `F` with absolute locktimes `L_k = L_0 − k·interval`
(mainnet `initlock` = 10 000 blocks, `interval` = 100; regtest 1 000 / 10 — the same 100 hops of
capacity, scaled). Its lowest locktime, `min(L_k)`, is the coin's epoch expiry and the calendar named
above. Broadcasting `T` spends `F` and kills every flat backup permanently — the
two lanes are rivals for one output, which is why only one of them is ever live for a given coin.

The full treatment is [PROTOCOL.md](../spec/PROTOCOL.md); what is verified versus trusted is
[TRUST-MODEL.md](../spec/TRUST-MODEL.md).

## 5. Non-exact amounts: the in-ladder split

Payments are arbitrary amounts, and no coin set can be made fine enough for a subset sum to land on
one — so the ordinary payment is an **in-ladder split**: a state tier `SP` spending `X_m.out[0]` (a
*descendant* of the trigger, never a rival for `F`) funding a piece child for the recipient and a
change child for you. The piece bundle is conveyed straight to the recipient's mailbox with the
standard key handover.

Each child funds its own two tiers plus dust, so there is an admission floor:
`mercurylib::tesr::min_child_value(rate, DUST_LIMIT)` = `2·(committed_fee + P2A) + dust` =
`2·(375 + 240) + 330` = **1 560 sat** at the shipped 3 sat/vB. Below it the split is refused —
combine, or send a whole coin. The floor is a *function of the rate*, not a constant: quote it with
its rate or not at all. The sender's change leg has a cheaper shape of its own
(`min_spine_tip_value`, one rung: `committed_fee + P2A + dust` = 945 sat at that same rate), which is
why the SDK reports a floor **per leg**. Applying the tip's floor to a payee's piece admits a piece
that cannot fund its own two tiers, and it dies *after* `SP` is co-signed — stranding the parent.

Ask before you commit: `quote_transfer(amount)` returns a `TransferQuote` with `network_fee_sats`,
`renewal_fee_sats`, `total_fee_sats`, `fundable`, `stuck_coins` and `no_exit_material_coins`, and it
runs the *same* planner `transfer` executes, so `fundable: true` followed by a refusal is not
expressible.

Received children are **first-class**. The claim completes the SE key handover, so the receiver
co-owns `A_child` — that key is invariant across the rotation, which is what keeps the pre-signed
child tiers valid — and the sender is permanently locked out. A child pays onward off-chain, whole
or split again, one co-signature per hop, and each receiver runs the same exact-equality census
(`child_num_sigs == child_flat_backups + conveyed_tiers`, REQ-38) in its N-hop form, so every
co-signature the SE ever issued on that child is accounted for. `transfer` routes to
`child_in_ladder_pay` on its own when the coin it selects is a child. See
[CHILDREN.md](../spec/CHILDREN.md).

## 6. The other coin shape: un-laddered

Not every coin is laddered, by design:

| Shape | Which coins | How it transfers |
|---|---|---|
| **Laddered** | every plain BTC deposit | co-sign a lower-CSV state; exit walks `T → X_m → S_k` |
| **Un-laddered** | RGB carriers; split sub-coins whose funding is un-broadcast | signed-once backup with decrementing `nLockTime`; transfer hands over the backup chain |

An **RGB carrier** is not laddered on the shipped configuration: a plain tier spend is sats-only and
would destroy the allocation (terminal freeze). `SdkConfig::colored_ladder` ships **false** in both
constructors, so a carrier takes the flat signed-once backup shape and moves by colored split plus
backup-chain handover. That path is load-bearing for assets. A **split sub-coin** whose funding
output is still un-broadcast cannot root a trigger, so it keeps the signed-once shape too.

`withdraw` and `unilateral_exit` exclude carriers from their sweep-everything defaults and hard-error
if you name one, rather than silently burning the allocation. Let `auto_exit_due` materialize the
carrier instead (`WalletEvent::TokenCarrierMaterialized`), or move the asset off the coin first.

An app can always ask which coins are flat: `flat_only_coins()` returns
`(statechain_id, raw_reason, may_still_be_transferred)` per coin, and `ladder_skip_reason` /
`ladder_skip_reason_raw` answer for one. The third element is what you act on — `true` means the
reason is structural and the coin still transfers on the flat lane; `false` means `transfer` will
refuse it — run `claim()` again, and if the reason persists the coin needs operator attention. A flat
**plain** coin's value is unaffected either way: it stays withdrawable and unilaterally exitable. A
flat **carrier** does not — both of those refuse it by design (above), and its route out is
materialization.

## 7. What a payment costs

Price the **leaf** lane. A root holder is the depositor, and after the first payment everyone
downstream holds leaves.

| leaf lane, per payment | block space | against ~154 vB on chain |
|---|---:|---|
| spent onward off-chain | **0 vB** | this is the product |
| swept and settled | **~105 vB** | 1.47× better — and this is the cap |
| walked out unilaterally | **250 – 2 719 vB** | worse than on-chain |
| shipped default | **418 vB** | 2.7× worse |

The walked range is the leaf's own exit chain, `293·d + 375` vB over `3 + 2d` sequential
transactions, topping out at the mainnet depth cap of 8. The design rule that falls out: **a piece
received and immediately cashed out should never have been an off-chain split.** What the system
sells is every payment *after* the first.

The discharge round that would change this by an order of magnitude
([SPEC.md](../spec/SPEC.md) §5.4) is **design, not built** — its SE enforcement point is empty.
Full model in [PARTIAL-PAYMENT-ECONOMICS.md](../spec/PARTIAL-PAYMENT-ECONOMICS.md).

## 8. Where next

- [Wallet SDK guide](wallet-sdk.md) — every operation, with examples.
- [Issuer SDK guide](issuer-sdk.md) — launch, mint, burn and distribute an RGB asset.
- [API reference](api-reference.md) — the full surface.
- [Testing guide](testing-guide.md) — the E2E suites and how to run them.
- [PROTOCOL.md](../spec/PROTOCOL.md) — tiers, renewal, splits, races, exit costs. *Normative.*
- [CHILDREN.md](../spec/CHILDREN.md) — first-class children and the per-hop census.
- [LIGHTNING.md](../spec/LIGHTNING.md) — both directions on the ladder via a HODL-invoice latch.
- [TRUST-MODEL.md](../spec/TRUST-MODEL.md) — verified versus trusted, party by party.
