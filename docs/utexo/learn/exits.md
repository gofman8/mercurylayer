# Deposits and exits

> This page is the guided tour. The normative accounts are [PROTOCOL.md](../spec/PROTOCOL.md) §5.2
> (the tiers), §5.7 (races), §5.8 (the cooperative de-trigger), §5.9 (exit costs) and §5.13
> (watchtowers), and [SPEC.md](../spec/SPEC.md) §4 (deposit) and §9 (exit). The trust boundaries a
> tower does and does not remove are in [TRUST-MODEL.md](../spec/TRUST-MODEL.md) §5.

**Read this first.** A laddered coin has **two clocks, and only one of them is stopped.**

- The **CSV clock is stopped**: `claim()` pre-signs a trigger → extension → state chain of relative
  timelocks and leaves all three **un-broadcast**. BIP-68 locks do not tick until their parent
  confirms, and the trigger has no timelock at all, so **nothing matures until someone broadcasts
  the trigger**. An idle coin never ages on this side: no expiry, 0 vB of idle rent.
- The **calendar clock is not**. The flat backup chain is retained alongside the ladder, and its
  absolute locktimes belong to the coin's **prior owners**. A coin received `k` times sits on
  `min(L_k) = L_0 − k·interval` — a real, finite, approaching height. `sdk86` measures both clocks
  on one coin across three owners.

Everything below is written against that pair. The tour ends with a cost table.

---

## Deposits

`get_deposit_address(amount)` performs the SE handshake (`deposit/init/pod`) and returns a taproot
address whose key is the aggregate of yours and the SE's. Send the exact amount; the SDK's watcher
picks it up.

The order the watcher works in is load-bearing:

1. **First sighting** (the coin flips to `IN_MEMPOOL`): `create_tx1` signs the coin's first flat
   backup at absolute locktime `deposit_height + initlock`. The backup exists from the mempool
   sighting, **not** from confirmation, so a deposit that never confirms still leaves a signed exit.
2. **Later, in a separate pass**: confirmations are counted, the coin flips `UNCONFIRMED` →
   `CONFIRMED`, and `DepositConfirmed` is emitted.
3. **`claim()` then ladders it**, unconditionally and idempotently, emitting `LadderEstablished`. A
   coin that already carries a ladder is skipped, so repeated passes never double-sign, and the exit
   payee is always the coin's own seed-derived `backup_address`.

Off one funding output `F` the ladder pre-signs, but never broadcasts:

```
F   (on-chain, your deposit — the ONLY thing resting on chain)
└─ T   TRIGGER    v3/TRUC + 240-sat P2A. NO timelock. Signed ONCE, at deposit. Never re-signed.
   └─ X_m EXTENSION  relative CSV E_m = E0 − m·δE, counted from T's confirmation.
      │              Renewal replaces it HORIZONTALLY with a lower-CSV X_{m+1}, off-chain.
      └─ S_k STATE   relative CSV Δ_k = D0 − k·δ, counted from X_m's confirmation. Pays owner k.
```

Mainnet schedule, from `TesrParams::mainnet()` (`lib/src/tesr.rs`) verbatim: `D0/δ/D_floor =
1440/36/144`, `E0/δE/E_floor = 720/36/144`, forced rollover at `m_max = 15`,
`committed_fee_rate = 3.0` sat/vB. Testnet and signet run the **same** schedule; only regtest keeps
the small numbers so a full lifecycle mines in seconds. `sdk44` pins the arithmetic; `sdk40` PART 1
pins that real consensus enforces BIP-68 here — an extension is rejected before `E` confirmations of
the trigger, a state before `Δ` of the extension — and that nothing ages while un-broadcast.

**The SE does not publish this schedule.** `/info/config` (`server/src/endpoints/utils.rs`) serves
only `initlock`, `interval`, `batchtimeout` and `version`. The tier schedule is compiled in per
network and a conveyed one is measured against the receiver's own preset field by field
(`cap_schedule`), which is strictly stronger than publication because it holds against a lying
coordinator.

**Two things this pass will not do, both by design and both fail-closed:**

