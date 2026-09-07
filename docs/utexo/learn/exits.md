# Deposits and exits

> This page is the guided tour. The normative accounts are [PROTOCOL.md](../spec/PROTOCOL.md) §5.2
> (the tiers), §5.7 (races), §5.8 (the cooperative de-trigger), §5.9 (exit costs) and §5.13
> (watchtowers), and [SPEC.md](../spec/SPEC.md) §4 (deposit) and §9 (exit). The trust boundaries a
> tower does and does not remove are in [TRUST-MODEL.md](../spec/TRUST-MODEL.md) §5.

**Read this first.** A laddered coin has **one clock, and it is stopped.**

- The **CSV clock is stopped**: the deposit watcher pre-signs a trigger → extension → state chain of
  relative timelocks at the *first mempool sighting* of the funding transaction and leaves all three
  **un-broadcast**. BIP-68 locks do not tick until their parent confirms, and the trigger has no
  timelock at all, so **nothing matures until someone broadcasts the trigger**. An idle coin never
  ages: no expiry, 0 vB of idle rent.
- **There is no calendar clock.** No coin carries an absolute-locktime backup transaction — none at
  deposit, none at any hop (**INV-31**; the flat backup chain and its `create_tx1` are gone, and
  INV-5 is RETIRED 2026-09-06). `coin.locktime` is `None` for life, no previous owner holds a
  matured spend of `F`, and INV-27 ("idle coins never age") is unconditional. What bounds a coin's *off-chain* life is
  its renewal/rollover capacity; the only on-chain cadence is the cooperative re-anchor at that cap.

Everything below is written against that one clock. The tour ends with a cost table.

> **Test evidence on this page.** The rule that removed the flat backup chain landed together with
> re-derivations of every flow whose subject it was, and those re-derivations have **not been run
> against the regtest stack yet**. Where this page cites `sdk12`, `sdk17`, `sdk30`, `sdk32`, `sdk34`,
> `sdk36`, `sdk39`, `sdk40`, `sdk41`, `sdk42`, `sdk43`, `sdk44`, `sdk45`, `sdk46`, `sdk47`, `sdk48`,
> `sdk50`, `sdk58`, `sdk59`, `sdk60`, `sdk71`, `sdk74`, `sdk76`, `sdk79`, `sdk80`, `sdk84`, `sdk86`,
> `sdk87` or `sdk88`, read "re-derived, pending run". `sdk04`, `sdk16`, `sdk38`, `sdk51`, `sdk52`,
> `sdk75`, `sdk81`, `sdk83`, `sdk89`, `sdk90`, `sdk91` and `sdk94` were **not** touched. `sdk73`,
> `sdk78` and `RGB_E2E` 1–3, 5, 6 and 8–10 no longer exist. `git diff` over
> `clients/tests/rust/src/` is the authority.

---

## Deposits

`get_deposit_address(amount)` performs the SE handshake (`deposit/init/pod`) and returns a taproot
address whose key is the aggregate of yours and the SE's. Send the exact amount; the SDK's watcher
picks it up.

The order the watcher works in is load-bearing:

1. **First sighting** (the coin flips `INITIALISED` → `IN_MEMPOOL`): the ladder is established —
   `T`, `X_0`, `S_0` built and blind-co-signed over the still-unconfirmed `F`. Under
   `coin_status::check_deposit`'s own lane (`LadderAtSight::Plain`, the `update_coins` default) the
   watcher does it itself; under the SDK's `claim()` the watcher runs as `LadderAtSight::Defer` and
   `claim()`'s establish pass ladders every un-laddered `IN_MEMPOOL` / `UNCONFIRMED` / `CONFIRMED`
   coin in the **same pass** — plain, or **coloured** for a carrier whose allocation is booked (an
   issuance books it at broadcast). The enclave count after a deposit is exactly **3**. Nothing
   exit-related waits for a confirmation.
2. **What happens when the ladder cannot be established differs by lane, and the difference
   matters.** Under `LadderAtSight::Plain` the deposit is **not booked**: `check_deposit` rolls the
   coin back to `INITIALISED`, returns the failure by name, and the next pass retries. Under
   `LadderAtSight::Defer` — the lane every SDK wallet takes — `update_coins_ex` books the coin
   `IN_MEMPOOL` and persists it *before* `claim()`'s establish pass runs, so a failure there leaves
   a **booked coin with no exit material**: the pass records a `ladderskip-<sid>` row, emits
   `LadderSkipped{reason}` and moves on. Such a coin cannot be conveyed and cannot be unilaterally
   exited; only cooperative withdrawal gets the value out. There is no "signed exit" underneath a
   coin that has no ladder, on either lane. (One deliberate exception on the `Plain` lane: a
   `single_use` coin is booked with **no** ladder, because the SE refuses any second co-sign on such
   a coin and a three-tier ladder cannot exist over it. It never had exit material of its own.)
3. **Later, in a separate pass**: confirmations are counted, the coin flips `UNCONFIRMED` →
   `CONFIRMED`, and `DepositConfirmed` is emitted. Confirmation changes nothing about the exit
   material: same trigger on disk, same three co-signs. `LadderEstablished` was already emitted at
   sight. A coin that already carries a ladder is skipped, so repeated passes never double-sign,
   and the exit payee is always the coin's own seed-derived `backup_address`.

Off one funding output `F` the ladder pre-signs, but never broadcasts:

