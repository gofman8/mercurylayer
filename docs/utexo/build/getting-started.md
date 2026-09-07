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

### The enclave attestation identity — pinned for regtest, supplied elsewhere

The client verifies the enclave's signature-count attestation against a **pinned** identity, and the
resolution order is *compiled-in pin → configuration → refuse*. Never the key the coordinator
serves: verifying an attestation against a key its own counterparty hands you proves nothing.

**Regtest now ships a compiled-in pin** —
`TesrParams::attestation_identity_const("regtest")` returns
`TesrParams::REGTEST_ATTESTATION_IDENTITY`, the identity derived from the dev seed this repo commits
for its own stack. So on the local stack above you need configure nothing, and a compiled-in pin is
**not overridable**: if you set `SdkConfig::attestation_identity` or `UTEXO_ATTESTATION_IDENTITY` to
something that disagrees with it, that is an error rather than an override — a pin a config file can
override is a default, not a pin. Point the SDK at a differently-seeded lockbox and it refuses rather
than passes, which is the whole point.

```bash
# Only needed to check WHICH identity your lockbox has — the value is already compiled in for regtest.
curl -s http://127.0.0.1:18080/attestation_identity   # -> {"attestation_identity_pubkey":"0x…"}
```

**Every other network still ships `None`** — mainnet/bitcoin *and* every public testnet (testnet,
testnet3, testnet4, signet) — because no enclave is provisioned for any of them yet. There, on the
bare defaults, the SDK `claim()` establish pass **refuses**: it needs the coordinator's aggregate
through the attested `get_statechain_info`, that call fails with no identity to verify against, the
pass records `LadderSkipReason::AttestationIdentityUnpinned` and ladders **nothing**. That is the
correct direction to fail, and it is configuration you must supply —
`SdkConfig::attestation_identity` first, the `UTEXO_ATTESTATION_IDENTITY` environment variable as the
fallback (`ClientConfig::from_params`).

> ### ⚠️ On an unpinned network an SDK deposit is booked with NO exit material
>
> This is the sentence to read before you point a wallet at mainnet or a public testnet. The deposit
> is **booked** — it appears in the balance — and it has **no exit material at all**: it cannot be
> conveyed (`transfer_sender::execute` refuses a coin with no `tesr-` row by name) and it cannot be
> unilaterally exited (`unilateral_exit` refuses it by name too). **Cooperative withdrawal is the
> only route out**, and it needs the SE. Until 2026-09-06 the flat signed-once backup gave such a
> coin a unilateral exit that needed no attestation; that backup no longer exists, so nothing is left
> underneath.
>
> This is a **not-yet-deployable state, not a live regression**: no mainnet enclave is provisioned at
> all, so there is no mainnet deployment to regress. Do not read "deposits and exits work without a
> pin, only receiving does not" anywhere — that was true of the flat lane and is false now.
>
> Note which lane this is about. `coin_status::check_deposit` under `LadderAtSight::Plain` (the
> `mercuryrustlib` `update_coins` lane, used by the CLI and the upstream suite) ladders **without**
> consulting a pin at all — `tesr::establish_auto` / `cosign_tier` never call `get_statechain_info`.
> The refusal above is specific to the SDK `claim()` establish pass (`LadderAtSight::Defer`), which
> is the only lane a wallet user takes.