- **give an RGB carrier a *plain* ladder.** A plain tier spend is sats-only, so broadcasting one
  sweeps the sats and destroys the allocation (terminal freeze,
  [PROTOCOL.md](../spec/PROTOCOL.md) §5.10). The carrier is not excluded from laddering for that
  reason any more — it gets a **coloured** ladder instead, every tier carrying its own valid RGB state
  transition. `SdkConfig::colored_ladder` (`clients/libs/rust-sdk/src/config.rs`) selects it by
  **reading the enclave pin**, `TesrParams::attestation_identity_const(network).is_some()`, because a
  coloured ladder whose terminality cannot be verified against a pinned identity is not worth
  building. Regtest is pinned and ships on; mainnet, testnet and signet evaluate false only because
  **no enclave is provisioned there yet**, and pinning one flips it with no other change
  ([SPEC.md](../spec/SPEC.md) §0.4 rows V-1 and V-6). Where a carrier does stay flat,
  `LadderSkipReason` names why;
- **root a trigger on an un-broadcast funding output.** A coin funded by a split output — an
  in-ladder child, a spine tip — has no confirmed prevout for a trigger to spend, and a v3 tier
  cannot relay over an unconfirmed parent. Colouring a tier does not change this and never could:
  it is the same fact that makes idle rent 0 vB. Such a coin is laddered by the split that created
  it, whose chain reaches back to the parent's confirmed `F`, not by this pass.

If the carrier set or `F`'s on-chain status cannot be resolved, the pass is skipped and retried: a
missed ladder is harmless, a plainly-laddered carrier is not. Either way the coin keeps its
signed-once backup and stays exitable. `sdk71` proves the unconditional half on the live stack.

Deposit slots consume a **deposit token** (anti-spam / fee mechanism); when payment is required the
SDK surfaces `SdkError::TokenPaymentRequired` with the details. Slots minted by an SE-co-signed flow
over an existing statechain — a split piece or change, a `transfer_many` recipient, a combine output,
a refresh re-anchor — are **derived slots** and cost nothing (`deposit/get_derived_token`, capped at
64 per parent). `sdk36` covers this.

*Static addresses:* deposit addresses are per-coin. Reuse is detected and handled, but
`get_deposit_address` is cheap — call it per receive.

---

## Cooperative exit — the normal path, one transaction

`withdraw(to_address, statechain_ids?, fee_rate?)`: the SE co-signs a **fresh direct spend** of each
coin's funding output to your L1 address. No timelock, one on-chain transaction per coin, ≈ 111 vB.
The pre-signed ladder is simply abandoned un-broadcast — a cooperative exit never touches it.

Three cases behave differently:

- a **sub-coin that carries a `branch-` row** — one minted by a colored split or combine — is
  materialized first: its funding is un-broadcast, so the SDK broadcasts the branch (branch
  transactions carry no locktime, so this is instant) to give the withdraw spend an on-chain input,
  then the withdraw spend itself;
- a **token carrier** is excluded from the withdraw-everything default and **hard-errors** if named,
  because an RGB-unaware sweep destroys the allocation;
- a received **split child** has no confirmed outpoint to spend at all — its funding `SP.out[j]` is
  un-broadcast — so `withdraw` cannot co-sign a direct spend. It is routed automatically to the
  unilateral exit below and booked `WITHDRAWING`. Nothing is lost: the child's pre-signed chain
  already pays your own key. It just settles over several blocks instead of one.

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
signed at deposit or at claim, and each pays the coin's own seed-derived `backup_address`. There is
nothing left to choose at exit time, which is exactly the property that makes the walk keyless and
delegable. To land the value somewhere else, exit first and spend the result.

It dispatches on where the coin sits in a ladder, and the arms are probed **in this order** — the
spine tip is a position of its own, and omitting it is not a simplification, because a tip that fell
through to the flat fallback would get its branch rows and its absolute-locktime backup broadcast,
which is RGB-unaware:

1. **Root** (`exit_pass`) — walk the tier chain: broadcast `T`, then each extension and state as
   its relative CSV matures. **No absolute-locktime backup is broadcast on this arm.** A carrier
   whose ladder is **coloured** walks here too, and because every tier is a valid RGB transition the
   walk moves the allocation to the owner's own key rather than sweeping it away (`sdk75`).