```
F   (on-chain, your deposit — the ONLY thing resting on chain)
└─ T   TRIGGER    v3/TRUC + 240-sat P2A. NO timelock. Signed ONCE, at first sight of F. Never re-signed.
   └─ X_m EXTENSION  relative CSV E_m = E0 − m·δE, counted from T's confirmation.
      │              Renewal replaces it HORIZONTALLY with a lower-CSV X_{m+1}, off-chain.
      └─ S_k STATE   relative CSV Δ_k = D0 − k·δ, counted from X_m's confirmation. Pays owner k.
```

Mainnet schedule, from `TesrParams::mainnet()` (`lib/src/tesr.rs`) verbatim: `D0/δ/D_floor =
1440/36/144`, `E0/δE/E_floor = 720/36/144`, forced rollover at `m_max = 15`,
`committed_fee_rate = 3.0` sat/vB. Testnet and signet run the **same** schedule; only regtest keeps
the small numbers so a full lifecycle mines in seconds. `sdk44` pins the arithmetic (re-derived to
start from the deposit's own first-sight ladder, pending run); `sdk40` PART 1 pins that real
consensus enforces BIP-68 here — an extension is rejected before `E` confirmations of the trigger, a
state before `Δ` of the extension — and that nothing ages while un-broadcast.

**The SE does not publish this schedule.** `/info/config` (`server/src/endpoints/utils.rs`) serves
only `initlock`, `interval`, `batchtimeout` and `version`. The tier schedule is compiled in per
network and a conveyed one is measured against the receiver's own preset field by field
(`cap_schedule`), which is strictly stronger than publication because it holds against a lying
coordinator. `initlock` and `interval` survive there as **compatibility constants**
(`TesrParams::flat_ladder_params`): `initlock` is now the FIXED exit window the split-depth cap
measures a leaf's exit walk against, and `interval` is applied to nothing.

**The enclave attestation pin gates the SDK lane for *every* deposit, plain or coloured.** This is
the sharpest thing on the page and it is easy to state backwards. `TesrParams::attestation_identity`
resolves compiled-in pin → configured value → **refuse**, and
`TesrParams::attestation_identity_const` returns `Some` only for **regtest**: mainnet, testnet,
testnet3, testnet4 and signet all return `None`. The two lanes then diverge:

- `coin_status::check_deposit`'s own lane (`LadderAtSight::Plain`) reaches `tesr::establish_auto` →
  `cosign_tier` **without** calling `get_statechain_info`, so it ladders a plain deposit on an
  unpinned network;
- the SDK `claim()` establish pass **does** call `get_statechain_info` — it needs the coordinator's
  aggregate to bind the ladder against — and on a network with no pin and no configured identity
  that call fails. The pass records `LadderSkipReason::AttestationIdentityUnpinned` and ladders
  **nothing**.

So on an unpinned network an SDK wallet's deposit is booked and has **no exit material at all**: it
cannot be conveyed *and* it cannot be unilaterally exited. Cooperative withdrawal is the only route
out. The flat backup used to supply that unilateral exit without any attestation; it no longer
exists. Note this is a **not-yet-deployable** state rather than a live regression — mainnet has no
enclave provisioned at all — but "deposits and exits work without a pin, only receiving does not" is
false for the SDK path, which is the only path a wallet user takes.

**Two things this pass will not do, both by design and both fail-closed:**

- **give an RGB carrier a *plain* ladder.** A plain tier spend is sats-only, so broadcasting one
  sweeps the sats and destroys the allocation (terminal freeze,
  [PROTOCOL.md](../spec/PROTOCOL.md) §5.10). The carrier is not excluded from laddering for that
  reason — it gets a **coloured** ladder instead, every tier carrying its own valid RGB state
  transition. `SdkConfig::colored_ladder` (`clients/libs/rust-sdk/src/config.rs`) selects it by
  **reading the enclave pin**, `TesrParams::attestation_identity_const(network).is_some()`, because
  a coloured ladder whose terminality cannot be verified against a pinned identity is not worth
  building. Regtest is pinned and ships on; mainnet, testnet and signet evaluate false only because
  **no enclave is provisioned there yet**, and pinning one flips it with no other change
  ([SPEC.md](../spec/SPEC.md) §0.4 rows V-1 and V-6). A carrier that cannot be coloured — below the
  coloured floor, RGB state unavailable, no pin — has **no exit material** until a later pass
  colours it; `LadderSkipReason::RgbCarrier` says so, and the coin cannot be conveyed. A plain ladder
  found over a carrier (tokens moved onto an already-laddered outpoint) is recorded
  `PlainLadderOverCarrier`, and it has **no remedy**: `colored_reanchor` refuses a plain ladder by
  name ("use `refresh`") and a plain `refresh` would destroy the allocation, so the coin exits as
  satoshis and the tokens are what is lost;
- **root a trigger on an un-broadcast funding output.** A coin funded by a split output — an
  in-ladder child, a spine tip — has no confirmed prevout for a trigger to spend, and a v3 tier
  cannot relay over an unconfirmed parent. Colouring a tier does not change this and never could:
  it is the same fact that makes idle rent 0 vB. Such a coin is laddered by the split that created
  it, whose chain reaches back to the parent's on-chain `F`, not by this pass.

If the carrier set or `F`'s status cannot be resolved, the pass is skipped and retried: a missed
ladder is recoverable, a plainly-laddered carrier is not. A skipped coin has no exit material until
the retry succeeds — there is no flat backup to fall back on. On the `Plain` lane that is why the
deposit is left `INITIALISED`; on the SDK's `Defer` lane the coin is already booked, so the skip is
recorded and surfaced instead (`ladder_skip_reason` / `flat_only_coins`) and the coin sits there,
withdrawable but not conveyable and not exitable, until a retry succeeds. `sdk71` drives the
unconditional half on the live stack (re-derived: every conveyance licence now refuses, pending
run).

Deposit slots consume a **deposit token** (anti-spam / fee mechanism); when payment is required the
SDK surfaces `SdkError::TokenPaymentRequired` with the details. Slots minted by an SE-co-signed flow
over an existing statechain — a split piece or change, a `transfer_many` recipient, a refresh
re-anchor — are **derived slots** and cost nothing (`deposit/get_derived_token`, capped at 64 per
parent). `sdk36` covers this.

*Static addresses:* deposit addresses are per-coin. Reuse is detected and handled, but
`get_deposit_address` is cheap — call it per receive.

---

## Cooperative exit — the normal path, one transaction

`withdraw(to_address, statechain_ids?, fee_rate?)`: the SE co-signs a **fresh direct spend** of each
coin's funding output to your L1 address. No timelock, one on-chain transaction per coin, ≈ 111 vB.
The pre-signed ladder is simply abandoned un-broadcast — a cooperative exit never touches it.

Three cases behave differently:

- a **token carrier** is excluded from the withdraw-everything default and **hard-errors** if named,
  because an RGB-unaware sweep destroys the allocation;
- a received **split child** has no confirmed outpoint to spend at all — its funding `SP.out[j]` is
  un-broadcast — so `withdraw` cannot co-sign a direct spend. It is routed automatically to the
  unilateral exit below and booked `WITHDRAWING`. Nothing is lost: the child's pre-signed chain
  already pays your own key. It just settles over several blocks instead of one;
- a legacy sub-coin that still carries a `branch-` row from the **retired** coloured split/combine
  lane is materialized first (its branch transactions carry no locktime, so this is instant) to give
  the withdraw spend an on-chain input. Nothing mints such rows any more —
  `register_split_subcoins_n` and `register_combine_subcoins` refuse by name — so this arm only ever
  sees coins that predate the rule.

There is one further cooperative move worth knowing. Once a split's `SP` **confirms**, every
`SP.out[j]` is an ordinary on-chain P2TR paying that leaf's aggregate key — so the owner can spend it
with a **fresh** co-signature carrying no timelock, and N such outputs go into one transaction.
That is `mercuryrustlib::combine::combine_leaves`, driven end to end by `sdk83`.

---

## Unilateral exit — the SE is gone

`unilateral_exit(statechain_ids?, to?) → Vec<ExitStatus>` needs nobody. It is **incremental**, not a
single broadcast: each call advances the pre-signed chain as far as maturity allows and reports
`ExitStatus { statechain_id, complete, wait_blocks }`. Call it once per block, or let the background
pass do it, until `complete`.

The destination argument is **inert, and has to be**: every transaction the walk broadcasts was
signed at first sight of the deposit or at the split, and each pays the coin's own seed-derived
`backup_address`. There is nothing left to choose at exit time, which is exactly the property that
makes the walk keyless and delegable. To land the value somewhere else, exit first and spend the
result.

It dispatches on where the coin sits in a ladder, and the arms are probed **in this order** — the
spine tip is a position of its own, and omitting it is not a simplification:

1. **Root** (`exit_pass`) — walk the tier chain: broadcast `T`, then each extension and state as
   its relative CSV matures. **No absolute-locktime transaction exists to broadcast on this arm, or
   on any other.** A carrier whose ladder is **coloured** walks here too, and because every tier is a
   valid RGB transition the walk moves the allocation to the owner's own key rather than sweeping it
   away (`sdk75`).
2. **Split child** (`exit_child_pass`) — the same walk over the full pre-co-signed chain
   `T → X_m → SP → ext_child → state_child`, whose final state already pays this wallet's own key.
   This is also where a cooperative `withdraw` of a child is routed.
3. **Spine tip** (`exit_spine_tip_pass`) — the sender's own change leg from an in-ladder split,
   walking the one-rung cap over `SP.out[K]` via `next_spine_tip_exit_tier`.
4. **There is no fourth arm.** A coin carrying no ladder of the three kinds is **refused by name**:
   "coin *id* has no `tesr-<id>` ladder row and therefore no exit material: a coin's only exit is
   its TES-R ladder, and there is no flat backup to fall back to". The flat exit fallback — branch
   rows, then a latest absolute-locktime backup — is deleted, and so is the shape that fed it. The
   refusal names the two repairs it knows: restore the recovery bundle's `tesr-*` rows if the wallet
   was rebuilt from a mnemonic alone, or run `claim()` so the establish pass ladders a fresh
   deposit. Such a coin is a coin to repair (`ladder_skip_reason` says why a pass declined), not a
   coin with a slower exit.

Two refusals are part of the contract. `unilateral_exit`'s liveness rule is `is_live_for_defence`:
a coin is walkable while it is `IN_MEMPOOL`, `UNCONFIRMED` or `CONFIRMED` — a ladder exists, and is
exitable, from the block its deposit is first seen in — and every other status is refused even when
named explicitly (exiting a parent already consumed by a split would kill the transaction funding
the receiver's child). And it refuses a token carrier **unless its ladder is coloured**, because
otherwise every pre-signed spend of `F` this wallet holds is RGB-unaware; it says "move the asset
off this coin first" rather than returning `complete` on a walk it did not perform. A tier whose
timelock is unreached is reported as `ExitStatus{complete: false, wait_blocks > 0}`, never as an
error.

⚠️ **The second of those refusals still names routes that are closed.** For a carrier the wallet
judges *permanently* un-colourable (`carrier_is_permanently_flat`), the refusal text offers
`materialise_carrier` and `transfer_tokens` as the two remaining routes. Neither works on a coin
minted under the rule: `materialise_carrier` can only find `branch-` rows that predate it, and the
legacy coloured-split lane `transfer_tokens` would fall back to now refuses **unconditionally**
(`refuse_legacy_colored_split_lane` returns `Err` on both sides of the `colored_ladder` flag). What
such a carrier actually has is cooperative withdrawal, and a coloured re-anchor if it becomes
colourable. Treat that error message as stale text, not as advice.

*Evidence (all three re-derived under the rule, pending run):* `sdk50` (the SDK surface, end to end
— no absolute-locktime backup is broadcast, and none exists), `sdk40` PART 1 (real consensus rejects
each tier before its CSV is met and accepts it after), `sdk45` (a keyless tower drives the same walk
with no key material).

### What the walk costs

The signed tier is **125 vB** (`TIER_VBYTES`), measured through the production finaliser — TES-R
hashes with `TapSighashType::All`, so the witness carries the explicit sighash byte and a 124-vB
assumption would silently under-commit the fee the tier relies on to relay.

`config::tesr_exit_vbytes` gives the whole walk: `293·d + 375` vB — `T`, `X_m` and the final state at
125 each, plus per split level an `SP` (the only rung with two payload outputs, 125 + 43) and one
extension. `config::tesr_exit_txs_for` gives the transaction count by shape: `3 + 2d` sequential
transactions on the ordinary child lane (`ExitShape::TwoTier`), `4 + d` on a spine. `tesr_exit_txs`
is the `TwoTier` arm unconditionally, and every safety **margin** takes that one deliberately —
`3 + 2d ≥ 4 + d` for all `d ≥ 1`, so over-counting makes a tower act early, while a shape-aware
margin that guessed `Spine` for a coin that is actually two-tier would act late.

`config::tesr_exit_wait_blocks` gives the latency, and it is derived from the schedule rather than
written down: `720·d + 2160` blocks of relative locks on mainnet, plus `tesr_exit_txs(d)` — one
block per transaction in the walk, its parent's confirmation, which a tier's relative lock cannot
even begin counting before. That `+1` per tier is a floor and not a budget: it is the fastest the
walk can possibly go, so a margin built on it is already optimistic.

The tail — the payee's own extension-and-state pair — is the full `E0 + D0 = 2,160` blocks ≈ 15
days, shrinking by 36 blocks per hop and per renewal as the coin is used. A split level costs
only its extension, because `SP` is a spine tier at `SPINE_CSV = 0` and waits only for its parent to
confirm.

How deep a chain can get is **derived, not a literal** (`max_split_depth`,
`lib/src/transfer/receiver.rs`), and it is measured against a **fixed window**, not a calendar: a
laddered coin has no epoch deadline, so `enforce_split_depth_cap` (`clients/libs/rust/src/tesr.rs`)
bounds the *length and latency* of the exit walk a leaf would inherit — `exit_wait_blocks +
exit_slack_margin` — against `initlock` as a constant (10,000 on mainnet). On mainnet that tops out at
depth **8**, 19 transactions to walk; on regtest depth 54, 111 transactions. (The receive-side
headroom gate that used to measure the same walk against a parent's flat-backup deadline,
`check_exit_headroom_with_margin`, has no caller: there is no deadline to measure against.)

`estimate_exit_cost(statechain_id)` reports `{branch_txs, branch_vbytes, backup_vbytes,
total_vbytes, wait_blocks, exit_deadline_block, exit_deadline_blind}`. For a laddered **root**
`backup_vbytes` is the signed vsize of the tier walk (`TesrBundle::exit_tiers`); for a `ctesr-`
child or a `spinetip-` tip it reports **0**, because the function looks up a `tesr-` row and there
is none — read the child's cost from `config::tesr_exit_vbytes(d)` instead, not from this call.
`wait_blocks` is **0** on every laddered coin — an idle ladder has nothing maturing; the walk's
latency once started is `config::tesr_exit_wait_blocks` above — `branch_txs`/`branch_vbytes` are 0,
and `exit_deadline_block` and `exit_deadline_blind` are both `None`. That `None`/`None` pair means
"laddered, event-driven": the coin has no calendar at all, and the race, if one ever starts, starts
when somebody spends `F`. A `Some` deadline, or a non-zero `branch_txs`, appears only for a legacy
`branch-` coin, and only for that shape does `exit_deadline_blind == Some(reason)` mean "a deadline
exists and could not be computed" (`ExitCostEstimate::deadline_is_unknown()`).

---

## Broadcasting the trigger — what it actually does

`T` is the alarm, and pulling it is irreversible in three ways at once.

- **It starts every clock below it.** Until `T` confirms, no extension and no state anywhere in the
  tree can mature. After it confirms, the CSV walk runs on whoever's schedule.
- **It pre-empts every other spend of `F`, permanently.** With no flat backup anywhere, the only
  spends of `F` in a past owner's hands are their retained copies of the same `T` — no timelock, so
  the current owner or their watcher can always broadcast it first — and the superseded states
  below it, which lose the CSV race. Once `T` confirms, every historical key share for this coin
  authorises a spend of an output that no longer exists. The SDK exposes this use under its own
  name, `sever_from_f`, which is mechanically `unilateral_exit` on one coin.
- **It is loud.** No theft transaction can become *valid* until someone publicly spends your funding
  output on chain and then waits at least 144 blocks of CSV. Compare an absolute-locktime chain,
  where a rival's maturity arrives silently on the calendar and the contest afterwards is a
  minutes-scale mempool race — which is why no such chain exists on any coin here.

Missed liveness is never confiscation by design: nothing expires, and no output ever pays the
operator by timeout. Loss requires a real adversary winning a telegraphed public race while every
tower slept through a day or more of alarm.

### Answering someone else's trigger

Two responses exist, in preference order.

**1. Cooperative de-trigger** (needs the SE). `T.out[0]` pays the coin's own aggregate, so you and
the SE key-path-spend it with **no relative timelock** — valid immediately, confirming unopposed
inside the ≥ 144-block window during which no adversary transaction is valid. `build_detrigger`
(`lib/src/tesr.rs`) emits a *tier*, anchor and all, so it is **125 vB**, not the 111 vB of a bare
1-in-1-out spend. `cosign_detrigger` is wired through `UtexoWallet::detrigger_to_owner`.

**Say what it is: an exit, not a restoration.** The de-trigger pays a plain address you name. There
is no fresh funding output `F′` and no rebuilt `T′/X′_0/S′_0` on this lane. What ships is the half
that matters when you are being griefed — *you choose when the coin lands, in two transactions with
zero CSV wait, and every retained tier dies with it*. Getting back off-chain afterwards is a fresh
deposit. `sdk89` drives it end to end against bitcoind: a griefer confirms `T`, the owner answers,
the value lands at the owner's address, and the pre-signed extension is then submitted and refused
with `bad-txns-inputs-missingorspent`. `sdk40` PART 2 proves the consensus half independently.

The coloured variant is a coloured self-transition at **168 vB** (`COLORED_TIER_VBYTES`, an `opret`),
wired as `cosign_colored_detrigger` and reachable only from `colored_reanchor`, a manual call. It is
**not test-covered**, and neither is the mass-grief prioritization policy.

Griefing is survivable and bounded, but it is **not economically losing for the attacker**: both
transactions pay out of the coin's own committed fees, so at or below the committed rate the griefer
broadcasts something he already holds and pays nothing, while the coin loses two committed fees plus
two anchors. The damage is fee-sized sats out of the coin, never the coin.

**2. Race and win** (needs nobody). `defend_ladders()` runs one watch pass per adopted `tesr-`
bundle, one child pass per adopted `ctesr-` split child and one per `spinetip-` tip, over every
**live** coin — `is_live_for_defence`: `IN_MEMPOOL`, `UNCONFIRMED` or `CONFIRMED` — so a ladder is
defended from the block its deposit is first seen in. On an untriggered coin it is a no-op — there is
nothing to defend. On a triggered one it broadcasts your tiers as they mature, and because every
transfer co-signs a state strictly below the lowest rival, the current owner's state matures first.
`sdk51` drives exactly this: a prior owner triggers with a stale state, the owner runs only
`defend_ladders()`, and the funds land at the owner's key.

---

## Watching: what runs, and what it cannot do

Each tick, `start_background()` runs `claim()` — which is what ladders a newly seen deposit — then
iterates `maintenance_plan`, then runs two further passes. On a laddered wallet only one of the three
has a subject:

| pass | gate | margin | what it defends |
|---|---|---|---|
| `deadline_safety_due` | **unconditional** — `maintenance_plan` returns it for every config | `auto_refresh_margin_blocks` = 144 | **nothing on a laddered coin.** Both of its remedies — the cooperative re-anchor, then a sever from `F` — are applied to coins near a `locktime`, and `coin.locktime` is `None` for life. On a laddered wallet it returns `(vec![], vec![])` every tick. It stays scheduled because it stays unconditional (ci-guard `deny_optional_deadline_safety`), not because anything is due |
| `defend_ladders()` | **unconditional**, gated to one pass per new block | — | a hostile trigger; a no-op while `F` is unspent; the **only** defence a laddered coin needs, from first sight |
| `auto_exit_due` | `SdkConfig::auto_exit`, default on | `auto_exit_margin_blocks` | a **legacy subject only**: a coin still carrying `branch-` rows from the retired coloured split/combine lane, force-exited or (for a received carrier) materialized against its deposit-anchored deadline. **The leaf near-deadline loop is deleted** — an adopted child or spine tip has no height deadline, because no ancestor holds a matured spend of `F`; its exposure is the parent's trigger being broadcast, an event `defend_ladders` already answers. On a wallet with no legacy rows it finds nothing |

`auto_exit_margin_blocks` is still **derived, never chosen** —
`auto_exit_margin_blocks_for(k_max, interval, d) = k_max·interval + tesr_exit_txs(d)·144`, **2,120
blocks on mainnet** (14·100 + 5·144) and **860 on regtest** (14·10 + 5·144) — but it is consumed only
by that legacy loop. The `k_max·interval` term was the ancestor-locktime gap of the retired flat
chain and bounds nothing on a laddered coin; the second term is one confirmation window per
SEQUENTIAL transaction of an exit walk, and a single window is not enough for a walk that lands
`3 + 2d` transactions one after another.

A failing pass **fails closed and loud**: any blindness — unreadable tip, unreadable wallet record,
unresolvable carrier set — emits `WalletEvent::WatchtowerBlind`, retains a fault readable through
`watchtower_faults()`, and returns `Err`. It never proceeds on a defaulted-empty carrier set, which
would silently make a loop find nothing to protect and report success.

### Delegation is keyless

Everything a tower must broadcast is already fully signed and pays **only the owner**, whichever
material it is holding.

- *The tier chain*: the persisted `TesrBundle` (`tesr::persist` / `tesr::load`) is every tier, each
  paying the owner's own key. `tesr::watch_pass` runs one iteration from that bundle and an
  electrum connection alone — no wallet, no coin, no SE, no keys.
- *The keyless bundle*: `export_watch_bundle()` walks every live coin (`is_live_for_defence`) and
  emits an entry for every **laddered** one — root, adopted child, spine tip — each **event-driven**:
  `deadline_block: u32::MAX` (the height predicate permanently false), `backup_tx: None`,
  `backup_locktime: None`, and a `WatchTrigger` on `F` whose `push_txs` are the owner's own
  pre-signed tiers. **No entry the exporter can now produce carries a `backup_tx` or a finite
  deadline**: the height-driven arm is deleted, so a legacy `branch-` coin is not exported either —
  it is omitted, and `flat_only_coins` reports only those coins a `claim()` pass recorded a skip for,
  so an un-laddered coin can leave the bundle without appearing in that listing. Check
  `estimate_exit_cost` per coin if you need certainty that a delegate covers it.
  `watchtower::watch_pass` (`clients/libs/rust-sdk/src/watchtower.rs`) is the matching keyless pass.
  Adopted split children and spine tips are read from their own rows, so a *laddered* leaf is never
  silently absent from the bundle.

`sdk45` serializes the bundle a user would hand a third party, asserts it contains no key material
at all, has the keyless tower defend an offline owner against a hostile trigger end to end, and then
runs a **second independent tower** over the same bundle to show the re-broadcast is harmlessly
idempotent. Redundancy is pure upside.

The worst a malicious or buggy tower can do is broadcast **early** — which settles the owner's coins
on chain, to the owner, costing only their off-chain-ness — or not act, which is the same risk as
running no tower. Bundles are **snapshots**: re-export after anything that mints or replaces coins,
including a refresh (`WalletEvent::CoinRefreshed`).

### The limit, stated as a limit

**A keyless tower cannot fee-bump.** It can watch `F` and broadcast the pre-signed tiers at their
committed fee. A CPFP child spending the P2A anchor needs a funding input the tower does not hold and
a signature it cannot make, so if the mempool floor rises above a tier's committed rate the tier is
refused at `sendrawtransaction` and the tower has no move. This is a property of the protocol, not a
gap in the implementation — "delegable, keyless watching" must not be read as implying spike-time
rescue.

Nor does the anyone-can-spend anchor supply a rescuer, and the reason is structural: a child's change
must clear `CHILD_CHANGE_DUST = 330` while the anchor is worth `P2A_VALUE = 240`, so an anchor-only
child can never produce a legal change output at any fee rate.

**Who bumps, then: the owner.** `mercurylib::wallet::p2a_fee_child::build_p2a_fee_child` builds and
prices the owner-funded v3 child — **153 vB**, estimated and measured — and
`mercuryrustlib::core_rpc::submit_package` submits the 1P1C package to a Bitcoin Core node (electrum
has no `submitpackage`). `exit_pass_with_bump` and `watch_pass_with_bump` escalate a tier refused at
its committed fee into a package, and `unilateral_exit` / `defend_ladders` use them **whenever
`SdkConfig::fee_bump` supplies an owner fee source** — it is `None` by default, so fee bumping ships
with no fee source, and it is an explicit argument rather than ambient config, so the plain
`exit_pass` / `watch_pass` keep their exact keyless meaning. A keyless pass reports a fee-stuck tier
as a *stated limit* rather than as one more retryable failure. The anchor is located by matching the
P2A script, never by a guessed vout, because a coloured tier carries an extra `opret`.

Two things remain open here: `watch_child_pass_seen` has **no bump variant**, so a tower defending a
child tier is stuck at that tier's committed rate; and no E2E suite test exercises spike-time
bumping — the two tests that do (`live_p2a_package_rescue.rs`, `live_tower_float.rs`) need a Core RPC
endpoint and skip loudly without one, so a green suite run is not evidence the rescue works.

---

## Extending a coin's life (off-chain) and re-anchoring (on-chain)

Idle coins do not age, so there is nothing to renew on a schedule. What gets consumed is the **hop
budget**, and both refills are off-chain and unbounded. They are **library calls** —
`mercuryrustlib::tesr::renew_auto` / `rollover_auto` for a root, `renew_child` for a leaf,
`renew_colored_ladder` on the SDK for a coloured root — and they are **not yet invoked on the
transfer path**: renewal is by hand today, and a transfer that reaches the floor is refused with the
remedy named rather than renewed in place.

- **Renewal** — when the next state would fall below `D_floor`, two blind co-signs mint a fresh
  extension `X_{m+1}` at a lower CSV plus a fresh state on it. Zero on-chain bytes. Older extensions
  become consensus-dead: the new one strictly undercuts them in the race for `T.out[0]`, so every
  state hanging off an older one can never confirm (`sdk40` PART 2).
- **Rollover** — at `m_max = 15` a 1-in-1-out self-split's child output hosts fresh extension and
  state tiers, i.e. a whole fresh hop budget, for +1 depth level and zero on-chain bytes.
- **Leaf renewal** — a received leaf that has spent its own transfer budget gets it back the same
  way, for zero on-chain bytes and no added depth (`sdk84`, re-derived to assert the leaf conveys
  an empty parent chain, pending run).

`sdk43` drives renew → rollover → renew past the renewal cap (`m_max = 15`) through those library
calls and then
exits unilaterally through the whole deep chain, with `F` untouched throughout: a coin can live
off-chain indefinitely. A coin's off-chain life is bounded by renewals and rollover only; the
on-chain cadence is the cooperative re-anchor at the renewal/rollover cap, and nothing else.

**Refresh is the re-anchor primitive.** `refresh(statechain_id, fee_rate?)` spends the coin's current
outpoint into a **fresh aggregate** in one SE-co-signed on-chain transaction (~112 vB): a new
`statechain_id` at a new funding outpoint, same owner, laddered at first sight of the new funding
transaction through the same deposit path as any other coin. Because the old outpoint is spent, every
exit right rooted at it — every retained trigger copy and every superseded state — is permanently
dead.

What it does and does not reset:

- it does **not** reset a laddered coin's *exit*, which is the CSV chain and never matures while idle;
- it does **not** reset a calendar, because there is none — it is the answer for a coin at its
  renewal/rollover cap, or one whose exit chain has grown deeper than its owner wants to walk;
- it is **cooperative**. If the SE is gone, exit unilaterally instead.

The fee is drawn from the coin (single-input, blind SE), so the user-pays variant yields
`amount − fee`. `refresh_sponsored(statechain_id, sponsor, fee_rate?)` layers an **off-chain**
operator rebate on top, sized `max(fee_sats + DUST_LIMIT, min_child_value)` — the rebate is itself a
non-exact payment out of the sponsor's own laddered coin, so it is minted by an in-ladder split. The
`min_child_value` term (**1,560 sat** at the shipped 3.0 sat/vB) is what makes the rebate a **two-rung**
child that can exit unaided — since REQ-83 admission itself would have taken one satoshi, so this is a
deliberate sizing choice, not a floor the split would have enforced. Sizing it below that leaves the
sponsor's rebate in a band that cannot be put on chain by its owner alone. The
operator absorbs the difference; the user ends ≥ whole. `sdk30` pins both fee models; `sdk38` pins
that a broke sponsor loses boundedly.

Refresh is **refused outright on an RGB carrier** — a plain re-anchor would destroy the allocation.
The coloured re-anchor (`colored_reanchor`) is the carrier's primitive, and it is a manual call.

`auto_refresh_due` — the pre-spend hook `transfer` runs and the routine background pass — selects
coins by `coin.locktime − tip ≤ margin`, and no laddered coin has a `locktime`, so on a laddered
wallet it re-anchors nothing and returns `Ok(vec![])`. The `auto_refresh` and `background_auto_refresh`
flags are inert for the same reason.

---

## Exits with tokens

Both *plain* exit paths refuse a carrier — a plain tier spend and a plain sweep are both RGB-unaware
— so how a carrier settles depends on what it holds.

A carrier whose ladder is **coloured** exits by walking it, arm 1 above: every tier is a valid RGB
state transition, so the walk moves the allocation to the owner's own key (`sdk75`). That is a real
unilateral exit, needing nobody. An idle coloured ladder never ages either (`sdk32`).

A carrier with **no** coloured ladder — one on a network where no enclave attestation identity is
pinned, or one funded below the coloured root floor — has **no exit material at all**, and
`unilateral_exit` says so rather than reporting a walk it did not perform. It is never plain-laddered,
never plain-exited, and it cannot be conveyed; a later pass that can colour it is the only thing that
gives it an exit. The lane such a carrier used to ride — the RGB-aware branch split/combine, with
`materialise_carrier` broadcasting a chain of pre-signed coloured branch transactions to settle the
allocation — is retired: `register_split_subcoins_n` and `register_combine_subcoins` refuse by name,
so `materialise_carrier` only ever finds `branch-` rows on a coin that predates the rule.

A received **coloured child** has no clawback window to beat: its parent is a laddered coin with no
flat backup, so no ancestor holds a matured spend of `F`, and its only exposure is the parent's
trigger being broadcast — an event the per-block `defend_ladders` child loop answers. The
near-deadline materialization `auto_exit_due` used to perform for received carriers (`sdk34`), the
carrier deadline pass (`sdk87`) and the carrier headroom bound (`sdk88`, measured against a deadline
that no longer exists) have no laddered subject; each is retired or re-derived, pending run, and
none is evidence for a laddered claim until the re-derived flow has run. See [tokens](tokens.md).

---

## Which exit costs what

| Path | On-chain weight | Wait | Needs the SE? |
|---|---|---|---|
| Cooperative withdraw | ≈ 111 vB, 1 tx | none | yes |
| Cooperative de-trigger (grief response) | **125 vB**, 1 tx (168 vB coloured) | none | yes |
| Unilateral, laddered root | **375 vB**, 3 txs (+ up to 3 owner-funded P2A children at 153 vB each in a spike) | `E_m + Δ_k`, worst 2,160 blocks ≈ 15 d, shrinking 36 blocks/hop | no |
| Unilateral, depth-*d* child | `293·d + 375` vB over `3 + 2d` txs (mainnet cap: depth 8, 19 txs) | `720·d + 2160` blocks + one confirmation per tx | no |
| Leaf combine after `SP` confirms | 1 tx, N inputs → 1 output | none | yes |
| Re-anchor (`refresh`) | ~112 vB, 1 tx | none | yes |
| Renewal / rollover / leaf renewal | **0 vB** | none | yes (blind co-sign only) |
| Legacy token materialization (`branch-` rows that predate the rule) | branch only, `2d + 1` txs | none — branch txs carry no locktime | no |

## Timelock summary

| Transaction | Shape | Timelock |
|---|---|---|
| `T` — trigger | laddered | **none**; signed once at first sight of `F`, un-broadcast. Broadcasting it starts every clock below and pre-empts every other spend of `F` |
| `X_m` — extension | laddered | **relative** CSV `E_m = E0 − m·δE`, from `T`'s confirmation |
| `S_k` — state | laddered | **relative** CSV `Δ_k = D0 − k·δ`, from `X_m`'s confirmation |
| `SP` — in-ladder split | laddered | a spine tier at `SPINE_CSV = 0`; each child output then hosts its own extension + state |
| spine-tip cap | laddered | one rung over `SP.out[K]` |
| cooperative withdraw / de-trigger | — | none — a fresh co-signed spend |
| exit branch txs (legacy coloured split/combine, retired) | legacy `branch-` rows only | none — immediately broadcastable |

There is no absolute-locktime transaction on any coin. The "deposit backup #1 at `deposit_height +
initlock`" and "each transfer's new backup at `previous − interval`" rows that used to end this table
describe material that is no longer built: `create_tx1` is deleted, and a v2/v4 transfer message
carrying any `backup_transactions` is refused by the receiver (`verify_flat_backup_lane`,
`refuse_conveyed_flat_backups`).

---

## What is not built

- **The sweep at claim** ([SPEC.md](../spec/SPEC.md) §5.3) — replacing a received leaf with an
  ordinary root coin at the moment it is first seen. The **decision** is built and sited where
  REQ-49 puts it: `SdkConfig::sweep_at_claim` exists, `claim()` reads it, and the predicate is
  `mercurylib::sweep::may_absorb` / `should_settle`. What is missing is the **swap itself** — there
  is no absorption path — so the flag ships `false` and turning it on is a hard error by name, which
  is what REQ-49 requires until the cooperative child exit it depends on is demonstrated end to end.
- **The discharge *round* is deleted, and this page used to describe it wrongly.** The claim that
  ran here — "its enforcement point is empty; `disclosure` and `prevout_value` occur 0× in
  `lockbox/`, so the SE would co-sign a collapse that pays out nobody" — is false on both halves.
  The round as a round (the R0–R9 sequence, round eligibility, the operator float) was **removed
  from the design**, not left unbuilt ([SPEC.md](../spec/SPEC.md) §5.4.7; ci-guard
  `deny_round_shaped_mechanisms` keeps its shapes from returning, REQ-81: a close is triggered by a
  root owner's decision, never by a calendar). What replaced it — an owner-triggered **close** — is
  built on both sides: the client has `collapse_obligations` / `collapse_first` / `collapse_grant` /
  `request_collapse`, and the SE enforces REQ-56 in the enclave (`lockbox/include/registry.h`,
  `db_manager.h`'s `freeze_root_and_store_collapse_sig`, the `/collapse_grant` route in
  `lockbox/src/server.cpp`), refusing any `C` that does not pay every unreleased frontier leaf its
  full funding value to its own exit key. `sdk94` drives the accept path end to end and was not
  touched by the flat-backup rule. What a close is **not** is a scheduler: nothing runs one for you.

Two smaller gaps on this page's own subject: renewal and rollover are library calls not yet invoked
on the transfer path (renewal by hand), and the coloured re-anchor is a manual call — so "the
on-chain cadence is the re-anchor at the cap" describes what the primitives allow, not a scheduler
that exists.

That matters here because of what the leaf lane actually costs today. Per payment: **0 vB** if the
piece is spent onward off-chain, **~105 vB** if it is swept and settled (1.47× better than a ~154 vB
on-chain payment — the cap for a leaf that is settled one at a time, i.e. without a close), **418
vB** on the shipped default, and
**250 – 2,719 vB** if it is walked out unilaterally. Walking a depth-1 leaf out is 250 vB, 1.62×
*worse* than doing the payment on chain. The design rule that follows: **a piece received and
immediately cashed out should never have been an off-chain split.**

Read next: [transfers](transfers.md) for how a payment is built, [tokens](tokens.md) for the coloured
lane, [trust-model](trust-model.md) for who has to be awake.
