# Tokens on RGB

> How partial token amounts work — colored splits, raw units vs precision, the piece size, exit
> behaviour — is covered end-to-end in the [granularity deep dive](granularity-deep-dive.md).

## Hello RGB

The token standard here is **RGB**, not a server-side token ledger. An RGB asset is a
client-validated contract: issuance and every transfer are cryptographic state transitions committed
inside Bitcoin transactions (tapret/opret) and validated by the *receiving wallet* from a
**consignment** — the transition history together with the witness transactions that seal it. Nobody
— not the SE, not an indexer — is trusted for token state.

On this layer allocations ride **statechain coins and off-chain sub-coins**, so token payments
inherit what sats have: instant off-chain transfers, exact amounts via coloured in-ladder splits,
census-verified receiving (the exit *branch* the receiver used to walk is retired with the flat
backup, 2026-09-06), and an SE-free settlement path
(walk the coloured ladder — see [Exits with tokens](#exits-with-tokens); it is not the plain one-tx
sweep sats get).

## The carrier, and why its ladder is coloured

A coin that holds an RGB allocation is a **carrier**.

`claim()` establishes a TES-R exit ladder — `T` trigger → `X_m` extension → `S_k` state, relative
CSV, all un-broadcast — for every un-laddered **root** coin it sees, at **first sight**: the
establish pass admits `IN_MEMPOOL`, `UNCONFIRMED` and `CONFIRMED` coins alike, because the trigger
needs only the funding outpoint, its value and the aggregate key, and the coordinator never gates a
co-sign on a confirmation. A carrier is not exempt from that
any more, but it may never be given a **plain** ladder: a plain tier is a *sats-only* spend of the
carrier's funding output `F`, so broadcasting one destroys the allocation
([`../spec/PROTOCOL.md`](../spec/PROTOCOL.md) §5.10 rule 1, terminal freeze;
[`../spec/SPEC.md`](../spec/SPEC.md) INV-29). What it gets instead is a **coloured** ladder — every
tier a coloured tier carrying its own RGB state transition (CTES-R) — so that the same walk which
exits a plain coin *moves* the allocation rather than sweeping it away.

The decision site in `claim()` is `match (self.inner.config.colored_ladder, one)`
(`UtexoWallet::claim`, `clients/libs/rust-sdk/src/wallet.rs`), where `one` is the single booked
allocation on the carrier's outpoint. With `colored_ladder` set *and* exactly one allocation resolved,
the carrier reaches `mercuryrustlib::tesr::build_colored_ladder_auto` + `cosign_colored_ladder` and
**is** laddered. Every other case records `LadderSkipReason::RgbCarrier` and leaves the coin flat.

**What decides `colored_ladder` is the enclave pin, not a preference.** It is no longer a bool the
constructors state; both presets compute it as
`TesrParams::attestation_identity_const(network).is_some()`
(`clients/libs/rust-sdk/src/config.rs`). The coupling is the point: colouring a carrier retires the
legacy coloured-split lane for it, and the CTES-R lane that replaces that lane cannot establish
anything without an identity to verify the enclave's attestation against — so `true` with no pin is
not a bolder default, it is a wallet whose token lane refuses forever while claiming to be retryable.
Reading the pin makes the two unable to disagree.

- **regtest is pinned** (`TesrParams::REGTEST_ATTESTATION_IDENTITY`, derived from the dev seed this
  repository commits, so the key is a fact about the source rather than about a running server), and
  therefore ships **on**.
- **mainnet and every public testnet evaluate false** — `attestation_identity_const` returns `None`
  for `bitcoin`/`mainnet` and for `testnet`, `testnet3`, `testnet4` and `signet` — for one reason:
  **no enclave is provisioned there yet**, so there is no identity to pin. Inventing one would be
  worse than leaving it absent — a wrong pin refuses every attestation, and the obvious "fix" is to
  trust the key the coordinator serves, which is exactly the hole the pin exists to close. Publishing
  a real enclave identity and pinning it turns this true with no other change.
  [`../spec/SPEC.md`](../spec/SPEC.md) §0.4 rows V-1 and V-6.

**And the pin does not only gate carriers.** The SDK's establish pass calls `get_statechain_info` for
every coin it is about to ladder — it needs the coordinator's aggregate to bind against — and that
call resolves the attestation identity pin → configured value → **refuse**. So on an unpinned
network with no `SdkConfig::attestation_identity` (or `UTEXO_ATTESTATION_IDENTITY`) set, a **plain**
deposit is left un-laddered too, under `LadderSkipReason::AttestationIdentityUnpinned`, and with no
flat backup underneath it that coin has no exit material either: cooperative withdrawal is its only
route out. A *carrier* on such a network is recorded under `RgbCarrier` rather than the attestation
reason, because the carrier arm runs before that call. (`mercuryrustlib`'s own `update_coins` /
`LadderAtSight::Plain` deposit path ladders through `tesr::establish_auto`, which calls no attested
endpoint and so needs no pin — it is the SDK path, the one a wallet user takes, that stops.)

So the two lanes below are not "shipped vs experimental" — they are "wherever an enclave is
provisioned" vs "not yet there":

| | Coloured ladder (an enclave identity is pinned) | No ladder (none is pinned) — a fault to repair, not a lane (2026-09-06) |
|---|---|---|
| Exit material | `T → X_m → S_0`, coloured; a received piece walks five tiers `T → X_m → SP → ext_child → state_child` | **none** — no flat backup exists for any coin (`create_tx1` is deleted; [SPEC §2.4](../spec/SPEC.md), INV-31), and no coloured ladder can be built without a pin (`LadderSkipReason::RgbCarrier` for the carrier; `AttestationIdentityUnpinned` for the wallet's plain coins). Legacy `branch-` rows exist only on coins that predate the rule |
| Calendar | **none** — no flat chain beside the tiers, nothing matures on its own (INV-27, unconditional); the tier walk is relative-CSV | **none** — there is no backup to mature; there is also no exit (TRUST-MODEL B12) |
| Payment shape | coloured in-ladder split (`colored_in_ladder_pay`) | *retired* — the flat colored split over `F` (`create_colored_split_tx`) is refused by `refuse_legacy_colored_split_lane` **before any SE co-sign**, on both settings of the flag, and `register_split_subcoins_n` refuses again behind it |
| Sends per carrier | **one per split, and the carrier is sized for one** — but not capped at one: the change leg of a coloured root split is a *one-rung coloured spine tip* (`change_leg_role(SplitLane::Colored)` = `SpineTip`), and a further payment out of it routes to `colored_spine_batch_pay`. `CTESR_CARRIER_SEND_DEPTH` = 1 is the sizing input behind `TOKEN_CARRIER_SATS`, not an enforced ceiling; what bounds the chain is the tip's remaining sats against the coloured floors. A received coloured CHILD is different — it forwards WHOLE (`transfer_colored_child`); a coloured child-level split does not exist | **0**; `LEGACY_CARRIER_SEND_DEPTH` = 5 survives only as the sizing constant behind `TOKEN_CARRIER_SATS` |
| Multi-payee batch | refused by name (`refuse_colored_multi_payee`) | *retired* with the lane — `batch_transfer_tokens` is refused at `refuse_legacy_colored_split_lane`, and again at `register_combine_subcoins` / `register_split_subcoins_n` behind it (`sdk09`: premise retired, re-derivation pending) |
| Re-anchor | `colored_reanchor` (coloured de-trigger) | none — `refresh` refuses a carrier |
| Unilateral exit | the coloured walk moves the allocation to the owner's own key (`sdk74`, `sdk75`) | none — and no pre-signed spend of `F` exists at all |

The coloured lane is built and exercised end to end (`sdk74` establish, `sdk75` exit, `sdk77`
coloured in-ladder split; `sdk87` / `sdk88`, which drove a carrier deadline and the exit-headroom
gate, are re-derived — no calendar, and the gate has no caller — pending run).

**The right-hand column is not a lane the product can complete (2026-09-06).** Where no identity is
pinned a carrier gets no ladder — recorded as `RgbCarrier`, the same reason a carrier below the
coloured floor gets on regtest — and there is nothing underneath it: no flat backup is ever
co-signed (`create_tx1` is deleted), the flat conveyance lane and its licence classifier
(`assert_flat_conveyance_is_legitimate`, `PermanentLicence`) are deleted,
`is_legitimate_flat_reason` answers `false` for every recorded reason and `permits_flat_conveyance`
is never true, and `transfer_sender::execute` refuses a coin with no ladder row by name. The legacy
flat colored split/combine over `F` is refused **outright** by `refuse_legacy_colored_split_lane`,
whichever way `colored_ladder` is set and before any SE co-sign, with
`register_split_subcoins_n` / `register_combine_subcoins` refusing again behind it. **The migration
hatch is closed with the lane**: a carrier CTES-R can never colour used to be allowed onto the legacy
split as an exception, and it no longer is — `migration_hatch_verdict` survives only to name that
class precisely in the refusal, because a child carved there would have been exited by the flat
backup chain and there is none. The five chained sends are what the carrier is still *sized* for
(below), not anything it can do. The reasons are still *recorded* (`flat_only_coins`,
`ladder_skip_reason`) so the state is visible; they open no gate. That is the sharp end of "waiting
on an enclave": on an unpinned network a carrier is holding only — no send, no SE-free settlement, no
exit (TRUST-MODEL B12) — until an identity is pinned and a later `claim()` pass colours it.

**One class has no repair at all: a PLAIN ladder over a carrier.** If tokens are moved onto an
outpoint that was already plain-laddered as a deposit — and under laddering-at-first-sight that is
every plain deposit — `claim()` records `LadderSkipReason::PlainLadderOverCarrier` and stops. The
plain tiers are co-signed and cannot be unsigned, so the coin's own exit material would burn the
allocation; `colored_reanchor` refuses a plain-laddered coin by name ("use `refresh`"), and
`refresh`'s plain re-anchor would destroy the very allocation it was called to save. There is **no
remedy in this SDK today**, and the variant's own documentation says so. The allocation on such a
coin is stranded; the sats are still exitable by walking the plain ladder, at the price of the
tokens. Avoid it by never moving an allocation onto an already-laddered outpoint.

## Where the sats come from — the carrier is sized, not rounded

Two constants in `clients/libs/rust-sdk/src/tokens.rs` fix a carrier's economics, and both are
derived from the protocol committed fee rate `TesrParams::committed_fee_rate` = **3.0 sat/vB**
(`TIER_COMMITTED_FEE_RATE`), not chosen:

* **`TOKEN_PIECE_SATS` = 4 074** — the sats a token piece carries. It is the coloured ROOT floor
  `colored_ladder_floor` computed at *double* the committed rate (`PIECE_FEE_RATE_HEADROOM` = 2.0):
  `3 · (⌈168 · 6⌉ + 240) + 330`. A piece is a coin like any other — its receiver claims it, and if
  the carrier is coloured that claim ladders it — so the piece must clear the floor it will be
  measured against, with head-room for a rate that moves after it is carved.
  `token_piece_sats_is_the_coloured_root_floor` recomputes the floors from the real `tesr` functions
  and fails if the constant ever drops below them.
* **`TOKEN_CARRIER_SATS` = 22 536** — what a freshly-issued carrier is funded with, and the *larger*
  of two sizings, kept although the legacy lane it was sized for is retired. Legacy flat lane: `5 · (4 074 + 300) + 666`
  (`legacy_carrier_sats`). Coloured lane: 8 253 (`ctesr_carrier_sats`). Over-sizing parks sats in a
  change output; under-sizing is refused at spend time with the carrier already terminalized, so the
  max is the fail-closed choice.

What bounds a coloured split's legs is a **per-leg max of two floors**, taken up front and refused
with the carrier untouched: `min_split_output(rate) = 330 + ⌈112 · rate⌉` — a dust-plus-fee floor
that survives the retirement of the flat lane it was named for, as a fact about whatever transaction
eventually spends the output — and the coloured floor for that leg's *shape*, `colored_child_floor`
for a payee's two-rung piece and `colored_spine_tip_floor` for the sender's one-rung change tip
(`change_leg_role(SplitLane::Colored)` = `SpineTip`). `split_fee_reserve(parent) =
clamp(parent/100, 300, 2000)` belongs to the retired flat lane only: an in-ladder tier commits its
fee inside itself, so this lane takes no reserve.

## Issuance, mint, burn

| Spark BTKN | Here |
|---|---|
| `createToken` | `issue_token(ticker, name, precision, supply)` — RGB NIA, full supply at issuance / `issue_inflatable_token(..., inflation_amounts)` — IFA |
| `mintTokens` | `mint_tokens(asset_id, inflation_amounts)` — IFA on-chain inflate, the newly-minted allocation bound to a fresh statechain coin (NIA supply stays fixed); [SPEC §7](../spec/SPEC.md) REQ-20, `sdk09` |
| `transferTokens` | `transfer_tokens(asset_id, receiver_address, amount)` — coloured in-ladder split + child handover; a carrier with no coloured ladder is refused (the legacy flat split is retired, 2026-09-06) |
| `batchTransferTokens` | `batch_transfer_tokens(asset_id, &[(address, amount)])` — *retired 2026-09-06*: the one-split-N-pieces lane is refused at `refuse_legacy_colored_split_lane` (and again at `register_split_subcoins_n` / `register_combine_subcoins` behind it), and a coloured carrier refuses multi-payee by name (`refuse_colored_multi_payee`); `sdk09`: premise retired, re-derivation pending |
| `freezeTokens` / `unfreezeTokens` | **N/A by design** — see below |
| `burnTokens` | `burn_tokens(asset_id, amount)` — burns engine-held free balance on-chain; statechain-bound supply must be exited first |
| token identifier (`btkn1…`) | RGB contract id (`rgb:…`) |

`issue_token` does three things:

1. issues the NIA contract in the wallet's RGB engine (`create_utxos` then `issue_nia`),
2. deposits the full supply onto a **fresh statechain coin** of `TOKEN_CARRIER_SATS` in one colored
   on-chain transaction (`bind_engine_supply`) — the only on-chain tx a token needs until settlement,
3. registers that coin as the asset carrier.

`issue_inflatable_token` creates one colorable UTXO per allocation (the fungible supply plus each
inflation-right) before issuing, so binding the supply never consumes the reserve
([SPEC §7](../spec/SPEC.md) REQ-19, INV-12). `mint_tokens` snapshots `list_allocations` *before*
inflating and binds only the difference, so a mint never re-binds already-bound supply (REQ-20).
Each of `issue_token`, `issue_inflatable_token` and `mint_tokens` has a `_sized` sibling that takes
the carrier's sats explicitly; the default is `TOKEN_CARRIER_SATS`.

From then on the supply moves off-chain.

## Transfer lifecycle

```
alice: 1000 TKN on coloured carrier C (22 536 sats), ladder T → X_0 → S_0
alice.transfer_tokens(TKN, bob, 250)
  → coloured IN-LADDER split (off-chain, un-broadcast): SP over X_0's payload output
      out: [piece child: 250 TKN + 4 074 sats]  → conveyed to bob
      out: [change: 750 TKN + rest]             → alice's own one-rung coloured SPINE TIP
      out: P2A anchor
  → the piece's own two coloured rungs (ext_child, state_child) are co-signed
  → the consignment for the 250-TKN assignment travels in the CHILD BUNDLE
    (protocol_version 4, `child_tesr_bundle`); its leaf consignment resolves against the
    child's own witness chain T → X_m → SP → ext_child → state_child
bob's watcher:
  → verifies the child bundle (census, §"What a receiver verifies" in trust-model.md)
  → books what the CONSIGNMENT assigns to his own final-state payload output,
    under the consignment's VERIFIED contract id
balances: alice 750 / bob 250 — zero on-chain footprint
```

*(The retired flat lane put the consignment on a backup row as `BackupTx.rgb_consignment` and shipped
an exit branch with it. No conveyance writes a backup row any more, which is why the SSP's pre-pay
gate had to be re-plumbed to read the bundle — see [lightning.md](lightning.md).)*

Two receiver rules do the work, and both are normative
([SPEC §7](../spec/SPEC.md) REQ-21/REQ-22): the amount comes from the consignment, with the
envelope's `a` field treated only as a cross-checked hint (a mismatch rejects, ERR-8); and the asset
is booked under the cryptographically verified `contract_id`, never a sender-claimed one. The single
predicate `verify_consignment_assignment` serves both the claim path and the SSP's pre-payment gate,
so the two can never drift apart.

**Multiple carriers.** If no single carrier holds the amount, `transfer_tokens` spans several, and
the two lanes span them differently.

On the legacy flat lane it COMBINEd — N carriers → exact piece + change in one SE-co-signed colored
combine tx (`colored_combine_transfer`, over `mercuryrustlib::rgb::create_colored_combine_tx`), with
`required_terminal_ancestors` counting one terminal ancestor per structural *input*. That lane is
RETIRED 2026-09-06, and the refusal comes *before* any SE co-sign: `refuse_legacy_colored_split_lane`
declines the route with the carriers untouched, and `register_combine_subcoins` refuses again behind
it, because a combine's outputs were exited by flat backups and no flat backup exists any more
(`sdk36` re-derived, pending run).

On the coloured lane that shape cannot exist — each carrier's `F` is already spent by its own trigger
`T`, and `SP` spends exactly one `X_m`, so there is no multi-parent coloured tier. `colored_multi_carrier_transfer`
pays it as a multi-PIECE payment instead: one in-ladder split per carrier, each conveying a coloured
child to the same recipient, who books them as separate allocations summing to the amount. Terminality
is then one terminal parent per leg rather than N per transaction, and the legs are sequential and
**not atomic** — a failure on leg `k > 0` leaves legs `0..k` already conveyed, so the recipient is
short-paid rather than unpaid, and the error names every piece already handed over. `sdk31` drives
this lane: two carriers, two coloured children, each child's `SP` spending its parent's `X_m` payload
output and never `F`, each source carrier terminal at the SE, and a read-only stock probe binding each
child's exact share to its own exit output.

**Many recipients.** `batch_transfer_tokens` is not a lane any more, in either direction. On a
coloured carrier it is refused by name (`refuse_colored_multi_payee`, raised inside the split engine
so no route can reach a K > 1 batch by another door) — that lane conveys serially after the carrier
is already terminal and journals no recipient address, so a failure part-way through would strand the
remaining pieces. On a carrier with no coloured ladder it is refused by
`refuse_legacy_colored_split_lane` before anything is co-signed. *(The retired flat lane carved one
piece per recipient plus change in a single colored split, each piece shipping its own consignment
envelope for its receiver to validate — `sdk09`, whose premise is retired and whose re-derivation is
pending.)* Pay two recipients from two carriers.

## Why there is no freeze

Spark tokens can be issuer-frozen because operators enforce token state. RGB is
**client-validated**: token state has no central enforcement point, so an issuer freeze list has no
consensus meaning — a "frozen" holder's transfers still validate for any receiver that does not
honour the list. This is treated as a property of the trust model (tokens behave like bearer
instruments) and is documented rather than faked. Issuers who need freeze semantics should not issue
on client-validated rails.

## Exits with tokens

A carrier refuses the plain exit operations, and each refusal has a reason:

* **`withdraw`** excludes carriers from the withdraw-everything default and hard-errors when a
  carrier is named — an RGB-unaware L1 sweep destroys the allocation
  ([SPEC §9.1](../spec/SPEC.md)). `sdk02` asserts exactly this: a cooperative sweep of a wallet
  holding a received token coin sweeps **nothing**, and the balance survives intact. (That test runs
  with `colored_ladder` set, but the refusal is lane-independent — `withdraw` never consults the flag.)
* **`unilateral_exit`** likewise excludes carriers, with one opening — and on a network whose
  enclave identity is pinned that opening is now the normal case: a carrier whose ladder **is
  coloured** may walk it, because every tier is then a valid RGB state transition and the walk moves
  the allocation to the owner's own key. Where no such ladder exists the call refuses, and it refuses
  *by name* rather than returning `complete` on a walk it did not perform. **Read its refusal with
  one correction:** for the un-colourable class it names two routes, `materialise_carrier` and
  `transfer_tokens`, and only the first still exists — the "migration hatch" the second half points
  at was closed with the legacy lane on 2026-09-06, so `transfer_tokens` now refuses that carrier
  too. The message has not caught up.
* **`refresh`** (the cooperative on-chain re-anchor) refuses carriers outright — it routes through
  `withdraw`, which is RGB-unaware, so a plain re-anchor would move the sats and destroy the
  allocation. The coloured counterpart is `colored_reanchor` (broadcast `T`, then co-sign and
  broadcast a coloured de-trigger — two transactions, zero CSV wait, no SE change), and it needs a
  **coloured ladder** to build from. Where there is none, the refusal names that too: move the asset
  off the coin first — which is advice only where the coin *can* still be coloured. On a
  plain-laddered carrier (`PlainLadderOverCarrier`) it is not advice at all: `colored_reanchor`
  refuses the plain ladder by name, `refresh` would destroy the allocation, and `transfer_tokens`
  has no coloured lane to move it with. That class has no remedy today.

For a carrier that predates 2026-09-06, token settlement instead means **materializing the coin's
branch on-chain**: broadcasting the stored `branch-<id>` rows, which for a carrier *are* the un-broadcast coloured split/combine transactions —
the RGB witnesses that carved the allocation. Landing them root-first settles the allocation on a
confirmed outpoint and spends the shared root. `sdk39` drove this at depth 2 on the retired lane
(re-derived, pending run): two successive
transfers build a piece whose branch is `[split1, split2]`, the recipient broadcasts both root-first,
and the 250 units settle on-chain with no SE involved. Onward movement of the *sats* still needs the
SE: materializing settles the asset, it does not exit the coin. `unilateral_exit` refuses rather than
returning an `ExitStatus` with `complete` set, which would be a false green on an escape hatch — and
that refusal is where its stale second route is printed (above).

`materialise_carrier(statechain_id)` is the named call for a carrier for which no coloured ladder can
ever be built — and it only ever finds `branch-` rows on a carrier that predates 2026-09-06; a
carrier minted since has none to settle. It is gated on `carrier_is_permanently_flat` — the same shared definition
`unilateral_exit` refuses against, so the two can never disagree about which coins they mean — and it
verifies against the chain before returning: an unreachable backend is an `Err`, never a quiet
success.

**Stated plainly, because it is the sharpest edge in this document.** For an un-colourable legacy
carrier the *complete* list of what still works is: `materialise_carrier`, which settles the
ALLOCATION on a confirmed outpoint. There is no unilateral exit (every pre-signed spend of `F` this
wallet holds is RGB-unaware, and it is refused rather than broadcast), no cooperative withdrawal
(`withdraw` hard-errors on a carrier), and since 2026-09-06 no onward payment either (the legacy
split/combine lane it used to escape through is retired outright). The sats stay on the 2-of-2 and
need a co-operating SE to move.

## Tokens over time — holding, and doing nothing

*"If I issue or receive tokens and then do nothing for a year, do I lose them?"* No. Tokens are never
lost by inactivity. What differs is what still works, and that depends on how you got them.

**Tokens you issued or minted — a root carrier, funded on-chain.** Terminal freeze keeps it off the
*plain* T/X/S tiers for good; whether it gets *coloured* ones depends on the enclave pin above. Either
way it has no ancestor, so there is no clawback risk. It keeps **no** signed-once deposit backup — no
coin does since 2026-09-06 (`create_tx1` is deleted), so nothing on it matures at
`deposit_height + initlock`; `initlock` / `interval` survive in `TesrParams::flat_ladder_params` only
as compatibility constants. Without a coloured ladder the carrier has no exit material and no send:
the flat colored split that used to distribute from it (each sub-coin with its own fresh backup) is
refused at `refuse_legacy_colored_split_lane` before any co-sign, so its tokens stay put until the
carrier can be laddered — which is the enclave pin again, not a decision. A coloured one pays by
coloured in-ladder split and can walk its own ladder.

**Tokens you received — a coloured in-ladder child**, funded by an output of the un-broadcast `SP`.
That funding is why it can never root a **trigger** of its own — nothing can broadcast a funding
output that was never meant to be broadcast — so its exit material reaches back through its parent:
the five-tier walk `T → X_m → SP → ext_child → state_child` in its `ctesr-` bundle, SE-free, moving
the allocation to your own key. It inherits **no calendar** from the parent, because the parent has
none (`CHILD_V2_BASELINE = 0`).

The one risk is an event, not a date, and it is defended automatically. No sender or ancestor holds a
matured spend of `F` any more — the "past the root deadline the sender's retained deposit backup
matures and burns the allocation" hazard is REMOVED with the flat backup (2026-09-06). What a prior
owner still holds is the un-timelocked trigger `T` (a griefing tool: broadcasting it starts your
walk, and you win the CSV race) and superseded states that lose that race. `defend_ladders()` watches
`F` for every live coin from the block its deposit is first seen in and drives the child's chain when
`T` lands. *(A piece received before the rule on the legacy flat lane is a `branch-` sub-coin whose
allocation still settles through `materialise_carrier`; no such piece is minted now.)*

`auto_exit_due(margin_blocks)` — the height-keyed pass — has a **legacy subject only** since
2026-09-06: it still runs, gated on a *verified* branch read (an unreadable read is blindness, never
"nothing due"), and what remains of it iterates `branch-` rows from before the rule:

* a carrier whose branch reads **verified-empty** has no ancestor and nothing to race it, and is
  skipped, and only then;
* a legacy carrier that **has** a branch and is inside its margin emits
  `WalletEvent::TokenCarrierMaterialized` and is materialized with `broadcast_branch_if_any` —
  **branch only** ([SPEC §9.5](../spec/SPEC.md) REQ-33 is RETIRED; this arm reads rows no lane can
  mint);
* the third loop — the leaf near-deadline loop that drove `unilateral_exit` for a received split
  child or a spine tip against an inherited deadline, emitting `LeafExitForced` — is **deleted**. A
  child has no height to be near: its defence is `defend_ladders()`, event-driven on the parent's
  `F`, and `LeafExitForced` is no longer emitted (the variant survives so subscribers compile).

`sdk34`, which drove the coloured variant against a deadline and asserted that the sender's matured
backup failed to broadcast, is re-derived to the event-driven defence — pending run.

The pass runs every poll of the background watcher (`start_background`, gated on
`SdkConfig::auto_exit`, which ships **true** in both constructors), and it fails **closed and loud**:
any failure to read the chain tip, the wallet record, the carrier enumeration, or the split journal
emits `WalletEvent::WatchtowerBlind`, retains a `WatchtowerFault` readable through
`watchtower_faults`, and returns `Err`. It never proceeds on a defaulted-empty carrier set — that
would skip every carrier's protection while reporting success.

The margin itself is derived, not chosen — `auto_exit_margin_blocks_for(k_max, interval, child_depth)`
= `k_max·interval + tesr_exit_txs(d)·144`, i.e. **860** blocks on regtest and **2 120** on mainnet —
and it now sizes a pass with no laddered subject: its `k_max·interval` term was the ancestor-locktime
gap of the retired flat chain. The second term is real for any walk: the exit lands its transactions
one after another and each must confirm before the next tier's relative lock starts counting.

**Delegation.** The duty is also delegable, keyless, to any external tower
([SPEC §9.5](../spec/SPEC.md) REQ-34). `sdk45` pins the property on the ladder bundle itself: it
carries **zero** key material (a serialized bundle is scanned for every secret-key spelling), a
second independent tower over the same bundle is harmlessly idempotent, and a keyless tower drives an
offline owner's whole exit off nothing but `tesr::watch_pass`.

`export_watch_bundle` is the SDK's export of that duty across a whole wallet, and it is where the
carrier rule lives: every `WatchEntry` it emits — carrier or not — carries **no** `backup_tx` (none
exists since 2026-09-06) and is event-driven, so a delegated tower can only ever drive the pre-signed
walk, never sweep and destroy the allocation. That omission is structural rather than a policy the
tower is trusted to follow: the export has exactly two entry constructors — the laddered arm, which
hard-codes `backup_tx: None`, `branch_txs: []` and a trigger on `F`, and `leaf_watch_entry` for an
adopted child or spine tip, which does the same with the leaf's own chain — and no third arm emits
anything. The height-driven arm that used to broadcast a branch and then a backup is **deleted**: it
was unreachable (no coin has an `exit_deadline_block`) and it opened by demanding a backup row, which
failed the whole export. **A legacy `branch-`-only coin is therefore omitted from the bundle
entirely**, not exported with its branch — it is reported by `flat_only_coins` instead, and
materialising it stays the owner's own `auto_exit_due` / `materialise_carrier` business. (The unit
test `bundle_roundtrip_and_carrier_has_no_backup` is a serde round-trip over a hand-built bundle: it
pins that a *carrier* entry survives with no `backup_tx` and that the types carry no key material at
all. What holds the whole-wallet claim is the exporter's two constructors, not that test.) The export
also fails **closed**: a token wallet whose carriers cannot be enumerated exports nothing rather than
mis-exporting a carrier as plain, and adopted split children and spine tips are read from their own
rows so a leaf is never silently absent from the bundle.

**One limit worth naming, restated for the lane that ships.** A lone received piece carries
`TOKEN_PIECE_SATS` = 4 074 sats. On the coloured lane it is a coloured CHILD, and what it can do is
be forwarded **whole** — `transfer_tokens` takes the child arm and calls `transfer_colored_child` —
or walk its own five-tier ladder out. What it cannot do is be *split*: a coloured child-level split
does not exist, so a child's allocation cannot fund part of a payment, and the multi-carrier planner
says so by name when it comes up short. *(On the retired flat lane the same limit had a different
cause — `transfer_tokens` refused "carrier coin too small" whenever
`TOKEN_PIECE_SATS + split_fee_reserve >= carrier_sats`, and the escape was to combine two pieces.
That combine is retired with the lane, so it is no longer a remedy for anything.)*

**Summary.** Never lost. Cooperative operations work throughout for plain coins, and for a carrier
they work through the *coloured* calls only — `withdraw` and `refresh` refuse a carrier by name at
any time. A received token's unilateral
walk is available indefinitely — nothing on it matures, and if someone spends the shared root the
watchtower drives the walk for you, so even an offline receiver is protected. The asymmetry with sats is narrower than it
used to be, and worth stating exactly. Every coin, plain or coloured, has its CSV clock stopped by an
un-broadcast trigger and no other clock — there is no retained flat chain and no `min(L_k)`
(2026-09-06) — so in time the two are alike. What still differs is the escape hatch: a plain coin's
walk is always available, whereas a carrier's is available only if its ladder is **coloured** — and
where no enclave identity is pinned it is not, so such a carrier has no SE-free story at all
(TRUST-MODEL B12; only a legacy `branch-` carrier from before the rule can still materialise). That is
the residual price of anchoring RGB in signed-once transactions, and it shrinks to nothing on a
network with a provisioned enclave.

---

**Test status.** The unit and CI-guard suites are green and the E2E crate compiles, but **no E2E flow
has been re-run against the regtest stack since the flat backup was removed on 2026-09-06**. Every
`sdk*` / `rgb*` flow named above — including the coloured-lane ones (`sdk74`, `sdk75`, `sdk77`) — is
evidence *pending a run*; the results attributed to them are the ones recorded before that change.

---

Normative sources: [`../spec/SPEC.md`](../spec/SPEC.md) §7 (tokens), §6.2 (branch split & combine),
§9.5 (watchtower); [`../spec/PROTOCOL.md`](../spec/PROTOCOL.md) §5.10 (RGB integration and terminal
freeze). Tokens over Lightning: [lightning.md](lightning.md).