2. **Split child** (`exit_child_pass`) — the same walk over the full pre-co-signed chain
   `T → X_m → SP → ext_child → state_child`, whose final state already pays this wallet's own key.
   This is also where a cooperative `withdraw` of a child is routed.
3. **Spine tip** (`exit_spine_tip_pass`) — the sender's own change leg from an in-ladder split,
   walking the one-rung cap over `SP.out[K]` via `next_spine_tip_exit_tier`.
4. **Flat fallback** — a coin carrying no ladder of any of the three kinds: a legacy pre-ladder coin,
   or one whose `claim()` never completed. Broadcast the exit branch (instant, locktime-free), then
   the coin's latest pre-signed backup, subject to its **absolute** locktime. Every previous owner's
   backup unlocks *later* than yours, so you have a window in which only you can claim. This is a
   *recovery* arm, not a lane payments are minted into — the plain un-laddered shape that used to
   feed it is deleted — and it repeats the carrier test at the point of no return, refusing rather
   than filtering, because a restored-from-mnemonic coin can arrive here with the wallet unable to
   see that it is holding an asset.

Two refusals are part of the contract. `unilateral_exit` refuses a coin that is not `CONFIRMED` even
when named explicitly — exiting a parent already consumed by a split would kill the transaction
funding the receiver's child — and it refuses a token carrier **unless its ladder is coloured**,
because otherwise every pre-signed spend of `F` this wallet holds is RGB-unaware. That refusal names
the two routes that do exist rather than returning `complete` on a walk it did not perform:
`materialise_carrier` to settle the allocation on chain, `transfer_tokens` to move it onward. A tier
whose timelock is unreached is reported as `ExitStatus{complete: false, wait_blocks > 0}`, never as
an error.

*Evidence:* `sdk50` (the SDK surface, end to end), `sdk40` PART 1 (real consensus rejects each tier
before its CSV is met and accepts it after), `sdk45` (a keyless tower drives the same walk with no
key material).

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
`lib/src/transfer/receiver.rs`): on mainnet the admission rule tops out at depth **8**, 19
transactions to walk; on regtest depth 54, 111 transactions.

`estimate_exit_cost(statechain_id)` reports `{branch_txs, branch_vbytes, backup_vbytes,
total_vbytes, wait_blocks, exit_deadline_block}` — and its scope is honest: it measures the **flat**
material only, the stored branch plus the latest absolute-locktime backup, and it is
what feeds the calendar deadline the watchtower acts on. A laddered coin's tier chain is structural
instead, per the formulas above, and `exit_deadline_block` is `None` for one. That `None` means "no
ancestor can race this coin off-chain", **not** "this coin has no calendar" — the retained flat
chain's locktime is a real deadline and no client surfaces it.

---

## Broadcasting the trigger — what it actually does

`T` is the alarm, and pulling it is irreversible in three ways at once.

- **It starts every clock below it.** Until `T` confirms, no extension and no state anywhere in the
  tree can mature. After it confirms, the CSV walk runs on whoever's schedule.
- **It kills every flat backup rooted at `F`, permanently.** `T` spends `F`; so does every
  absolute-locktime backup any prior owner retains. Once `T` confirms, none of them can ever
  confirm. This is why `T` is also the *defence* of last resort against the calendar clock: it is
  valid immediately, so it beats every retained *timelocked* rung by being valid first. The SDK
  exposes that use under its own name, `sever_from_f`, which is `unilateral_exit` on one coin.
- **It is loud.** No theft transaction can become *valid* until someone publicly spends your funding
  output on chain and then waits at least 144 blocks of CSV. Compare an absolute-locktime chain,
  where a rival's maturity arrives silently on the calendar and the contest afterwards is a
  minutes-scale mempool race.

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
wired as `cosign_colored_detrigger` and reachable only from `colored_reanchor`. It is **not
test-covered**, and neither is the mass-grief prioritization policy.