This pin is also what decides the coin shape for RGB carriers: `SdkConfig::colored_ladder` READS it
rather than stating a bool (see [§6](#6-coins-with-no-ladder)), so pinning a network's enclave turns
one coin shape on for it in the same move.

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
    // Create (or re-open — pass Some(mnemonic)) a wallet. On regtest the attestation identity is
    // COMPILED IN, so there is nothing to set here; on a network without a pin, set
    // cfg.attestation_identity (or export UTEXO_ATTESTATION_IDENTITY) or nothing gets a ladder.
    let cfg = SdkConfig::regtest("alice");
    let (wallet, mnemonic) = UtexoWallet::initialize(cfg, None).await?;
    println!("seed phrase: {mnemonic}");
    // The mnemonic alone is NOT a backup. Off-chain exit material — the pre-signed tier chain
    // (`tesr-*`, `ctesr-*`, `spinetip-*`) — lives only on disk. `export_recovery_bundle` is the
    // complete one, and it must be re-taken after every transfer, claim or split.

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
**unconditional** — `maintenance_plan` returns it for every configuration — and which, since
2026-09-06, has no laddered subject (its due-predicate reads `coin.locktime`, `None` for life); then
`defend_ladders`, once per new block, a no-op while `F` is unspent and the thing that races a hostile
trigger when it is not — the only defence a laddered coin needs; then `auto_exit_due` when
`SdkConfig::auto_exit` is set (it ships **on**; legacy `branch-` rows are its only subject). A pass that cannot *see* fails closed and says so —
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

**Since 2026-09-06 that is a claim about the whole coin.** No coin carries a flat backup chain: no
absolute-locktime backup is co-signed at deposit (`create_tx1` is deleted; the ladder is co-signed at
the first mempool sighting of the funding transaction) or at any hop, so there is no `min(L_k)`, no
height a prior owner holds, and `coin.locktime` is `None` for life. There is no calendar for your
app to watch — `deadline_safety_due` still runs on every background tick and has no laddered
subject. What is finite is the hop budget (576 whole-coin hops per depth level), and the on-chain
cadence is one ~112-vB cooperative re-anchor at that cap, reached by hops and never by time.
*(The "10 000 blocks ≈ 69 days, 100 per hop, ≈ 589 vB per coin-year" figures that stood here
described the retired chain.)*

- **Transfer** = the SE co-signs a fresh state one δ **lower** than the one it replaces
  (replace-by-lower-timelock), so the new owner's exit always matures first. The superseded state is
  disclosed and counted by the receiver's census at claim.
- **Renewal** (a lower-CSV extension) and, at `m_max`, **rollover** (an off-chain self-split onto a
  fresh level) are both free of the chain — the CSV budget is renewable without limit (as library calls —
  `renew_auto` / `rollover_auto` — that `transfer()` does not yet invoke; renewal is by hand today).
  What is finite is the number of hops per depth level; a re-anchor resets depth. The mainnet schedule is `d0 = 1440`, `δ = 36`, `d_floor = 144`, `e0 = 720`, `δE = 36`,
  `e_floor = 144`, `m_max = 15` (`TesrParams::mainnet`); regtest runs a scaled-down copy so an E2E
  lifecycle mines in seconds. Testnet and signet run the **mainnet** schedule, deliberately.
- **`refresh` is the re-anchor primitive**: one SE-co-signed, single-input on-chain tx moving the
  coin into a fresh deposit aggregate — new `statechain_id`, same owner, a full ladder of its own —
  which permanently invalidates every exit right rooted at the old outpoint, every prior owner's
  retained trigger and every old tier included. It is **not** a deadline reset (the CSV chain never
  matures while idle, and there is no calendar to reset); it resets depth and the hop budget, which
  makes it the answer for a coin that has spent most of its hop budget. The fee comes out of the coin
  (single input, blind SE); `refresh_sponsored` rebates it off-chain.
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

There is **no separate flat backup chain** (RETIRED 2026-09-06, INV-5 with it). `initlock` /
`interval` (mainnet 10 000 / 100, regtest 1 000 / 10) survive in `/info/config` and
`TesrParams::flat_ladder_params` only as compatibility constants: `initlock` is the fixed exit window
the split-depth cap measures a leaf's walk against, `interval` is applied to nothing. Broadcasting
`T` spends `F` and pre-empts every other spend of it — the copies of `T` in prior owners' hands carry
no timelock either, so the current owner or their watcher can always move first.

The full treatment is [PROTOCOL.md](../spec/PROTOCOL.md); what is verified versus trusted is
[TRUST-MODEL.md](../spec/TRUST-MODEL.md).

## 5. Non-exact amounts: the in-ladder split

Payments are arbitrary amounts, and no coin set can be made fine enough for a subset sum to land on
one — so the ordinary payment is an **in-ladder split**: a state tier `SP` spending `X_m.out[0]` (a
*descendant* of the trigger, never a rival for `F`) funding a piece child for the recipient and a
change child for you. The piece bundle is conveyed straight to the recipient's mailbox with the
standard key handover.

**How small a payment can be, on the plain in-ladder root lane: one satoshi.** The payee's leg no
longer has to fund two rungs, because it no longer has to *be* a two-rung child. What the leg's value
buys is decided by `mercurylib::tesr::LeafShape::for_value(value, rate, DUST_LIMIT)`, and the SDK's
admission floor for a payee's leg is the cheapest of those bands —
`SplitLegRole::Tail.min_value(…)` = **1 sat** (REQ-83):

| Leg value at the shipped 3.0 sat/vB | Shape built | What it funds |
|---|---|---|
| ≥ 1 560 (`min_child_value`) | `Piece` — extension + state | two rungs; exits unaided, two renewals |
| 945 – 1 559 (`min_spine_tip_value`) | `ThinPiece` — one cap rung | one rung; exits unaided, one renewal |
| 330 – 944 (`DUST_LIMIT` up) | `Ladderless` stub — `SP.out[j]` pays the payee's own key | no rung; leaves on its group's exit |
| 1 – 329 | `Tail` — sub-dust, coin-backed, zero fee | no rung; off-chain spendable, swept as fee credit if the tree settles |

`min_child_value` = `2·(committed_fee + P2A) + dust` = `2·(375 + 240) + 330` = 1 560 and
`min_spine_tip_value` = `375 + 240 + 330` = 945 are still the *band boundaries*, and they are
functions of the rate, not constants: quote one with its rate or not at all.

Two things to hold onto. First, **the sender's change leg still carries a ladder** and is floored at
`max(min_split_output(backup_rate), 945)` on the root and spine lanes (`min_child_value` on the
child lane) — which is why the SDK resolves a floor **per leg**, and why a payment can be refused for
the change rather than the piece. Second, **only the plain in-ladder ROOT lane actually builds the two
lower bands.** `spine_batch_split` refuses a `Stub`- or `Tail`-band leg by name ("this lane carries
no ladderless legs — pay it from the plain root lane"), and the coloured lane refuses a ladderless
leg and a coloured tail by name (`verify_ladderless_leaf`, `refuse_coloured_tail`); nothing is
co-signed by those refusals.

> ⚠️ **The child lane is the exception, and it is a hazard, not a feature.**
> `child_in_ladder_split` does not consult `LeafShape` at all — it hard-codes
> `SplitLegRole::Piece` for every grandchild — while the SDK still admits a payee's leg at 1 sat on
> that lane. So paying an amount under 1 560 sat *out of a received child* is admitted and then built
> as a two-rung shape it cannot fund. Until that is reconciled, keep payments out of a received child
> at or above `min_child_value`. Reported as a code defect, not fixed here.

Ask before you commit: `quote_transfer(amount)` returns a `TransferQuote` with `network_fee_sats`,
`renewal_fee_sats`, `total_fee_sats`, `fundable`, `stuck_coins` and `no_exit_material_coins`, and it
runs the *same* planner `transfer` executes, so `fundable: true` followed by a refusal is not
expressible.

Received children are **first-class**. The claim completes the SE key handover, so the receiver
co-owns `A_child` — that key is invariant across the rotation, which is what keeps the pre-signed
child tiers valid — and the sender is permanently locked out. A child pays onward off-chain, whole
or split again, one co-signature per hop, and each receiver runs the same exact-equality census
(`child_num_sigs == conveyed_tiers + superseded`, flat term zero by construction, REQ-38) in its N-hop form, so every
co-signature the SE ever issued on that child is accounted for. `transfer` routes to
`child_in_ladder_pay` on its own when the coin it selects is a child. See
[CHILDREN.md](../spec/CHILDREN.md).

## 6. Coins with no ladder

A coin either carries a ladder — a laddered root, a received child or a spine tip — or it carries
none, and the second case is a state to repair rather than a lane to route:

| Shape | Which coins | How it transfers |
|---|---|---|
| **Laddered** | every plain BTC deposit — and, wherever `colored_ladder` is on, every RGB carrier too | co-sign a lower-CSV state; exit walks `T → X_m → S_k` |
| **No ladder** | a coin `claim()` declined to ladder (with a recorded `LadderSkipReason`) | **it does not** (2026-09-06): it has no exit material and no conveyance — no flat backup exists, and `transfer` refuses it by name. Run `claim()` again; the reason says what to repair |

An **RGB carrier** cannot take a *plain* ladder: a plain tier spend is sats-only and would destroy
the allocation (terminal freeze). What it can take is a **coloured** one, and
`SdkConfig::colored_ladder` decides — by reading the network's pinned attestation identity rather
than stating a bool, because the coloured lane establishes its ladders through `claim()` and
`claim()` refuses without a pin. Regtest has a pin, so a carrier there is laddered like any other
coin. Mainnet has none, and there is **no second lane behind that**: the legacy colored split plus
backup-chain handover is retired — `register_split_subcoins_n` and `register_combine_subcoins` refuse
by name, and `refuse_legacy_colored_split_lane` refuses on *both* settings of the flag — so a carrier
on a network still waiting for its enclave is holding-only, with no exit material and no way to move
the asset off it.

A **split sub-coin** whose funding output is un-broadcast still cannot root a trigger — colouring a
tier cannot broadcast a funding output — so it is never given a root ladder; it is laddered by its
parent's split instead (`establish_child` gives every in-ladder piece its own two rungs where its
value affords them). What changed is the *route*: the plain off-chain split that used to carve such
coins for plain BTC payments is DELETED. It spent the coin's funding output `F` directly, and `F` is
what a prior owner's retained, un-timelocked trigger also spends — so that owner could void the split
and destroy the payee's sub-coin, with no way for the payee to detect the exposure. Every payment now
carves in-ladder, as a **descendant** of the trigger rather than a rival for `F`, and the hazard is
closed by construction. There is no "signed-once shape" left for anything to keep.

`withdraw` and `unilateral_exit` exclude carriers from their sweep-everything defaults and hard-error
if you name one, rather than silently burning the allocation. A carrier still holding legacy `branch-` rows
from before 2026-09-06 is materialized by `auto_exit_due` (`WalletEvent::TokenCarrierMaterialized`);
otherwise move the asset off the coin first;
where the ladder is coloured, `unilateral_exit` opens and walks it.

An app can always ask which coins have no ladder: `flat_only_coins()` returns
`(statechain_id, raw_reason, may_still_be_transferred)` per coin, and `ladder_skip_reason` /
`ladder_skip_reason_raw` answer for one. The third element is **always `false`** now
(`is_legitimate_flat_reason` answers `false` for every reason): there is no flat lane for any coin to
transfer on (2026-09-06), so `transfer` will refuse it — run `claim()` again, and if the reason
persists the coin needs operator attention. Such a coin's value is not lost, but it has **no exit
material** until it is laddered: a plain one stays withdrawable cooperatively; a carrier is neither
withdrawable nor exitable (both refuse it by design, above) and — unless it carries legacy `branch-`
rows from before the rule — has no materialization route either (TRUST-MODEL B12).

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
