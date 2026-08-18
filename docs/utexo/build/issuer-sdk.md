# Issuer SDK guide

Token issuance is part of `UtexoWallet`. Any wallet can be an issuer — RGB has no privileged issuer
role beyond holding the contract's issuance rights.

Token calls need RGB configured (`rgb_proxy_url` + `rgb_data_dir`), otherwise you get
`SdkError::TokensNotConfigured`. `SdkConfig::regtest(name)` fills both in; `SdkConfig::mainnet(…)`
leaves them `None` for you to set.

> **Which shape your carrier gets depends on whether its network has an enclave.** Every plain BTC
> deposit is laddered at claim — trigger → extension → state, relative CSV, pre-signed and
> un-broadcast (see [getting started §4](getting-started.md)). A plain tier spend over an RGB
> **carrier** is sats-only and would destroy the allocation (terminal freeze,
> [PROTOCOL.md](../spec/PROTOCOL.md) §5.10), so the carrier needs a *coloured* ladder or none at all
> — and that is what `SdkConfig::colored_ladder` decides.
>
> It is no longer a stated bool. Both constructors READ the network's pinned attestation identity
> (`TesrParams::attestation_identity_const`), because the coloured lane establishes its ladders
> through `claim()` and `claim()` refuses without a pin: true-without-a-pin would ship a wallet whose
> token lane refuses forever. So **regtest is on** — a carrier there is laddered like any other coin,
> coloured, via `build_colored_ladder_auto` — and **mainnet is off**, not as a judgement about the
> lane but because no mainnet enclave is provisioned yet. Pin one and it flips itself.
>
> Where it is off, the carrier keeps the **flat signed-once backup** and moves by colored split plus
> backup-chain handover. That is why the whole of
> [Over time](#over-time-what-you-and-your-holders-actually-watch) has so much to say about absolute
> locktimes: a flat carrier has no CSV tier chain, so its exit is not calendar-free the way a laddered
> coin's is. `sdk02`, `sdk29`, `sdk31`, `sdk32`, `sdk34`, `sdk74`, `sdk75`, `sdk77` and `sdk78` set
> the flag by name; `sdk09`, `sdk36`, `sdk39`, `sdk52` and `sdk73` inherit whatever the constructor
> derives.

## Launch a token (NIA — fixed supply)

```rust
let (issuer, _) = UtexoWallet::initialize(SdkConfig::regtest("issuer"), None).await?;

// 1. Fund the RGB engine (one-time): issuance needs colorable UTXOs + witness fees.
let fund = issuer.get_token_funding_address().await?;   // get_token_l1_address is an alias
// send sats to `fund` and confirm…

// 2. The carrier occupies a statechain slot like any deposit — it consumes one deposit token.
issuer.add_prepaid_token(&token_id).await;   // or handle SdkError::TokenPaymentRequired and retry

// 3. Issue. The full supply lands on a fresh statechain coin: the asset's CARRIER.
let asset_id = issuer.issue_token("DEMO", "Demo Token", /*precision*/ 2, /*supply*/ 1_000_000).await?;
println!("token id: {asset_id}");   // rgb:…  — share this as the token identifier
```

**How much to send to the funding address.** The engine sizes every colorable UTXO it makes at
`TOKEN_CARRIER_SATS × 4`. An NIA issuance makes one; an IFA makes `inflation_amounts.len() + 2` —
one per allocation (the fungible supply and each inflation right, since `max_allocations_per_utxo`
is 1) plus a spare for the fund and witness transactions. Everything left over pays for those
transactions. On regtest the live flows use 100 000 sats for a plain NIA issuance (`sdk02`,
`sdk52`), 500 000 when a mint follows (`sdk09`) and 600 000 for repeated distribution (`sdk39`).

Two on-chain transactions get an asset onto a statechain: the engine's colorable-UTXO creation, and
the colored deposit that binds the supply to the carrier. After that confirms — `claim()`, or the
`DepositConfirmed` watcher event — the entire supply transacts off-chain (`sdk02`).

### The carrier is 22 536 sats, and that number is derived

`TOKEN_CARRIER_SATS` = `legacy_carrier_sats(LEGACY_CARRIER_SEND_DEPTH)` =
`5 · (TOKEN_PIECE_SATS + 300) + 666` = **22 536**. Each flat send consumes one
`TOKEN_PIECE_SATS`-sized piece plus a floored fee reserve, and the last change must still clear the
sub-coin floor `min_split_output` (666 sat at the committed 3 sat/vB). So a stock carrier affords
**five chained sends** on the legacy flat lane — the size is derived from that lane's depth, and it
is kept for the networks still on it. On the coloured lane the same 22 536 buys exactly **one** send,
with the rest landing in a depth-1 change child that can only be moved whole or exited — a real cost
of that lane, not a rounding.

`issue_token_sized`, `issue_inflatable_token_sized` and `mint_tokens_sized` take the carrier's sats
as an argument. They exist to reproduce and migrate **under-sized** carriers — a carrier funded
below the coloured root floor can never be laddered, and the migration hatch that serves that class
needs a way to construct it — not as a knob for ordinary issuance.

Two practical consequences of the carrier being a special coin:

- **Its sats are not spendable BTC.** Carrier value is excluded from `Balance::available_sats` /
  `pending_sats` / `in_transfer_sats` and surfaces only through `Balance::tokens`, because a
  plain-BTC spend of that outpoint would destroy the allocation. Budget the 22 536 sats as part of
  the asset, not as change. (`sdk09` counts confirmed coins rather than sats for exactly this
  reason.)
- **It never gets a *plain* ladder.** Where `colored_ladder` is on it gets a coloured one and
  `LadderEstablished` fires; where it is off no ladder event fires at all and `claim()` records
  `LadderSkipReason::RgbCarrier` instead, readable back through `ladder_skip_reason` /
  `flat_only_coins`. Read the `may_still_be_transferred` element rather than assuming it: that reason
  used to license a flat whole-coin conveyance and **no longer does** — the licence was retired with
  the un-laddered shape, on the ground that "this coin is an RGB carrier" stops being a reason a coin
  may travel without a ladder once carriers are laddered. Distributing the asset is a colored split
  (below), not a whole-coin handover, so it is a different question from this one.

## Inflatable supply (IFA): mint and burn

```rust
// 1000 units now, plus a 500-unit inflation right reserved for later.
let asset = issuer.issue_inflatable_token("IFT", "Inflatable Token", 0, 1_000, vec![500]).await?;

// Realize the inflation right. This IS on-chain — inflation is a contract state transition and
// there is no off-chain variant: one inflate tx in the RGB engine, then the minted supply is bound
// to a FRESH carrier coin (another colored deposit). mint_tokens waits for the inflate to confirm,
// so the chain must be advancing (on regtest: run a miner).
let (inflate_txid, minted) = issuer.mint_tokens(&asset, vec![500]).await?;   // minted == 500

// Burn engine-held (free) balance — also on-chain. Supply already bound to a statechain carrier
// must be exited back into the engine first.
let burn_txid = issuer.burn_tokens(&asset, 100).await?;
```

IFA issuance creates one colorable UTXO per allocation — the fungible supply and each inflation
right — before issuing (SPEC REQ-19), and binding the supply consumes only the fungible allocation,
never the reserved right (INV-12). `mint_tokens` snapshots the allocation set *before* inflating and
binds only what is new, so a mint can never consume already-bound supply (REQ-20).

An NIA has no post-issuance mint: its supply is fixed by the contract at issuance. Issue an IFA if
you need inflation, and declare the rights up front — holders read them off the contract. Issue →
mint → distribute is verified end to end by `sdk09`.

After a mint you hold the asset across **two carriers**. That is normal; distribution handles it
(see combine, below).

## Distribute

```rust
// Single recipient — off-chain, instant.
issuer.transfer_tokens(&asset_id, &user_address, 5_000).await?;

// Many recipients — ONE SE-co-signed colored split carves one exact piece per recipient plus your
// change; each piece ships its own consignment. One TransferResult per recipient, in order.
let results = issuer
    .batch_transfer_tokens(&asset_id, &[(bob_address.clone(), 200), (carol_address, 300)])
    .await?;
```

**Which lane those two calls take is decided by `colored_ladder`, and the rest of this section
describes the flat one.** Where the flag is on, the carrier is laddered and `transfer_tokens` runs the
**coloured in-ladder split** instead: a coloured `SP` over `X_m`'s payload output — a descendant of
the trigger, never a rival for `F` — carving a coloured child per recipient, each with its own
headless coloured ladder (`sdk02`, `sdk29`, `sdk77`). Two differences worth knowing before you read
on: there is no combine transaction on that lane (`sdk31` — each carrier's `F` is already spent by
its own trigger, so paying across carriers is one in-ladder split per carrier), and a received
coloured child carries no `branch-` row at all, its exit material being the five-tier `ctesr-` chain.

What a distribution does on the **flat** lane: every input carrier is terminalized at the SE (one
final co-signature), the colored split is co-signed once, and each piece becomes a fresh sub-coin
carrying `TOKEN_PIECE_SATS` = **4 074** sats of packaging plus the exact token amount as payload.
Each piece gets its **own fresh signed-once backup at the full initial locktime** — no ladder
mechanics enter this path, which is why an issuer keeps distributing no matter how long the carrier
has been sitting. Holders receive, validate the consignment client-side, and re-transfer with the
standard wallet SDK; there is no issuer involvement after issuance.

**Why 4 074 and not a round number.** The piece is a coin like any other: its receiver claims it,
and if the carrier is coloured that claim wants a coloured root ladder. A coloured rung costs
`ceil(colored_tier_vbytes(1) · rate) + P2A` = 744 sat at the committed 3 sat/vB, so a coloured root
ladder floor is `3 · 744 + 330` = 2 562 and a coloured child floor `2 · 744 + 330` = 1 818.
`TOKEN_PIECE_SATS` is derived above the *root* floor with head-room for the committed rate doubling,
because a piece's sats are fixed the moment it is carved while the floor is not. Do not round it.

Slots for the pieces and the change are **derived** from the carrier — free SE vouchers — so
distributing does not burn paid onboarding tokens the way issuance does. The coordinator issues at
most `max_derived_tokens_per_statechain` of them per parent, counted over the parent's lifetime with
spent rows included (default 64), which bounds how many recipients one carrier can ever serve.

If no single carrier holds the amount — the normal case after a mint — the SDK **combines** several
carriers of the same asset into one SE-co-signed colored combine tx (N inputs → exact piece +
change), largest allocation first. The receiver then validates the multi-input branch and requires
**all N input carriers** to be terminal — one terminal ancestor per structural input, enforced by
`transfer_receiver::verify_terminal_parents` (`sdk31`). Token conservation across the whole
operation is INV-13: `Σ recipient amounts + change = Σ allocations of the combined inputs`.

A distribution is refused **before** anything is co-signed rather than stranding the carrier:

- the carrier must cover `TOKEN_PIECE_SATS + fee reserve`, where the reserve is 1 % of carrier value
  clamped to 300–2 000 sats;
- both outputs must stay above `min_split_output` at the *live* backup feerate — a sub-coin has to
  be able to fund its own backup. At high feerates the transfer errors out with the carrier
  untouched.

Token amounts are **raw u64 units**; `precision` is contract metadata the SDK never scales
(`sdk29`). `transfer_tokens(&asset, &addr, 5_000)` on a precision-2 asset moves 5 000 raw units =
50.00 display units.

## Query

```rust
let balances = issuer.get_token_balances().await?;
// [{ asset_id, ticker, name, precision, balance /* settled */, total /* incl. unsettled */ }]

let txs = issuer.query_token_transactions(&asset_id).await?;
// [{ kind, status, amount, txid }]

let where_it_sits = issuer.list_token_allocations(&asset_id).await?;
// [(outpoint, amount)] — the actual per-carrier bindings
```

Prefer `list_token_allocations` when the question is "is the allocation still on the coin I think it
is?". A balance is an aggregate computed from rgb-lib's tables and stays confidently wrong if the
stock has been invalidated underneath it; the allocation list is the per-outpoint truth.

## Semantics vs Spark's issuer SDK

| Spark | Here | Note |
|---|---|---|
| `createToken(isFreezable, maxSupply…)` | `issue_token` (NIA) / `issue_inflatable_token` (IFA) | metadata immutable, as in Spark; NIA supply fixed at issuance, IFA declares its inflation rights up front |
| `mintTokens` | `mint_tokens` | IFA only: on-chain inflate + bind to a fresh carrier (`sdk09`); an NIA declares no inflation right, so it has nothing to realize |
| `burnTokens` | `burn_tokens` | on-chain, engine-held balance only; statechain-bound supply must exit first |
| `transferTokens` | `transfer_tokens` | coloured in-ladder split where `colored_ladder` is on; colored off-chain split + backup-chain handover where it is off |
| `batchTransferTokens` | `batch_transfer_tokens` | one colored split, N pieces + change (`sdk09`) |
| `freezeTokens` | **intentionally absent** | client-validated assets have no consensus-meaningful freeze; see [tokens](../learn/tokens.md) |
| `getIssuerTokenBalance` | `get_token_balances` | |
| `getTokenL1Address` | `get_token_l1_address` | alias of `get_token_funding_address` |
| token id `btkn1…` | `rgb:…` contract id | |

## Trust properties for holders

- **Supply is bounded by the contract.** You cannot inflate an NIA at all, and an IFA only up to the
  inflation rights declared at issuance — both visible to every holder from the contract itself.
- **Transfers validate client-side from consignments.** The SE never vouches for token state. A
  receiver books the amount the consignment assigns to its *own* witness outpoint, treating the
  envelope's stated amount only as a cross-checked hint (REQ-21), and books it under the
  consignment's cryptographically-verified contract id rather than a sender-claimed one (REQ-22).
- **No superseded colored witness exists anywhere in the system.** A colored tx only ever spends
  outputs of terminalized structure — terminalization precedes the colored co-sign and the SE
  refuses renewal on a terminal node — so no ancestor of an RGB anchor is ever re-signed.
- **Plain sweeps of a carrier are refused, not silently destructive.** `withdraw` and
  `unilateral_exit` exclude carriers from their defaults and hard-error if a carrier is named
  explicitly. `refresh` (the on-chain re-anchor) likewise rejects a coin holding an allocation, and
  the maintenance refresh pass skips carriers.
- **A received piece SETTLES without the SE.** Token exit means **materializing the branch**:
  broadcasting the stored `branch-<id>` rows, which for a carrier *are* the un-broadcast coloured
  split/combine transactions — the RGB witnesses that carved the allocation. Landing them root-first
  settles the allocation on a confirmed outpoint and spends the shared root, with no SE involved. It
  works at depth: a piece two colored splits deep materializes by broadcasting `[split1, split2]`
  root-first (`sdk39`). The automatic route is `auto_exit_due`, which broadcasts the branch and only
  the branch — never the plain backup, which would sweep the sats and destroy the allocation.
  **Materializing settles the asset; it does not exit the coin.** The sats stay on the 2-of-2
  outpoint, so moving them onward still needs the SE. That is the flat lane's ceiling, and it is why
  the coloured ladder matters: where `colored_ladder` is on the carrier has a real unilateral exit
  (`sdk75` walks a coloured `T → X_0 → S_0` to confirmation with the allocation intact), and where it
  is off — a network with no enclave yet — it does not. `unilateral_exit` refuses this class by name
  and points at the two routes that do exist rather than returning a `complete` `ExitStatus` that
  would be a false green.
  (`materialise_carrier` is the manual call, and it is deliberately narrow: it serves only a carrier
  for which no coloured ladder can ever be built, so that a carrier which could still be laddered
  waits for its ladder instead of being settled early.)

## Over time: what you (and your holders) actually watch

Tokens are never lost by inactivity: `sdk32` idles the chain a "year" past every deployed horizon
and the stock still validates the full allocation afterwards. Read its lane label, though — `sdk32`
asks for `colored_ladder = true`, so it measures the *coloured* form of that claim. The flat carrier
shape is the one `sdk52` pins (plain coin laddered, carrier not) and `sdk39` exits; that is now the
shape of a network still waiting for its enclave rather than of the regtest default.

A calendar exists here — but not *only* here. A plain laddered coin also keeps an absolute flat
backup chain and therefore a `min(L_k)` its prior owners hold; what a carrier with no ladder lacks is
the CSV tier chain that makes the *exit* calendar-free. So the difference is the remedy available,
not the existence of a deadline.

**Your issued or minted carrier is flat**: funded on chain, with no ancestor above it. It has no
ladder to age on and no ancestor that could claw it back, so it carries no deadline duty at all —
`auto_exit_due` inspects it and correctly skips it. It does hold its one signed-once deposit backup,
maturing at `deposit_height + initlock` (10 000 blocks on the mainnet, testnet and signet profile;
1 000 on regtest), but that maturity does not gate distribution: a colored split never touches a
ladder and mints fresh full-locktime backups for its outputs. The standing limitation is
cooperative-only movement — a plain unilateral exit is refused, so an issued carrier's supply stays
where it is until the SE co-signs a colored spend.

**A holder's received piece on the flat lane is a sub-coin carrier** — held off the ladder twice
over: a carrier (terminal freeze) *and* a split sub-coin whose funding is un-broadcast, which cannot
root a trigger. That second half is permanent and survives every change to the first: colouring a
tier cannot broadcast a funding output, so an off-chain sub-coin's funding stays un-broadcast on
every lane, and its `branch-` rows stay its only way down. It can be
materialized SE-free at any time while the shared root is unspent. It does have a root deadline:
past it, the *sender's* signed-once backup matures and a malicious sender could sweep the shared
funding. Holders using this SDK are protected by default — the `auto_exit_due` watchtower
(`SdkConfig::auto_exit`, run by `start_background`) materializes the carrier as the deadline nears —
broadcasting **only** the stored exit branch, never the sats-sweeping backup — and emits
`WalletEvent::TokenCarrierMaterialized` (SPEC REQ-33). An issued/flat carrier has no exit branch, so
the pass verifies that and skips it. `sdk34` is the E2E for the duty, and it runs with
`colored_ladder = true`: there a received piece is a coloured child with no `branch-` row at all, so
the same duty is discharged by walking its pre-signed tier chain instead. The duty and the event are
the same; the action is what the lane decides.

The duty is delegable, keyless, via `export_watch_bundle` (`sdk45`). The exported bundle carries
**no key material**, a second independent tower is idempotent, and a `WatchEntry` with
`token_carrier: true` is exported *without* its `backup_tx` — a delegated tower can only ever
materialize the branch, never sweep and destroy the allocation. The export fails closed for a token
wallet whose carriers cannot be enumerated, because a carrier mis-exported as plain would hand the
tower a token-destroying backup.

**Advice worth putting in your holder docs:** hold, combine, or materialize — never "refresh before
your locktime expires" (refresh does not apply to carriers) and never a plain unilateral exit. Warn
too that a lone piece can be below the bar to move on its own: escaping a carrier costs
`TOKEN_PIECE_SATS + fee_reserve + min_split_output` = **5 040 sats** of aggregated carrier value, so
a holding under that must be combined with another piece of the same asset first.

## Where next

- [Wallet SDK guide](wallet-sdk.md) — every holder-side operation, with examples.
- [API reference](api-reference.md) — the full surface.
- [Tokens on RGB](../learn/tokens.md) — the conceptual model, the freeze rationale, exit behaviour.
- [Granularity deep dive](../learn/granularity-deep-dive.md) — colored splits, raw units vs
  precision, the piece floor, exits at depth.
- [SPEC.md](../spec/SPEC.md) §7 — tokens normatively: REQ-19…22, INV-12, INV-13, INV-29.
- [PROTOCOL.md](../spec/PROTOCOL.md) §5.10 — RGB integration and the terminal-freeze rules.
- [PARTIAL-PAYMENT-ECONOMICS.md](../spec/PARTIAL-PAYMENT-ECONOMICS.md) — what the coloured lane
  costs, measured.
- [Testing guide](testing-guide.md) — running the token E2Es (`sdk02`, `sdk09`, `sdk29`, `sdk31`,
  `sdk32`, `sdk34`, `sdk36`, `sdk39`, `sdk52`, `sdk78`).