Griefing is survivable and bounded, but it is **not economically losing for the attacker**: both
transactions pay out of the coin's own committed fees, so at or below the committed rate the griefer
broadcasts something he already holds and pays nothing, while the coin loses two committed fees plus
two anchors. The damage is fee-sized sats out of the coin, never the coin.

**2. Race and win** (needs nobody). `defend_ladders()` runs one watch pass per adopted `tesr-`
bundle and one child pass per adopted `ctesr-` split child. On an untriggered coin it is a no-op —
there is nothing to defend. On a triggered one it broadcasts your tiers as they mature, and because
every transfer co-signs a state strictly below the lowest rival, the current owner's state matures
first. `sdk51` drives exactly this: a prior owner triggers with a stale state, the owner runs only
`defend_ladders()`, and the funds land at the owner's key.

---

## Watching: what runs, and what it cannot do

Each tick, `start_background()` runs `claim()` — which is what ladders a newly seen deposit — then
iterates `maintenance_plan`, then runs two further passes. None of the three defensive passes is
optional in the way a reader might expect:

| pass | gate | margin | what it defends |
|---|---|---|---|
| `deadline_safety_due` | **unconditional** — `maintenance_plan` returns it for every config | `auto_refresh_margin_blocks` = 144 | the whole coin's `min(L_k)`: re-anchor cooperatively first, and **sever from `F`** for whatever the counterparty declines to co-sign |
| `defend_ladders()` | **unconditional**, gated to one pass per new block | — | a hostile trigger; a no-op while `F` is unspent |
| `auto_exit_due` | `SdkConfig::auto_exit`, default on | `auto_exit_margin_blocks` | sub-coins over un-broadcast funding near their deadline — received leaves and spine tips — and token-carrier materialization |

`auto_exit_margin_blocks` is **derived, never chosen**:
`auto_exit_margin_blocks_for(k_max, interval, d) = k_max·interval + tesr_exit_txs(d)·144` — **2,120
blocks on mainnet** (14·100 + 5·144) and **860 on regtest** (14·10 + 5·144). A single confirmation
window is not enough for a walk that lands `3 + 2d` transactions one after another, each of which
must confirm before the next tier's lock even starts counting. The walk's own `Σ csv` is deliberately
*not* folded in: `auto_exit_due` takes that head start per coin, off the coin's own chain, and
subtracts it before comparing.

A failing pass **fails closed and loud**: any blindness — unreadable tip, unreadable wallet record,
unresolvable carrier set — emits `WalletEvent::WatchtowerBlind`, retains a fault readable through
`watchtower_faults()`, and returns `Err`. It never proceeds on a defaulted-empty carrier set, which
would silently make the materialization loop find nothing to protect and report success.

`deadline_safety_due` propagates rather than acting blindly for a specific reason: broadcasting a
plain trigger over a coin it cannot tell is a carrier would destroy that carrier's allocation, which
is worse than the deadline it exists to beat.

### Delegation is keyless

Everything a tower must broadcast is already fully signed and pays **only the owner**, whichever
material it is holding.

- *The tier chain*: the persisted `TesrBundle` (`tesr::persist` / `tesr::load`) is every tier, each
  paying the owner's own key. `tesr::watch_pass` runs one iteration from that bundle and an
  electrum connection alone — no wallet, no coin, no SE, no keys.
- *The flat material*: `export_watch_bundle()` emits branch transactions, deadlines and — **plain
  coins only** — the latest backup transaction. A carrier's entry structurally omits the backup,
  which is what denies an RGB-destroying sweep. `watchtower::watch_pass`
  (`clients/libs/rust-sdk/src/watchtower.rs`) is the matching keyless pass. Adopted split children
  and spine tips are read from their own rows, so a leaf is never silently absent from the bundle.

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
`SdkConfig::fee_bump` supplies an owner fee source** — it is `None` by default, and it is an explicit
argument rather than ambient config, so the plain `exit_pass` / `watch_pass` keep their exact keyless
meaning. A keyless pass reports a fee-stuck tier as a *stated limit* rather than as one more
retryable failure. The anchor is located by matching the P2A script, never by a guessed vout, because
a coloured tier carries an extra `opret`.

