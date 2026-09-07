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
> Where it is off, the carrier has **no ladder and nothing underneath it** (2026-09-06): no flat
> signed-once backup exists for any coin (`create_tx1` is deleted), the legacy colored split plus
> backup-chain handover cannot complete (`refuse_legacy_colored_split_lane` refuses ahead of it on
> BOTH settings of the flag, and `register_split_subcoins_n` / `register_combine_subcoins` refuse by
> name behind it), and there is no calendar on any coin. Such a carrier is **holding only**, with no
> exit material, until an identity is pinned and a later `claim()` pass colours it (TRUST-MODEL
> B12) — see [Over time](#over-time-what-you-and-your-holders-actually-watch). `sdk02`, `sdk29`,
> `sdk31`, `sdk32`, `sdk34`, `sdk74`, `sdk75` and `sdk77` set the flag by name; `sdk09`, `sdk36`,
> `sdk39` and `sdk52` inherit whatever the constructor derives. (`sdk73` and `sdk78`, which used to
> appear in these lists, were DELETED with the retired lane.)

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
sub-coin floor `min_split_output` (666 sat at the committed 3 sat/vB). So a stock carrier was sized for
**five chained sends** on the legacy flat lane — the size is derived from that lane's depth, and the
constant is kept although the lane is retired (2026-09-06: no network is on it). On the coloured lane the same 22 536 buys exactly **one** send,
with the rest landing in a depth-1 change child that can only be moved whole or exited — a real cost
of that lane, not a rounding.

`issue_token_sized`, `issue_inflatable_token_sized` and `mint_tokens_sized` take the carrier's sats
as an argument. They exist to reproduce **under-sized** carriers — a carrier funded below the
coloured root floor (`colored_ladder_floor` = `3 · 744 + 330` = **2 562** at the shipped 3 sat/vB)
can never be laddered — not as a knob for ordinary issuance. Note that the migration hatch these were
originally written to feed is **closed**: `refuse_legacy_colored_split_lane` refuses that lane on
both settings of `colored_ladder`, so there is now no route that serves an under-sized carrier.
Re-funding above the floor is the only answer.

Two practical consequences of the carrier being a special coin:

- **Its sats are not spendable BTC.** Carrier value is excluded from `Balance::available_sats` /
  `pending_sats` / `in_transfer_sats` and surfaces only through `Balance::tokens`, because a
  plain-BTC spend of that outpoint would destroy the allocation. Budget the 22 536 sats as part of
  the asset, not as change. (`sdk09` counts confirmed coins rather than sats for exactly this
  reason.)
- **It never gets a *plain* ladder.** Where `colored_ladder` is on it gets a coloured one and
  `LadderEstablished` fires; where it is off no ladder event fires at all and `claim()` records
  `LadderSkipReason::RgbCarrier` instead, readable back through `ladder_skip_reason` /
  `flat_only_coins`. The `may_still_be_transferred` element is **always `false`** now
  (`is_legitimate_flat_reason` returns `false` unconditionally): that reason used to license a flat
  whole-coin conveyance and no longer does, and there is no other lane behind it. The one carrier
  state that is worse is `PlainLadderOverCarrier` — tokens moved onto an outpoint that was already
  plain-laddered. Its plain trigger is a plain spend of a sealed output, its tiers cannot be
  unsigned, `colored_reanchor` refuses a plain ladder by name and a plain `refresh` would destroy the
  allocation: **there is no remedy**, and the allocation is stranded. Under laddering-at-first-sight
  every confirmed plain deposit is a plain-laddered outpoint, so never move an allocation onto one.

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
mint → distribute was verified end to end by `sdk09` on the retired flat lane (re-derivation to the
coloured in-ladder split pending).

After a mint you hold the asset across **two carriers**. That is normal; distribution handles it
(see combine, below).

## Distribute

```rust
// Single recipient — off-chain, instant. This is the only shape that works today.
issuer.transfer_tokens(&asset_id, &user_address, 5_000).await?;

// Many recipients — REFUSED. See below: `refuse_colored_multi_payee` refuses K > 1 by name on the
// coloured lane, and the legacy N-piece lane behind it is retired. Pay one carrier per recipient.
let results = issuer
    .batch_transfer_tokens(&asset_id, &[(bob_address.clone(), 200)])
    .await?;
```

**Distribution runs on the coloured in-ladder lane, and there is no second lane behind it.** Where
`colored_ladder` is on, the carrier is laddered and `transfer_tokens` runs the **coloured in-ladder
split**: a coloured `SP` over `X_m`'s payload output — a descendant of the trigger, never a rival for
`F` — carving a coloured child for the payee plus a coloured spine tip for you, each with its own
headless coloured ladder (`sdk02`, `sdk29`, `sdk77` — all pending run). Where it is off, the call
**refuses**: `refuse_legacy_colored_split_lane` refuses the legacy lane on *both* settings of the
flag, so there is nothing to fall through to.

Two properties of the coloured lane worth knowing before you design around it. **There is no combine
transaction** (`sdk31`): each carrier's `F` is already spent by its own trigger, so paying across
carriers is one in-ladder split per carrier, executed sequentially and **not atomically** — a failure
on leg `k` leaves legs `0..k` conveyed, and the error names every piece already handed over. And a
received coloured child carries **no `branch-` row at all**; its exit material is the five-tier
`ctesr-` chain.

**`batch_transfer_tokens` is one recipient only.** Every coloured send funnels through
`colored_in_ladder_pay`'s engine, whose first act is `refuse_colored_multi_payee`, and that refuses
`K > 1` by name. This is a **shipped decision** (D43, taken 2026-08-13), not a block pending a fix:
the coloured lane conveys its pieces serially after the carrier is already terminal and journals no
`recipient_address`, so a failure at payee `j` strands pieces `j..K` with the sender holding their
keys and no route to the recipients. Size carriers to the value you intend to move and pay one
carrier per recipient. The legacy lane that DID serve `K > 1` — one `create_colored_split_tx` over
`F` carving `TOKEN_PIECE_SATS` = **4 074**-sat pieces, each with its own signed-once backup — is
retired: it is refused before any co-sign, and the sub-coins it made were exited by a flat backup
chain that no longer exists.

Holders receive, validate the consignment client-side, and re-transfer with the standard wallet SDK;
there is no issuer involvement after issuance.

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

If no single carrier holds the amount — the normal case after a mint — there is **no combine
transaction**. `colored_multi_carrier_transfer` runs one coloured in-ladder split per carrier and
conveys one coloured child per leg to the same recipient, who books them as separate allocations that
sum to the amount. That is not a workaround for a missing combine: each carrier's `F` is already
spent by its own coloured trigger, and no coloured tier has several parents (`SP` spends exactly one
`X_m`). RGB value conservation holds per split and the recipient's balance is the sum, which is what
the legacy combine delivered — but the legs are **not atomic**, and the error names every leg already
conveyed so the caller can finish or refund deliberately. `sdk31` is the flow, re-derived onto this
shape and **pending run**. The legacy multi-input combine behind it (whose receiver required all N
input carriers to be terminal, `transfer_receiver::verify_terminal_parents`) is refused before it is
reached.

A distribution is refused **before** anything is co-signed rather than stranding the carrier — the
coloured lane checks its floors and its `K > 1` rule ahead of the first SE call, and the legacy lane
is refused outright. On the legacy lane the checks were: the carrier covering
`TOKEN_PIECE_SATS + fee reserve` (the reserve being 1 % of carrier value clamped to 300–2 000 sats),
and both outputs staying above `min_split_output` at the *live* backup feerate. Those still run, but
only after a refusal that always fires first.

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
| `transferTokens` | `transfer_tokens` | coloured in-ladder split where `colored_ladder` is on; where it is off the legacy colored split + backup-chain handover cannot complete (`register_split_subcoins_n` refuses by name, 2026-09-06) |
| `batchTransferTokens` | `batch_transfer_tokens` | **one recipient only.** A coloured carrier refuses `K > 1` by name (`refuse_colored_multi_payee`, shipped decision D43); the legacy one-split-N-pieces lane behind it is retired and refuses first (`refuse_legacy_colored_split_lane`, then `register_split_subcoins_n` / `register_combine_subcoins`). The call still exists and still works for `K = 1` (`sdk09`: premise retired, re-derivation pending) |
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
- **A received piece SETTLES without the SE.** On the coloured lane that is the five-tier walk of its
  `ctesr-` bundle, moving the allocation to the holder's own key (`sdk75`). For a piece from the
  retired flat lane (before 2026-09-06) it meant **materializing the branch**:
  broadcasting the stored `branch-<id>` rows, which for a carrier *are* the un-broadcast coloured
  split/combine transactions — the RGB witnesses that carved the allocation. Landing them root-first
  settles the allocation on a confirmed outpoint and spends the shared root, with no SE involved. It
  works at depth: a piece two colored splits deep materializes by broadcasting `[split1, split2]`
  root-first (`sdk39`). The automatic route for such legacy rows is `auto_exit_due` (legacy subject only), which
  broadcasts the branch and only the branch; there is no plain backup to sweep with.
  **Materializing settles the asset; it does not exit the coin.** The sats stay on the 2-of-2
  outpoint, so moving them onward still needs the SE. That was the flat lane's ceiling, and it is why
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
asks for `colored_ladder = true`, so it measures the *coloured* form of that claim. The no-ladder carrier
shape is the one `sdk52` pins (plain coin laddered, carrier not — and since 2026-09-06 with nothing
underneath) and that `sdk39` used to exit through the retired branch lane (re-derived, pending run);
it is the shape of a network still waiting for its enclave, not of the regtest default.

No calendar exists here — or anywhere (2026-09-06). No coin, plain or carrier, keeps a flat backup
chain, so there is no `min(L_k)` a prior owner holds and nothing on a coin matures on its own; what a
carrier with no ladder lacks is not a calendar-free exit but **any** exit (TRUST-MODEL B12). So the
difference between the lanes is whether exit material exists, not the existence of a deadline.

**Your issued or minted carrier without a coloured ladder is a coin with no exit material**: funded
on chain, with no ancestor above it and no ancestor that could claw it back, so it carries no
deadline duty at all — `auto_exit_due` inspects it and correctly skips it. It holds no signed-once
deposit backup (none is co-signed for any coin; `deposit_height + initlock` matures nothing, and
`initlock` survives only as a compatibility constant). Distribution from it cannot complete either:
the colored split that used to mint fresh-backup outputs dies at `register_split_subcoins_n`. The
standing limitation is therefore holding only, until an identity is pinned and a later `claim()`
pass colours the carrier — after which it pays by coloured in-ladder split and walks its own ladder.

**A holder's received piece from the retired flat lane (before 2026-09-06) is a sub-coin carrier** —
held off the ladder twice over: a carrier (terminal freeze) *and* a split sub-coin whose funding is
un-broadcast, which cannot root a trigger. That second half is permanent and survives every change
to the first: colouring a
tier cannot broadcast a funding output, so an off-chain sub-coin's funding stays un-broadcast on
every lane. Its `branch-` rows are the only SE-free thing it can still do — and what they do is
SETTLE THE ALLOCATION, not exit the coin: `unilateral_exit` has no arm that broadcasts them and
refuses such a coin by name, and `has_exit_material` no longer counts them. It can be
materialized SE-free at any time while the shared root is unspent — and it has **no** root deadline
(2026-09-06): no sender holds a signed-once backup that matures, so nothing can sweep the shared
funding on a date. The only spends of the shared root in anyone's hands are un-timelocked triggers,
which a holder's watcher answers by walking, and superseded states that lose the CSV race. For such a
legacy piece the `auto_exit_due` watchtower (`SdkConfig::auto_exit`, run by `start_background`) still
materializes the branch — only the branch — and emits `WalletEvent::TokenCarrierMaterialized` (SPEC
REQ-33 is RETIRED; this arm reads rows no lane mints). An issued carrier has no exit branch, so the
pass verifies that and skips it. `sdk34`, which drove the duty against a deadline, is re-derived to
the event-driven defence — pending run; on regtest a received piece is a coloured child with no
`branch-` row at all, defended by `defend_ladders` walking its pre-signed tier chain when the
parent's `F` is spent.

The duty is delegable, keyless, via `export_watch_bundle` (`sdk45`). The exported bundle carries
**no key material**, a second independent tower is idempotent, and every `WatchEntry` it can emit is
exported *without* a `backup_tx` (none exists) and event-driven (`deadline_block: u32::MAX`) — a
delegated tower can only ever drive the pre-signed walk, never sweep and destroy the allocation.
Note what that costs. Since the height-keyed arm was deleted, a coin whose only material is a legacy
`branch-` chain gets **no entry at all**: it is omitted from the bundle and reported by
`flat_only_coins` instead, so materializing such a piece is the owner's own job
(`materialise_carrier`, or the in-process `auto_exit_due`) and never a delegated tower's. The export
fails closed for a token wallet whose carriers cannot be enumerated, because a carrier mis-exported
as plain would hand the tower a token-destroying backup.

**Advice worth putting in your holder docs:** hold, or (for a legacy piece) materialize — never
"refresh before your locktime expires" (there is no locktime, and `refresh` refuses a carrier
outright) and never a plain unilateral exit. Do **not** advise "combine": the legacy multi-carrier
combine is retired and the coloured lane has no combine transaction at all. The
`TOKEN_PIECE_SATS + fee_reserve + min_split_output` = **5 040 sats** "cost of escaping a carrier"
figure describes that retired lane's arithmetic and is kept here as history, not as guidance; on the
coloured lane the binding number is the coloured root floor
(`3 · 744 + 330` = 2 562 at the shipped 3 sat/vB), below which a carrier can never be laddered and
therefore has no exit material at all.

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
  `sdk32`, `sdk34`, `sdk36`, `sdk39`, `sdk52`). `sdk78` was DELETED with the retired lane, and
  `RGB_E2E=1, 2, 3, 5, 6, 8, 9, 10` with it — do not put those ids in a run list.