Two things remain open here: `watch_child_pass_seen` has **no bump variant**, so a tower defending a
child tier is stuck at that tier's committed rate; and no E2E suite test exercises spike-time
bumping — the two tests that do (`live_p2a_package_rescue.rs`, `live_tower_float.rs`) need a Core RPC
endpoint and skip loudly without one, so a green suite run is not evidence the rescue works.

---

## Extending a coin's life (off-chain) and re-anchoring (on-chain)

Idle coins do not age on the CSV side, so there is nothing to renew on a schedule. What gets consumed
is the **hop budget**, and both refills are off-chain, unbounded, and run unattended inside
`transfer()`:

- **Renewal** — when the next state would fall below `D_floor`, two blind co-signs mint a fresh
  extension `X_{m+1}` at a lower CSV plus a fresh state on it. Zero on-chain bytes. Older extensions
  become consensus-dead: the new one strictly undercuts them in the race for `T.out[0]`, so every
  state hanging off an older one can never confirm (`sdk40` PART 2).
- **Rollover** — at `m_max = 15` a 1-in-1-out self-split's child output hosts fresh extension and
  state tiers, i.e. a whole fresh hop budget, for +1 depth level and zero on-chain bytes.
- **Leaf renewal** — a received leaf that has spent its own transfer budget gets it back the same
  way, for zero on-chain bytes and no added depth (`sdk84`).

`sdk43` drives renew → rollover → renew past epoch exhaustion and then exits unilaterally through the
whole deep chain, with `F` untouched throughout: a coin can live off-chain indefinitely.

**Refresh is the re-anchor primitive.** `refresh(statechain_id, fee_rate?)` spends the coin's current
outpoint into a **fresh aggregate** in one SE-co-signed on-chain transaction (~112 vB): a new
`statechain_id` at a new funding outpoint, same owner, which `claim()` then ladders from scratch.
Because the old outpoint is spent, every exit right rooted at it — every previous owner's backup and
every old tier — is permanently dead.

What it does and does not reset:

- it does **not** reset a laddered coin's *exit*, which is the CSV chain and never matures while idle;
- it **does** reset the coin's **flat calendar**, minting a fresh chain at `tip + initlock` — which
  is exactly why it is the answer for a coin approaching `min(L_k)` or running out of hop budget;
- it is **cooperative**. If the SE is gone, exit unilaterally instead. This is why
  `deadline_safety_due` tries the re-anchor first and severs from `F` when it is refused: the party
  most interested in that deadline passing is the same party being asked to co-sign.

The fee is drawn from the coin (single-input, blind SE), so the user-pays variant yields
`amount − fee`. `refresh_sponsored(statechain_id, sponsor, fee_rate?)` layers an **off-chain**
operator rebate on top, sized `max(fee_sats + DUST_LIMIT, min_child_value)` — the rebate is itself a
non-exact payment out of the sponsor's own laddered coin, so it is minted by an in-ladder split and
must clear that split's admission floor (**1,560 sat** at the shipped 3.0 sat/vB). Sizing it below
that floor makes every sponsored refresh fail *after* the user has already paid the on-chain fee. The
operator absorbs the difference; the user ends ≥ whole. `sdk30` pins both fee models; `sdk38` pins
that a broke sponsor loses boundedly.

Refresh is **refused outright on an RGB carrier** — a plain re-anchor would destroy the allocation.
Materialize the branch instead.

---

## Exits with tokens

Both *plain* exit paths refuse a carrier — a plain tier spend and a plain sweep are both RGB-unaware
— so how a carrier settles depends on what it holds.

A carrier whose ladder is **coloured** exits by walking it, arm 1 above: every tier is a valid RGB
state transition, so the walk moves the allocation to the owner's own key (`sdk75`). That is a real
unilateral exit, needing nobody.

A carrier with **no** coloured ladder — one on a network where no enclave attestation identity is
pinned, or one funded below the coloured root floor — has no SE-free exit at all, and
`unilateral_exit` says so rather than reporting a walk it did not perform. What it has instead is
**materializing** its branch: broadcasting only the chain of pre-signed colored split/combine
transactions, which settles the RGB allocation on chain and defeats an ancestor's clawback, without
the sats-sweeping backup. The sats stay on the live 2-of-2 and still need the SE to move.

`auto_exit_due` does this automatically for a **received** carrier nearing its root deadline, emitting
`TokenCarrierMaterialized` (`sdk34`). An issued or flat carrier has no exit branch — no ancestor, no
clawback risk — and is skipped. `sdk32` is the standing record of the residual clawback window for a
received carrier whose owner never acts, and `sdk87` / `sdk88` cover the carrier deadline and
headroom. See [tokens](tokens.md).

---

## Which exit costs what

| Path | On-chain weight | Wait | Needs the SE? |
|---|---|---|---|
| Cooperative withdraw | ≈ 111 vB, 1 tx | none | yes |
| Cooperative de-trigger (grief response) | **125 vB**, 1 tx (168 vB coloured) | none | yes |
| Unilateral, laddered root | **375 vB**, 3 txs (+ up to 3 owner-funded P2A children at 153 vB each in a spike) | `E_m + Δ_k`, worst 2,160 blocks ≈ 15 d, shrinking 36 blocks/hop | no |
| Unilateral, depth-*d* child | `293·d + 375` vB over `3 + 2d` txs (mainnet cap: depth 8, 19 txs) | `720·d + 2160` blocks + one confirmation per tx | no |
| Leaf combine after `SP` confirms | 1 tx, N inputs → 1 output | none | yes |
| Token materialization | branch only, `2d + 1` txs | none — branch txs carry no locktime | no |
| Re-anchor (`refresh`) | ~112 vB, 1 tx | none | yes |
| Renewal / rollover / leaf renewal | **0 vB** | none | yes (blind co-sign only) |

## Timelock summary

| Transaction | Shape | Timelock |
|---|---|---|
| `T` — trigger | laddered | **none**; signed once at deposit, un-broadcast. Broadcasting it starts every clock below and kills every flat backup over `F` |
| `X_m` — extension | laddered | **relative** CSV `E_m = E0 − m·δE`, from `T`'s confirmation |
| `S_k` — state | laddered | **relative** CSV `Δ_k = D0 − k·δ`, from `X_m`'s confirmation |
| `SP` — in-ladder split | laddered | a spine tier at `SPINE_CSV = 0`; each child output then hosts its own extension + state |
| spine-tip cap | laddered | one rung over `SP.out[K]` |
| cooperative withdraw / de-trigger | either | none — a fresh co-signed spend |
| exit branch txs (coloured split/combine) | flat material | none — immediately broadcastable |
| deposit backup #1 | either | **absolute** `deposit_height + initlock` (mainnet `initlock` = 10,000) |
| each transfer's new backup | either | previous − `interval` (mainnet 100) |

---

## What is not built

Two mechanisms that change the settlement economics are **design, not built**, and neither has
anything plant-and-run:

- **The sweep at claim** ([SPEC.md](../spec/SPEC.md) §5.3) — replacing a received leaf with an
  ordinary root coin at the moment it is first seen. No `sweep_*` parameter and no absorption path
  exists in the tree.
- **The discharge round** ([SPEC.md](../spec/SPEC.md) §5.4) — retiring a whole tree for one
  transaction. Its **enforcement point is empty**: `disclosure` and `prevout_value` occur 83× in the
  client and **0× in `lockbox/`**, so the SE would presently co-sign a collapse that pays out
  nobody.

That matters here because of what the leaf lane actually costs today. Per payment: **0 vB** if the
piece is spent onward off-chain, **~105 vB** if it is swept and settled (1.47× better than a ~154 vB
on-chain payment — and that is the *cap* without the round), **418 vB** on the shipped default, and
**250 – 2,719 vB** if it is walked out unilaterally. Walking a depth-1 leaf out is 250 vB, 1.62×
*worse* than doing the payment on chain. The design rule that follows: **a piece received and
immediately cashed out should never have been an off-chain split.**

Read next: [transfers](transfers.md) for how a payment is built, [tokens](tokens.md) for the coloured
lane, [trust-model](trust-model.md) for who has to be awake.
